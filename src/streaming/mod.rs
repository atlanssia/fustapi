//! Streaming engine types and utilities.
//!
//! Defines `LLMChunk`, `LLMStream`, and `StreamError` — the core types for the
//! streaming pipeline: Provider → Normalize → Forward (SSE).

pub mod sse;
