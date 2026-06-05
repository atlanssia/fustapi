//! Capability pipeline for request/response orchestration.
//!
//! [`CapabilityPipeline`] encapsulates the transform pipeline that decouples the
//! router from capability-specific orchestration (e.g. tool emulation). The router
//! builds a pipeline from provider capabilities and request context, then applies
//! it to both the outgoing request and the incoming stream.

use crate::capability::tool::ToolDefinition;
use crate::capability::transform::RequestTransform;
use crate::streaming::LLMStream;
use crate::types::ToolCallingSupport;

// ── Pipeline ──────────────────────────────────────────────────────────

/// Orchestrates capability transforms for a single request/response cycle.
///
/// Built from provider capabilities and request context, the pipeline:
/// - Applies prompt transforms (e.g. injects tool schemas into system prompt)
/// - Strips consumed tools from the request
/// - Wraps the response stream with capability-specific interceptors
/// - Signals whether passthrough streaming must be disabled
pub struct CapabilityPipeline {
    transforms: Vec<Box<dyn RequestTransform>>,
}

impl CapabilityPipeline {
    /// Build a pipeline from provider capabilities and request tools.
    ///
    /// Returns a pipeline with the appropriate transforms based on whether
    /// the provider requires tool emulation and the request includes tools.
    pub fn build(tool_calling: ToolCallingSupport, tools: Option<Vec<ToolDefinition>>) -> Self {
        Self {
            transforms: crate::capability::transform::build_transforms(tool_calling, tools),
        }
    }

    /// Whether any transform in the pipeline requires disabling passthrough.
    pub fn should_disable_passthrough(&self) -> bool {
        crate::capability::transform::should_disable_passthrough(&self.transforms)
    }

    /// Apply all transforms to the request messages and strip consumed tools.
    ///
    /// For each transform:
    /// 1. Finds (or creates) the system message
    /// 2. Applies the transform to the system prompt
    /// 3. If transforms were applied, strips tools from the request
    pub fn apply_to_request(&self, request: &mut crate::provider::UnifiedRequest) {
        use crate::provider::Role;

        for t in &self.transforms {
            let system_idx = request.messages.iter().position(|m| m.role == Role::System);

            if let Some(idx) = system_idx {
                request.messages[idx].content = t.transform_prompt(&request.messages[idx].content);
            } else {
                let prompt = t.transform_prompt("You are a helpful AI assistant.");
                if prompt != "You are a helpful AI assistant." {
                    request.messages.insert(
                        0,
                        crate::provider::Message {
                            role: Role::System,
                            content: prompt,
                            images: None,
                            tool_calls: None,
                            tool_call_id: None,
                            extras: None,
                        },
                    );
                }
            }
        }

        // Remove tools from request if transforms consumed them
        if !self.transforms.is_empty() {
            request.tools = None;
        }
    }

