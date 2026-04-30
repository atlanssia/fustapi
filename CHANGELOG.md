# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Known Issues
- Anthropic protocol dispatch (`/v1/messages`) returns a mock response — real parsing/serialization code exists but is not wired into the dispatch path

---

## [0.1.0] - 2026-04-29

### Added
- **Core architecture** — async tokio runtime, axum HTTP server, graceful shutdown
- **Provider trait** — unified `Provider` interface with streaming support (`Stream<Item = LLMChunk>`)
- **Local provider adapters** — omlx (port 11434), LM Studio (port 1234), SGLang (port 30000)
- **Cloud provider adapters** — DeepSeek and OpenAI as fallback backends
- **OpenAI protocol** — full request/response parsing for `/v1/chat/completions` and `/v1/models`
- **Anthropic protocol** — request/response parsing for `/v1/messages` (parsing implemented, dispatch returns mock)
- **Tool calling** — native and emulated tool call support with JSON schema injection and argument parsing
- **Multi-modal image input** — capability layer for image passthrough with graceful degradation
- **Router** — model-to-provider priority chains with fallback on failure
- **SQLite control plane** — persistent storage for providers, routes, and metrics with `ArcSwap` for zero-downtime config updates
- **Web UI** — embedded single-page application (vanilla HTML/CSS/JS) with 3 tabs: Providers (CRUD), Routes, Health
- **CLI** — `fustapi serve`, `fustapi config init`, `fustapi providers`
- **Configuration** — TOML-based config at `~/.fustapi/config.toml` (macOS/Linux) or `%APPDATA%\fustapi\config.toml` (Windows)
- **Streaming engine** — zero-copy SSE forwarding, no buffering in streaming path
- **Release profile** — `strip=true`, `opt-level="z"`, `lto=true`, `codegen-units=1` for minimal binary size
- **Makefile** — `build`, `test`, `clippy`, `run`, `install`, `package`, `format`, `lint` targets
- **Documentation** — User Manual, Design Spec, Deployment Guide, config example

### Technical Details
- Rust 2024 edition, Rust 1.85+
- Dependencies: axum 0.8, tokio 1.48, reqwest 0.12, rusqlite 0.37 (bundled SQLite), arc-swap 1.7
- Single binary, no external services required
