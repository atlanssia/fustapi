//! Capability abstraction layer.
//!
//! Provides provider-agnostic abstractions for tool calling and image input.

pub mod image;
pub mod tool;
pub mod transform;

// Re-export for convenient use by other modules.
pub use image::{ImageInput, ImageSource};
pub use tool::{ToolCall, ToolDefinition};
pub use transform::RequestTransform;
