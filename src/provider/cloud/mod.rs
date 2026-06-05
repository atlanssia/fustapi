//! Cloud provider adapters (`DeepSeek`, `OpenAI`, GLM).
//!
//! These providers connect to cloud inference APIs as fallback when
//! local providers are unavailable.

pub mod deepseek;
pub mod glm;
pub mod health_prober;
pub mod openai;
pub mod sse_parser;

pub use deepseek::{DeepSeekConfig, DeepSeekProvider};
pub use glm::{GlmConfig, GlmProvider};
pub use openai::{OpenAIConfig, OpenAIProvider};
