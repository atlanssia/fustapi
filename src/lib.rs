//! FustAPI — Local-first, high-performance LLM API aggregation gateway.
//!
//! Core modules:
//! - `config` — Configuration loading and validation
//! - `server` — HTTP server setup and routing
//! - `router` — Model-to-provider routing
//! - `protocol` — OpenAI and Anthropic protocol parsing
//! - `capability` — Tool calling and image input abstraction
//! - `provider` — Provider trait and adapter registry
//! - `streaming` — Streaming engine (LLMChunk, SSE)
//! - `web` — Embedded Web UI (control plane)

pub mod capability;
pub mod config;
pub mod protocol;
pub mod provider;
pub mod router;
pub mod server;
pub mod streaming;
pub mod web;
