//! Request transform pipeline for capability orchestration.
//!
//! Defines the [`RequestTransform`] trait — a composable pipeline that decouples
//! the router from capability-specific orchestration (e.g. tool emulation).
//!
//! Each transform can modify the system prompt, wrap the response stream, and
//! signal whether passthrough mode must be disabled. The router applies all
//! transforms without knowing their internals.

use crate::capability::tool::{ToolDefinition, ToolEmulationStream, inject_tool_schemas};
use crate::streaming::LLMStream;

// ── Trait ──────────────────────────────────────────────────────────────

/// A transform applied to a chat request before forwarding to a provider.
///
/// Transforms form a pipeline: each one can modify the system prompt,
/// wrap the response stream, or otherwise alter behavior.
pub trait RequestTransform: Send + Sync {
    /// Modify the system prompt before sending to the provider.
    /// Returns the modified prompt, or the original if no change is needed.
    fn transform_prompt(&self, prompt: &str) -> String {
        prompt.to_string()
    }

    /// Wrap the response stream to intercept/transform chunks.
    /// Returns the stream as-is by default.
    fn transform_stream(&self, stream: LLMStream) -> LLMStream {
        stream
    }

    /// Whether this transform requires disabling passthrough mode.
    fn requires_passthrough_disable(&self) -> bool {
        false
    }
}

// ── Tool Emulation Transform ──────────────────────────────────────────

/// Transform that injects tool schemas into the system prompt and wraps
/// the response stream in a [`ToolEmulationStream`] for emulated tool calling.
///
/// This replaces the inline orchestration logic previously in the router.
pub struct ToolEmulationTransform {
    tools: Vec<ToolDefinition>,
}

impl ToolEmulationTransform {
    /// Create a new tool emulation transform with the given tool definitions.
    #[must_use]
    pub fn new(tools: Vec<ToolDefinition>) -> Self {
        Self { tools }
    }
}

impl RequestTransform for ToolEmulationTransform {
    fn transform_prompt(&self, prompt: &str) -> String {
        inject_tool_schemas(prompt, &self.tools)
    }

    fn transform_stream(&self, stream: LLMStream) -> LLMStream {
        let emulated = ToolEmulationStream::new(stream);
        Box::pin(emulated)
    }

    fn requires_passthrough_disable(&self) -> bool {
        true
    }
}

// ── Pipeline ──────────────────────────────────────────────────────────

/// Build a transform from provider capabilities and request context.
///
/// Returns an optional tool-emulation transform based on whether the
/// provider requires tool emulation and the request includes tool definitions.
/// `None` means pure passthrough — no transform needed.
pub fn build_transforms(
    tool_calling: crate::types::ToolCallingSupport,
    tools: Option<Vec<ToolDefinition>>,
) -> Option<ToolEmulationTransform> {
    if tool_calling == crate::types::ToolCallingSupport::Emulated
        && let Some(tool_defs) = tools
        && !tool_defs.is_empty()
    {
        Some(ToolEmulationTransform::new(tool_defs))
    } else {
        None
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming::LLMChunk;
    use tokio_stream::StreamExt;

    // ── ToolEmulationTransform tests ───────────────────────────────────

    #[test]
    fn tool_emulation_transform_injects_schemas_into_prompt() {
        let tools = vec![ToolDefinition {
            name: "get_weather".to_string(),
            description: "Get current weather".to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }];
        let t = ToolEmulationTransform::new(tools);
        let result = t.transform_prompt("You are helpful.");
        assert!(result.contains("You are helpful."));
        assert!(result.contains("get_weather"));
        assert!(result.contains("Get current weather"));
        assert!(result.contains("Schema:"));
    }

    #[test]
    fn tool_emulation_transform_injects_multiple_tools() {
        let tools = vec![
            ToolDefinition {
                name: "get_weather".to_string(),
                description: "Get weather".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            },
            ToolDefinition {
                name: "search".to_string(),
                description: "Search the web".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            },
        ];
        let t = ToolEmulationTransform::new(tools);
        let result = t.transform_prompt("System prompt.");
        assert!(result.contains("get_weather"));
        assert!(result.contains("search"));
    }

    #[test]
    fn tool_emulation_transform_requires_passthrough_disable() {
        let t = ToolEmulationTransform::new(vec![]);
        assert!(t.requires_passthrough_disable());
    }

    #[test]
    fn tool_emulation_transform_with_no_tools_still_transforms() {
        // Even with empty tools, the transform is active (inject_tool_schemas
        // on empty vec returns the prompt unchanged, but the transform still
        // exists and requires passthrough disable).
        let t = ToolEmulationTransform::new(vec![]);
        let result = t.transform_prompt("Hello");
        assert_eq!(result, "Hello"); // inject_tool_schemas on empty is identity
        assert!(t.requires_passthrough_disable());
    }

    #[tokio::test]
    async fn tool_emulation_transform_wraps_stream_with_tool_call_detection() {
        let tools = vec![ToolDefinition {
            name: "test_tool".to_string(),
            description: "A test tool".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let t = ToolEmulationTransform::new(tools);

        // Stream that outputs a JSON tool call
        let chunks = vec![Ok(LLMChunk {
            content: Some("{\"name\":\"test_tool\",\"arguments\":{}}".to_string()),
            done: true,
            ..Default::default()
        })];
        let stream: LLMStream = Box::pin(tokio_stream::iter(chunks));
        let mut result = t.transform_stream(stream);

        let c = result.next().await.unwrap().unwrap();
        // The ToolEmulationStream should detect the tool call
        assert!(c.tool_call.is_some());
        let tc = c.tool_call.unwrap();
        assert_eq!(tc.name, "test_tool");
        assert!(c.done);
    }

    // ── build_transforms tests ─────────────────────────────────────────

    #[test]
    fn build_transforms_returns_none_for_native_tool_calling() {
        let tools = Some(vec![ToolDefinition {
            name: "foo".to_string(),
            description: "bar".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }]);
        let transforms = build_transforms(crate::types::ToolCallingSupport::Native, tools);
        assert!(transforms.is_none());
    }

    #[test]
    fn build_transforms_returns_none_when_no_tools() {
        let transforms = build_transforms(crate::types::ToolCallingSupport::Emulated, None);
        assert!(transforms.is_none());
    }

    #[test]
    fn build_transforms_returns_none_for_empty_tools() {
        let transforms = build_transforms(crate::types::ToolCallingSupport::Emulated, Some(vec![]));
        assert!(transforms.is_none());
    }

    #[test]
    fn build_transforms_returns_tool_emulation_for_emulated_with_tools() {
        let tools = vec![ToolDefinition {
            name: "search".to_string(),
            description: "Search".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let transforms = build_transforms(
            crate::types::ToolCallingSupport::Emulated,
            Some(tools.clone()),
        );
        assert!(transforms.is_some());
        let t = transforms.unwrap();
        // Verify the transform works: inject into a prompt
        let prompt = t.transform_prompt("System");
        assert!(prompt.contains("search"));
        assert!(t.requires_passthrough_disable());
    }

    #[test]
    fn build_transforms_returns_none_for_unsupported() {
        let tools = Some(vec![ToolDefinition {
            name: "foo".to_string(),
            description: "bar".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }]);
        let transforms = build_transforms(crate::types::ToolCallingSupport::Unsupported, tools);
        assert!(transforms.is_none());
    }
}
