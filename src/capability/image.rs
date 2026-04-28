//! Image input types for multi-modal requests.
//!
//! Defines `ImageInput` and `ImageSource` (base64 or URL) for multi-modal input support.

use serde::{Deserialize, Serialize};

/// Default MIME type for images.
fn default_mime() -> String {
    "image/png".to_string()
}

/// Image input for multi-modal requests.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ImageInput {
    /// Base64-encoded image data or URL.
    pub source: ImageSource,
    /// MIME type (e.g., "image/png", "image/jpeg").
    #[serde(default = "default_mime")]
    pub mime_type: String,
}

/// Source of an image input.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ImageSource {
    /// Base64-encoded image data.
    Base64 {
        /// The base64-encoded data (without prefix).
        data: String,
    },
    /// URL pointing to the image.
    Url {
        /// The image URL.
        url: String,
    },
}
