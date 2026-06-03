//! Request transform pipeline for capability orchestration.
//!
//! Defines the [`RequestTransform`] trait — a composable pipeline that decouples
//! the router from capability-specific orchestration (e.g. tool emulation).
//!
//! Each transform can modify the system prompt, wrap the response stream, and
//! signal whether passthrough mode must be disabled. The router applies all
//! transforms without knowing their internals.

use crate::capability::tool::{inject_tool_schemas, ToolDefinition, ToolEmulationStream};
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

// ── Identity Transform ────────────────────────────────────────────────

/// A no-op transform that passes everything through unchanged.
pub struct IdentityTransform;

impl RequestTransform for IdentityTransform {
    fn transform_prompt(&self, prompt: &str) -> String {
        prompt.to_string()
    }

    fn transform_stream(&self, stream: LLMStream) -> LLMStream {
        stream
    }

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

/// Build transforms from provider capabilities and request context.
///
/// Returns a list of transforms to apply based on whether the provider
/// requires tool emulation and whether the request includes tool definitions.
pub fn build_transforms(
    tool_calling: crate::types::ToolCallingSupport,
    tools: Option<Vec<ToolDefinition>>,
) -> Vec<Box<dyn RequestTransform>> {
    let mut transforms: Vec<Box<dyn RequestTransform>> = Vec::new();

    if tool_calling == crate::types::ToolCallingSupport::Emulated {
        if let Some(tool_defs) = tools {
            if !tool_defs.is_empty() {
                transforms.push(Box::new(ToolEmulationTransform::new(tool_defs)));
            }
        }
    }

    transforms
}

/// Apply a pipeline of transforms to a prompt, returning the modified prompt.
pub fn apply_prompt_transforms(prompt: &str, transforms: &[Box<dyn RequestTransform>]) -> String {
    let mut result = prompt.to_string();
    for t in transforms {
        result = t.transform_prompt(&result);
    }
    result
}

/// Apply a pipeline of transforms to a stream in forward order.
pub fn apply_stream_transforms(
    stream: LLMStream,
    transforms: &[Box<dyn RequestTransform>],
) -> LLMStream {
    let mut result = stream;
    for t in transforms {
        result = t.transform_stream(result);
    }
    result
}

/// Check whether any transform in the pipeline requires disabling passthrough.
pub fn should_disable_passthrough(transforms: &[Box<dyn RequestTransform>]) -> bool {
    transforms.iter().any(|t| t.requires_passthrough_disable())
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming::LLMChunk;
    use tokio_stream::StreamExt;

    // ── IdentityTransform tests ────────────────────────────────────────

    #[test]
    fn identity_transform_preserves_prompt() {
        let t = IdentityTransform;
        let prompt = "You are a helpful assistant.";
        assert_eq!(t.transform_prompt(prompt), prompt);
    }

    #[tokio::test]
    async fn identity_transform_preserves_stream() {
        let t = IdentityTransform;
        let chunks = vec![
            Ok(LLMChunk {
                content: Some("hello".to_string()),
                ..Default::default()
            }),
            Ok(LLMChunk {
                content: Some(" world".to_string()),
                done: true,
                ..Default::default()
            }),
        ];
        let stream: LLMStream = Box::pin(tokio_stream::iter(chunks));
        let mut result = t.transform_stream(stream);

        let c1 = result.next().await.unwrap().unwrap();
        assert_eq!(c1.content.as_deref(), Some("hello"));

        let c2 = result.next().await.unwrap().unwrap();
        assert_eq!(c2.content.as_deref(), Some(" world"));

        assert!(result.next().await.is_none());
    }

    #[test]
    fn identity_transform_does_not_require_passthrough_disable() {
        let t = IdentityTransform;
        assert!(!t.requires_passthrough_disable());
    }

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
        let chunks = vec![
            Ok(LLMChunk {
                content: Some("{\"name\":\"test_tool\",\"arguments\":{}}".to_string()),
                done: true,
                ..Default::default()
            }),
        ];
        let stream: LLMStream = Box::pin(tokio_stream::iter(chunks));
        let mut result = t.transform_stream(stream);

        let c = result.next().await.unwrap().unwrap();
        // The ToolEmulationStream should detect the tool call
        assert!(c.tool_call.is_some());
        let tc = c.tool_call.unwrap();
        assert_eq!(tc.name, "test_tool");
        assert!(c.done);
    }

    // ── Pipeline tests ─────────────────────────────────────────────────

