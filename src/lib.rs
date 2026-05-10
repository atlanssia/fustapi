//! `FustAPI` — Local-first, high-performance LLM API aggregation gateway.
//!
//! Core modules:
//! - `config` — Configuration loading and validation
//! - `server` — HTTP server setup and routing
//! - `router` — Model-to-provider routing
//! - `protocol` — `OpenAI` and Anthropic protocol parsing
//! - `capability` — Tool calling and image input abstraction
//! - `provider` — Provider trait and adapter registry
//! - `streaming` — Streaming engine (`LLMChunk`, SSE)
//! - `web` — Embedded Web UI (control plane)

#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::unused_async)]
#![allow(clippy::match_wildcard_for_single_variants)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::redundant_else)]
#![allow(clippy::format_push_string)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::needless_pass_by_value)]

pub mod capability;
pub mod config;
pub mod metrics;
pub mod protocol;
pub mod provider;
pub mod router;
pub mod server;
pub mod streaming;
pub mod web;
