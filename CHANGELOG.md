# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [1.2.1] - 2026-05-06

### Added
- **Proxy Compatibility** — Added support for `/v1/v1/` endpoint pathing to ensure seamless integration with client SDKs (e.g., Claude Code) that may append redundant path segments.
- **Anthropic Model List** — Implemented Anthropic-compatible `/v1/models` response format when the `anthropic-version` header is present.

### Fixed
- **Anthropic SSE Compliance** — Fixed malformed Server-Sent Events (SSE) format by removing redundant newlines between `event:` and `data:` fields, ensuring strict compliance with the SSE specification.

## [1.1.0] - 2026-05-03

### Added
- **Observability Dashboard** — Implemented a high-performance, non-blocking metrics pipeline for real-time monitoring of QPS, latency, and success rates.
- **Lock-Free Telemetry** — Used `ArcSwap` and atomic counters to ensure the dashboard has zero impact on the request hot path.
- **Real-time Canvas Charts** — Added native 2D canvas-based charting to the Web UI for high-performance time-series visualization.

### Changed
- **UI/UX Optimization** — Enhanced the Control Plane with better contrast, responsive grid layouts, and context-aware "Empty States" with guided CTA buttons for new users.
- **Protocol Optimization** — Cleaned up protocol streaming logic for better throughput and lower allocation overhead.

## [1.0.9] - 2026-05-03

### Changed
- **Database Seeding** — Removed the automatic database seeding logic. FustAPI will no longer insert default providers when the database is empty, giving users full control over their configuration from a clean slate.

## [1.0.8] - 2026-05-03

### Fixed
- **Database Seeding** — Fixed a bug where default providers would reappear after being manually deleted. Added a `fustapi_settings` table to track initialization state.

## [1.0.7] - 2026-05-02

### Fixed
- **Web UI Provider Type** — Fixed a bug where provider types (e.g., omlx, lmstudio) added via the Web UI were incorrectly saved as `openai` due to a missing JSON field mapping in the backend.

## [1.0.6] - 2026-05-01

### Changed
- **Default Port** — Changed default server port to `8800`.
- **UI Accessibility** — Removed external Google Fonts dependencies to support offline environments and improve loading performance in restricted regions.

## [1.0.5] - 2026-05-01

### Added
- **Version Flag** — Added `--version` to the CLI for easy version checking.

### Fixed
- **Database Initialization** — Automatically create the parent directory for the SQLite database if it doesn't exist, preventing "unable to open database file" errors on fresh installs.
- **Compiler Warnings** — Cleaned up unused imports and unnecessary mutability in `src/protocol/mod.rs`.

## [1.0.4] - 2026-05-01

### Added
- **Security-First Installer** — Deterministic `install.sh` with strict target mapping, SHA256 verification, and manifest validation.
- **Anthropic Protocol Parity** — Full support for `/v1/messages` including streaming events (`message_start`, `content_block_start`, etc.) and native tool-use translation.
- **One-Click Install** — Simplified installation via `curl | sh` documented in README and Deployment Guide.

### Changed
- **Tool Emulation** — Fixed tool call interception in streaming paths to correctly collapse text chunks into structured tool calls.
- **SSE Streaming** — Reordered event serialization to ensure tool calls are emitted before the stop signal.
- **Capability Layer** — Hardened image input handling and protocol-specific response formatting.

### Fixed
- **Anthropic Dispatch** — Wired real parsing and serialization into the `/v1/messages` path, replacing the previous mock implementation.
- **Streaming Reliability** — Resolved `StreamExt` trait ambiguities and ensured proper cleanup of SSE streams.

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
