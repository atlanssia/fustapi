//! Cloud provider adapters (OpenAI-compatible).
//!
//! All providers use `OpenAIProvider` with `BalanceStrategy` config to
//! select provider-specific balance query logic.

pub mod health_prober;
pub mod openai;
pub mod sse_parser;

pub use openai::{BalanceStrategy, OpenAIConfig, OpenAIProvider};
