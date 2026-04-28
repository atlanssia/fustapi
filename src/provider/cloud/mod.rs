//! Cloud provider adapters (DeepSeek, OpenAI).
//!
//! These providers connect to cloud inference APIs as fallback when
//! local providers are unavailable.

pub mod deepseek;
pub mod openai_cloud;

pub use deepseek::{DeepSeekConfig, DeepSeekProvider};
pub use openai_cloud::{OpenAIConfig, OpenAIProvider};
