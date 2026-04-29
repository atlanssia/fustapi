//! Cloud provider adapters (DeepSeek, OpenAI).
//!
//! These providers connect to cloud inference APIs as fallback when
//! local providers are unavailable.

pub mod deepseek;
pub mod openai;

pub use deepseek::{DeepSeekConfig, DeepSeekProvider};
pub use openai::{OpenAIConfig, OpenAIProvider};
