//! Protocol dispatch layer.
//!
//! Routes incoming requests to the appropriate protocol parser (OpenAI or Anthropic).

pub mod openai;
pub mod anthropic;
