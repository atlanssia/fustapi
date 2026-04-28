//! Capability abstraction layer.
//!
//! Provides provider-agnostic abstractions for tool calling and image input.

pub mod image;
pub mod tool;

// Re-export for convenient use by other modules.
pub use image::ImageInput;
pub use tool::{ToolCall, ToolDefinition, ToolMode};
