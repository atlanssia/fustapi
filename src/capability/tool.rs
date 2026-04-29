//! Tool calling types and abstractions.
//!
//! Defines `ToolCall`, `ToolDefinition`, and `ToolMode` (native vs. emulated)
//! for provider-agnostic tool calling support.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A completed tool call from the LLM.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolCall {
    /// The name of the tool to call.
    pub name: String,
    /// JSON-encoded arguments for the tool.
    pub arguments: Value,
}

/// A tool definition provided to the LLM for discovery.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ToolDefinition {
    /// The name of the tool.
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON Schema describing the tool's parameters.
    pub parameters: Value,
}

/// Tool calling mode for a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolMode {
    /// Provider natively supports tool calling (passes tool definitions directly).
    Native,
    /// Gateway emulates tool calling (parses LLM output into tool calls).
    Emulated,
}

/// Parse error for tool call extraction.
#[derive(Debug)]
pub enum ParseError {
    /// The text contains invalid JSON.
    InvalidJson(serde_json::Error),
    /// A required field is missing from the parsed JSON.
    MissingField(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::InvalidJson(e) => write!(f, "invalid JSON: {e}"),
            ParseError::MissingField(field) => write!(f, "missing field: {field}"),
        }
    }
}

impl std::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ParseError::InvalidJson(e) => Some(e),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
struct ParsedToolCall {
    name: String,
    #[serde(default)]
    arguments: Value,
    #[serde(default)]
    input: Value,
}

/// Parse a tool call from LLM text output.
///
/// Tries to extract a JSON object with `name` and `arguments` (or `input`) fields from the text.
/// Supports both raw JSON and Anthropic-style `<tool_use>` tags.
///
/// # Returns
/// - `Ok(Some(ToolCall))` if a valid tool call was found
/// - `Ok(None)` if no tool call was found in the text
/// - `Err(ParseError)` if the JSON was invalid
pub fn parse_tool_call_from_text(text: &str) -> Result<Option<ToolCall>, ParseError> {
    // Try Anthropic-style <tool_use> tags first.
    if let Some(start) = text.find("<tool_use>")
        && let Some(end) = text.rfind("</tool_use>")
    {
        let inner = &text[start + "<tool_use>".len()..end];
        return parse_json_tool_call(inner);
    }
    // Try raw JSON.
    parse_json_tool_call(text)
}

fn parse_json_tool_call(json_str: &str) -> Result<Option<ToolCall>, ParseError> {
    // Find the first '{' to locate JSON object.
    let start = json_str.find('{').unwrap_or(0);
    let end = json_str.rfind('}').map(|i| i + 1).unwrap_or(json_str.len());
    let trimmed = json_str[start..end].trim();
    if trimmed.is_empty() || !trimmed.starts_with('{') {
        return Ok(None);
    }
    let parsed: ParsedToolCall = serde_json::from_str(trimmed).map_err(ParseError::InvalidJson)?;
    if parsed.name.is_empty() {
        return Err(ParseError::MissingField("name".to_string()));
    }
    // Use 'input' if 'arguments' is not present (Anthropic format).
    let arguments = if parsed.arguments.is_null() && !parsed.input.is_null() {
        parsed.input
    } else {
        parsed.arguments
    };
    Ok(Some(ToolCall {
        name: parsed.name,
        arguments,
    }))
}

/// Inject tool schemas into a system prompt for emulated tool calling.
///
/// Formats each tool definition as a JSON schema and appends it to the system prompt
/// in a format that LLMs can understand for emulated tool calling.
///
/// # Arguments
/// * `system_prompt` — The base system prompt
/// * `tools` — List of tool definitions to inject
///
/// # Returns
/// The enhanced system prompt with tool schemas appended
pub fn inject_tool_schemas(system_prompt: &str, tools: &[ToolDefinition]) -> String {
    if tools.is_empty() {
        return system_prompt.to_string();
    }
    let mut enhanced = system_prompt.to_string();
    enhanced.push_str("\n\nYou have access to the following tools:\n\n");
    for tool in tools {
        enhanced.push_str(&format!("- **{}**: {}\n", tool.name, tool.description));
        enhanced.push_str(&format!(
            "  Schema: {}\n\n",
            serde_json::to_string(&tool.parameters).unwrap_or_default()
        ));
    }
    enhanced.push_str("When you need to use a tool, respond with a JSON object containing 'name' and 'arguments' fields.\n");
    enhanced
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_tool_call() {
        let text = r#"{"name":"get_weather","arguments":{"city":"nyc"}}"#;
        let result = parse_tool_call_from_text(text).expect("parse should succeed");
        assert!(result.is_some());
        let tc = result.unwrap();
        assert_eq!(tc.name, "get_weather");
        assert_eq!(tc.arguments["city"], "nyc");
    }

    #[test]
    fn test_parse_nested_arguments() {
        let text = r#"{"name":"search","arguments":{"query":"rust async","page":1}}"#;
        let result = parse_tool_call_from_text(text).expect("parse should succeed");
        assert!(result.is_some());
        let tc = result.unwrap();
        assert_eq!(tc.name, "search");
        assert_eq!(tc.arguments["query"], "rust async");
        assert_eq!(tc.arguments["page"], 1);
    }

    #[test]
    fn test_parse_no_tool_call() {
        let text = "Hello, how can I help?";
        let result = parse_tool_call_from_text(text).expect("parse should succeed");
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_invalid_json() {
        let text = "{invalid json}";
        let result = parse_tool_call_from_text(text);
        assert!(result.is_err());
        match result.unwrap_err() {
            ParseError::InvalidJson(_) => {}
            other => panic!("expected InvalidJson, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_anthropic_tool_use() {
        let text = r#"<tool_use>{"name":"get_weather","input":{"city":"nyc"}}</tool_use>"#;
        let result = parse_tool_call_from_text(text).expect("parse should succeed");
        assert!(result.is_some());
        let tc = result.unwrap();
        assert_eq!(tc.name, "get_weather");
        assert_eq!(tc.arguments["city"], "nyc");
    }

    #[test]
    fn test_inject_single_tool() {
        let prompt = "You are a helpful assistant.";
        let tools = vec![ToolDefinition {
            name: "get_weather".to_string(),
            description: "Get weather info".to_string(),
            parameters: serde_json::json!({"type":"object","properties":{}}),
        }];
        let enhanced = inject_tool_schemas(prompt, &tools);
        assert!(enhanced.contains("get_weather"));
        assert!(enhanced.contains("Get weather info"));
        assert!(enhanced.contains("Schema:"));
    }

    #[test]
    fn test_inject_multiple_tools() {
        let prompt = "You are helpful.";
        let tools = vec![
            ToolDefinition {
                name: "get_weather".to_string(),
                description: "Get weather".to_string(),
                parameters: serde_json::json!({"type":"object"}),
            },
            ToolDefinition {
                name: "search".to_string(),
                description: "Search web".to_string(),
                parameters: serde_json::json!({"type":"object"}),
            },
        ];
        let enhanced = inject_tool_schemas(prompt, &tools);
        assert!(enhanced.contains("get_weather"));
        assert!(enhanced.contains("search"));
    }

    #[test]
    fn test_inject_no_tools() {
        let prompt = "You are helpful.";
        let enhanced = inject_tool_schemas(prompt, &[]);
        assert_eq!(enhanced, prompt);
    }
}
