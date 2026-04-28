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

pub mod config;
pub mod server;
pub mod router;
pub mod protocol;
pub mod capability;
pub mod provider;
pub mod streaming;
pub mod web;