    #[test]
    fn apply_prompt_transforms_with_empty_pipeline_is_identity() {
        let transforms: Vec<Box<dyn RequestTransform>> = vec![];
        let result = apply_prompt_transforms("original", &transforms);
        assert_eq!(result, "original");
    }

    #[test]
    fn apply_prompt_transforms_with_single_transform() {
        let tools = vec![ToolDefinition {
            name: "calculator".to_string(),
            description: "Do math".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let transforms: Vec<Box<dyn RequestTransform>> =
            vec![Box::new(ToolEmulationTransform::new(tools))];
        let result = apply_prompt_transforms("You are helpful.", &transforms);
        assert!(result.contains("calculator"));
        assert!(result.contains("You are helpful."));
    }

    #[test]
    fn should_disable_passthrough_with_empty_pipeline() {
        let transforms: Vec<Box<dyn RequestTransform>> = vec![];
        assert!(!should_disable_passthrough(&transforms));
    }

    #[test]
    fn should_disable_passthrough_with_tool_emulation() {
        let transforms: Vec<Box<dyn RequestTransform>> = vec![Box::new(
            ToolEmulationTransform::new(vec![ToolDefinition {
                name: "foo".to_string(),
                description: "bar".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }]),
        )];
        assert!(should_disable_passthrough(&transforms));
    }

    #[test]
    fn should_disable_passthrough_with_identity_only() {
        let transforms: Vec<Box<dyn RequestTransform>> = vec![Box::new(IdentityTransform)];
        assert!(!should_disable_passthrough(&transforms));
    }

    #[tokio::test]
    async fn apply_stream_transforms_with_empty_pipeline_is_identity() {
        let transforms: Vec<Box<dyn RequestTransform>> = vec![];
        let chunks = vec![Ok(LLMChunk {
            content: Some("data".to_string()),
            done: true,
            ..Default::default()
        })];
        let stream: LLMStream = Box::pin(tokio_stream::iter(chunks));
        let mut result = apply_stream_transforms(stream, &transforms);

        let c = result.next().await.unwrap().unwrap();
        assert_eq!(c.content.as_deref(), Some("data"));
    }

    #[tokio::test]
    async fn apply_stream_transforms_with_tool_emulation() {
        let tools = vec![ToolDefinition {
            name: "my_tool".to_string(),
            description: "does something".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let transforms: Vec<Box<dyn RequestTransform>> =
            vec![Box::new(ToolEmulationTransform::new(tools))];

        let chunks = vec![Ok(LLMChunk {
            content: Some("{\"name\":\"my_tool\",\"arguments\":{}}".to_string()),
            done: true,
            ..Default::default()
        })];
        let stream: LLMStream = Box::pin(tokio_stream::iter(chunks));
        let mut result = apply_stream_transforms(stream, &transforms);

        let c = result.next().await.unwrap().unwrap();
        assert!(c.tool_call.is_some());
        assert_eq!(c.tool_call.unwrap().name, "my_tool");
    }

    // ── build_transforms tests ─────────────────────────────────────────

    #[test]
    fn build_transforms_returns_empty_for_native_tool_calling() {
        let tools = Some(vec![ToolDefinition {
            name: "foo".to_string(),
            description: "bar".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }]);
        let transforms = build_transforms(crate::types::ToolCallingSupport::Native, tools);
        assert!(transforms.is_empty());
    }

    #[test]
    fn build_transforms_returns_empty_when_no_tools() {
        let transforms =
            build_transforms(crate::types::ToolCallingSupport::Emulated, None);
        assert!(transforms.is_empty());
    }

    #[test]
    fn build_transforms_returns_empty_for_empty_tools() {
        let transforms = build_transforms(
            crate::types::ToolCallingSupport::Emulated,
            Some(vec![]),
        );
        assert!(transforms.is_empty());
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
        assert_eq!(transforms.len(), 1);
        // Verify the transform works: inject into a prompt
        let prompt = transforms[0].transform_prompt("System");
        assert!(prompt.contains("search"));
        assert!(transforms[0].requires_passthrough_disable());
    }

    #[test]
    fn build_transforms_returns_empty_for_unsupported() {
        let tools = Some(vec![ToolDefinition {
            name: "foo".to_string(),
            description: "bar".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }]);
        let transforms = build_transforms(
            crate::types::ToolCallingSupport::Unsupported,
            tools,
        );
        assert!(transforms.is_empty());
    }
}