    /// Apply all stream transforms to the response stream.
    ///
    /// Each transform may wrap the stream (e.g. ToolEmulationStream wraps
    /// to detect and parse emulated tool calls).
    pub fn apply_to_stream(&self, stream: LLMStream) -> LLMStream {
        crate::capability::transform::apply_stream_transforms(stream, &self.transforms)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming::LLMChunk;
    use tokio_stream::StreamExt;

    fn sample_tools() -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            name: "get_weather".to_string(),
            description: "Get weather".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }]
    }

    fn make_request(tools: Option<Vec<ToolDefinition>>) -> crate::provider::UnifiedRequest {
        crate::provider::UnifiedRequest {
            model: "test-model".to_string(),
            messages: vec![crate::provider::Message {
                role: crate::provider::Role::System,
                content: "You are helpful.".to_string(),
                images: None,
                tool_calls: None,
                tool_call_id: None,
                extras: None,
            }],
            tools,
            stream: true,
            temperature: None,
            max_tokens: None,
            extra_params: serde_json::Map::new(),
        }
    }

    // -- build --

    #[test]
    fn build_returns_empty_pipeline_for_native_tool_calling() {
        let pipeline = CapabilityPipeline::build(ToolCallingSupport::Native, Some(sample_tools()));
        assert!(!pipeline.should_disable_passthrough());
    }

    #[test]
    fn build_returns_tool_emulation_pipeline_for_emulated_with_tools() {
        let pipeline =
            CapabilityPipeline::build(ToolCallingSupport::Emulated, Some(sample_tools()));
        assert!(pipeline.should_disable_passthrough());
    }

    #[test]
    fn build_returns_empty_pipeline_for_emulated_without_tools() {
        let pipeline = CapabilityPipeline::build(ToolCallingSupport::Emulated, None);
        assert!(!pipeline.should_disable_passthrough());
    }

    // -- should_disable_passthrough --

    #[test]
    fn empty_pipeline_does_not_disable_passthrough() {
        let pipeline = CapabilityPipeline::build(ToolCallingSupport::Native, None);
        assert!(!pipeline.should_disable_passthrough());
    }

    // -- apply_to_request: prompt injection --

    #[test]
    fn apply_to_request_injects_tool_schemas_into_existing_system_prompt() {
        let pipeline =
            CapabilityPipeline::build(ToolCallingSupport::Emulated, Some(sample_tools()));
        let mut req = make_request(Some(sample_tools()));
        pipeline.apply_to_request(&mut req);

        let system_msg = req
            .messages
            .iter()
            .find(|m| m.role == crate::provider::Role::System)
            .unwrap();
        assert!(system_msg.content.contains("get_weather"));
        assert!(system_msg.content.contains("You are helpful."));
    }

    #[test]
    fn apply_to_request_creates_system_prompt_if_missing() {
        let pipeline =
            CapabilityPipeline::build(ToolCallingSupport::Emulated, Some(sample_tools()));
        let mut req = make_request(Some(sample_tools()));
        req.messages = vec![crate::provider::Message {
            role: crate::provider::Role::User,
            content: "Hello".to_string(),
            images: None,
            tool_calls: None,
            tool_call_id: None,
            extras: None,
        }];
        pipeline.apply_to_request(&mut req);

        assert_eq!(req.messages[0].role, crate::provider::Role::System);
        assert!(req.messages[0].content.contains("get_weather"));
    }

    #[test]
    fn apply_to_request_strips_tools_when_transforms_active() {
        let pipeline =
            CapabilityPipeline::build(ToolCallingSupport::Emulated, Some(sample_tools()));
        let mut req = make_request(Some(sample_tools()));
        pipeline.apply_to_request(&mut req);
        assert!(req.tools.is_none());
    }

    #[test]
    fn apply_to_request_preserves_tools_when_no_transforms() {
        let pipeline = CapabilityPipeline::build(ToolCallingSupport::Native, Some(sample_tools()));
        let mut req = make_request(Some(sample_tools()));
        pipeline.apply_to_request(&mut req);
        assert!(req.tools.is_some());
        assert_eq!(req.tools.unwrap().len(), 1);
    }

    #[test]
    fn apply_to_request_leaves_prompt_unchanged_for_native() {
        let pipeline = CapabilityPipeline::build(ToolCallingSupport::Native, Some(sample_tools()));
        let mut req = make_request(Some(sample_tools()));
        pipeline.apply_to_request(&mut req);

        let system_msg = req
            .messages
            .iter()
            .find(|m| m.role == crate::provider::Role::System)
            .unwrap();
        assert_eq!(system_msg.content, "You are helpful.");
    }

    // -- apply_to_stream --

    #[tokio::test]
    async fn apply_to_stream_is_identity_for_empty_pipeline() {
        let pipeline = CapabilityPipeline::build(ToolCallingSupport::Native, None);
        let chunks = vec![Ok(LLMChunk {
            content: Some("data".to_string()),
            done: true,
            ..Default::default()
        })];
        let stream: LLMStream = Box::pin(tokio_stream::iter(chunks));
        let mut result = pipeline.apply_to_stream(stream);

        let c = result.next().await.unwrap().unwrap();
        assert_eq!(c.content.as_deref(), Some("data"));
    }

    #[tokio::test]
    async fn apply_to_stream_wraps_with_tool_emulation() {
        let pipeline =
            CapabilityPipeline::build(ToolCallingSupport::Emulated, Some(sample_tools()));
        let chunks = vec![Ok(LLMChunk {
            content: Some("{\"name\":\"get_weather\",\"arguments\":{}}".to_string()),
            done: true,
            ..Default::default()
        })];
        let stream: LLMStream = Box::pin(tokio_stream::iter(chunks));
        let mut result = pipeline.apply_to_stream(stream);

        let c = result.next().await.unwrap().unwrap();
        assert!(c.tool_call.is_some());
        assert_eq!(c.tool_call.unwrap().name, "get_weather");
    }
}
