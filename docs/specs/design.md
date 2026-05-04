# FustAPI v1 Design Specification

> **Version:** 1.0
> **Date:** 2026-04-30
> **Status:** Stable

---

## 1. Overview

### 1.1 What is FustAPI

FustAPI is a **local-first, high-performance LLM API aggregation gateway** written in Rust. It provides a single entry point for AI IDEs and applications to interact with multiple LLM backends through OpenAI-compatible and Anthropic-compatible APIs.

### 1.2 What FustAPI Is Not

- ❌ A general-purpose cloud gateway
- ❌ A heavy scheduling or orchestration system
- ❌ A model training or fine-tuning platform

### 1.3 Core Value Proposition

| Aspect           | Description                                          |
| ---------------- | ---------------------------------------------------- |
| Local-first      | Prioritizes local inference providers over cloud      |
| Multi-protocol   | Supports both OpenAI and Anthropic API formats        |
| Multi-provider   | Aggregates local (omlx, LM Studio, SGLang) and cloud |
| AI IDE ready     | Compatible with Claude Code, OpenCode, etc.           |
| Streaming-native | All paths support streaming; no full-buffer fallback  |
| Single binary    | Rust binary with embedded Web UI                     |

---

## 2. Design Principles

### 2.1 Local-first

Local inference providers are the primary backends. Cloud providers serve as fallback only.

**Priority order:**

1. omlx (high-performance local inference)
2. LM Studio (OpenAI-compatible local)
3. SGLang (high-performance local streaming)
4. Cloud providers (DeepSeek, OpenAI — fallback)

### 2.2 Streaming-first

- Every request path **must** support streaming
- Full response buffering is **forbidden** unless explicitly required by the protocol
- The streaming pipeline must never cache, concatenate, or re-parse chunks

### 2.3 Zero-overhead Abstraction

- Architectural abstractions (protocol layer, capability layer, provider adapters) exist for maintainability
- Abstractions must not introduce measurable overhead vs. direct provider calls
- Prefer trait-based dispatch with monomorphization over dynamic dispatch where feasible

### 2.4 Single Binary

- Distributed as a single Rust binary
- Web UI is embedded at compile time
- No external service dependencies (no Redis, no PostgreSQL)
- SQLite is the core embedded database for runtime configuration
- Bootstrap parameters via CLI flags or environment variables
- No legacy configuration files (`config.toml` removed)

---

## 3. System Architecture

### 3.1 High-level Diagram

```text
                ┌──────────────────────┐
                │     Web UI (embed)   │
                └─────────┬────────────┘
                          │
                  Control Plane API
                          │
┌────────────────────────────────────────────────────┐
│                    FustAPI Core                     │
│                                                     │
│  ┌───────────────┐    ┌──────────────────────────┐ │
│  │ Protocol      │    │ Capability Layer         │ │
│  │ (OAI/Claude)  │    │ (Tool Calling / Image)   │ │
│  └───────┬───────┘    └────────────┬─────────────┘ │
│          │                         │                │
│  ┌───────▼───────────┐  ┌─────────▼──────────────┐ │
│  │ Unified Model     │  │ Router                 │ │
│  │ (Request/Response)│  │ (model → provider)     │ │
│  └───────┬───────────┘  └─────────┬──────────────┘ │
│          │                         │                │
│  ┌───────▼─────────────────┐ ┌────▼──────────────┐ │
│  │ Local Providers         │ │ Cloud Providers   │ │
│  │ omlx / lmstudio / sglang│ │ DeepSeek / OpenAI │ │
│  └─────────────────────────┘ └───────────────────┘ │
└────────────────────────────────────────────────────┘
```

### 3.2 Layer Responsibilities

| Layer              | Responsibility                                         |
| ------------------ | ------------------------------------------------------ |
| Protocol           | Parse OpenAI and Anthropic request/response formats     |
| Capability Layer   | Abstract tool calling (native + emulated) and image I/O |
| Unified Model      | Provider-agnostic request/response types                |
| Router             | Map model names to provider backends                    |
| Providers          | Provider-specific adapters implementing streaming iface  |

### 3.3 Data Flow

```text
Client Request
    → Protocol Parser (identify API format: OpenAI / Anthropic)
    → Capability Layer (extract tools, images)
    → Router (resolve model → provider)
    → Provider Adapter (translate to provider-specific format)
    → Streaming Engine (Provider → Normalize → Forward SSE)
    → Client Response (SSE stream)
```

### 3.4 Cross-cutting Constraints

- Protocol logic must not be mixed with provider logic
- Each provider adapter is isolated; provider-specific quirks stay inside the adapter
- No cross-layer coupling — layers communicate through defined interfaces
- Streaming is the only response path; there is no separate buffered path

---

## 4. Provider Design

### 4.1 Local Providers (v1 Core)

#### 4.1.1 omlx

- **Role:** Primary local inference backend
- **Protocol:** Custom; requires a dedicated adapter
- **Characteristics:** High-performance local inference
- **Adapter type:** Custom implementation

#### 4.1.2 LM Studio

- **Role:** OpenAI-compatible local backend
- **Protocol:** OpenAI-compatible API
- **Characteristics:** Can reuse OpenAI protocol adapter
- **Adapter type:** Reuse / extend existing OpenAI adapter

#### 4.1.3 SGLang

- **Role:** High-performance local streaming backend
- **Protocol:** Custom protocol parsing required
- **Characteristics:** Optimized for streaming workloads
- **Adapter type:** Custom implementation

### 4.2 Cloud Providers (Fallback)

| Provider | Role    | Protocol          |
| -------- | ------- | ----------------- |
| DeepSeek | Fallback | DeepSeek API      |
| OpenAI   | Fallback | OpenAI API        |

### 4.3 Provider Adapter Contract

Every provider adapter must implement:

```rust
trait Provider {
    async fn chat_stream(&self, request: UnifiedRequest) -> Result<Stream<Item = LLMChunk>>;
    fn supports_tools(&self) -> bool;
    fn supports_images(&self) -> bool;
}
```

---

## 5. Tool Calling Design

### 5.1 Problem Statement

Most local LLM providers do not natively support tool/function calling. FustAPI must bridge this gap to support AI IDE workflows.

### 5.2 Dual-mode Strategy

| Mode      | Condition                     | Description                              |
| --------- | ----------------------------- | ---------------------------------------- |
| Native    | Provider supports tool calls  | Pass tool definitions and calls directly |
| Emulated  | Provider lacks tool support   | Gateway parses LLM output into tool calls|

### 5.3 Emulated Tool Calling Flow

```text
1. Client sends request with tool definitions
2. Gateway injects tool schema into the prompt (system message)
3. LLM generates text output containing structured tool call
4. Gateway parses LLM output → extracts tool name + arguments
5. Gateway constructs standardized tool_call response
6. Client receives tool_call as if it were native
```

### 5.4 Unified Tool Call Structure

```rust
struct ToolCall {
    name: String,
    arguments: Value,  // serde_json::Value
}

struct ToolDefinition {
    name: String,
    description: String,
    parameters: Value,  // JSON Schema
}
```

### 5.5 Rules

- Tool arguments must always be valid JSON
- Tool definitions must follow JSON Schema strictly
- No hallucinated tool fields — only fields present in the schema
- The capability layer abstracts native vs. emulated; callers are mode-agnostic
- Both OpenAI `tools` format and Anthropic `tool_use` format must be supported at the protocol layer

---

## 6. Multi-modal (Image) Handling

### 6.1 v1 Strategy

| Situation          | Handling     | Description                                    |
| ------------------ | ------------ | ---------------------------------------------- |
| Provider supports  | Passthrough  | Forward image data directly to provider        |
| Not supported      | Error/degrade| Return error or strip images with notification |

### 6.2 Rules

- Support multi-modal inputs at the protocol layer (both OpenAI and Anthropic image formats)
- Check provider capability before forwarding images
- Never assume a provider supports images
- Graceful degradation: if the provider cannot handle images, return a clear error or degrade with notification

---

## 7. Streaming Engine

### 7.1 Core Abstraction

```rust
// Every provider returns this
Stream<Item = Result<LLMChunk>>
```

### 7.2 Streaming Pipeline

```text
Provider Raw Stream
    → Normalize (provider-specific → LLMChunk)
    → Forward (LLMChunk → SSE event)
    → Client
```

### 7.3 Optimization Principles

| Principle            | Rule                                    |
| -------------------- | --------------------------------------- |
| No caching           | Chunks flow through; never stored       |
| No concatenation     | Each chunk is independent               |
| No redundant parsing | Parse once at provider boundary         |
| Zero-copy forwarding | Pass bytes, don't clone                 |

### 7.4 SSE Format

- OpenAI streaming: `data: {chunk}\n\n` with `data: [DONE]\n\n` terminator
- Anthropic streaming: event-type prefixed SSE (`message_start`, `content_block_delta`, etc.)

---

## 8. Router Design

### 8.1 Function

Maps user-facing model names to backend provider(s).

### 8.2 Configuration Example

```toml
[router]
"gpt-4" = ["omlx"]
"claude-3" = ["sglang"]
"deepseek-chat" = ["deepseek", "openai"]  # fallback chain
```

### 8.3 Behavior

- Models are matched by exact name
- Provider lists are ordered by priority (first = preferred)
- If the primary provider fails, fall through to the next in the list
- Router configuration is persisted in the SQLite database

---

## 9. API Design

### 9.1 OpenAI-compatible Endpoints

| Method | Path                    | Description          |
| ------ | ----------------------- | -------------------- |
| POST   | `/v1/chat/completions`  | Chat completion      |
| GET    | `/v1/models`            | List available models|

### 9.2 Anthropic-compatible Endpoints

| Method | Path            | Description      |
| ------ | --------------- | ---------------- |
| POST   | `/v1/messages`  | Message creation |

### 9.3 Compatibility Rules

- Request/response formats must match the respective API specifications
- Error responses must conform to the respective API's error schema
- Streaming uses SSE for both protocols
- Non-streaming responses are supported but streaming is the default and preferred path

---

## 10. Control Plane

### 10.1 v1 Scope

- Provider management (add, remove, configure providers)
- Model mapping (configure router rules)

### 10.2 Web UI

- **Stack:** React + Tailwind CSS
- **Distribution:** Embedded in the binary at compile time
- **Access:** Served via the same HTTP server as the API

---

## 11. CLI Design

### 11.1 Commands

```bash
fustapi serve              # Start the gateway server
fustapi providers list     # List configured providers
fustapi providers add      # Add a provider
fustapi routes list        # List model routes
fustapi routes add         # Add a model route
```

### 11.2 Persistence Path

```bash
~/.fustapi/fustapi.db
```

Bootstrap parameters can be set via CLI flags or environment variables:

| Parameter | CLI Flag | Env Var | Default |
|-----------|----------|---------|---------|
| Host | `--host` | `FUSTAPI_HOST` | `127.0.0.1` |
| Port | `--port` | `FUSTAPI_PORT` | `8800` |
| Data Dir | `--data-dir` | `FUSTAPI_DATA_DIR` | `~/.fustapi` |

---

## 12. Performance Design

### 12.1 Core Optimizations

| Optimization          | Implementation                        |
| --------------------- | ------------------------------------- |
| Zero-copy streaming   | Pass bytes through pipeline directly  |
| HTTP keep-alive       | Reuse connections to providers        |
| Async runtime         | tokio-based async I/O                 |
| Minimal JSON parsing  | Parse once; forward structured data   |
| No unnecessary copies | Borrow, slice, or move where possible |

### 12.2 Anti-patterns to Avoid

- Cloning large response bodies
- Buffering full responses before streaming
- Synchronous blocking on I/O
- Unnecessary serialization/deserialization rounds
- Creating intermediate `String` or `Vec<u8>` when references suffice

---

## 13. Technology Stack

| Component       | Technology                   |
| --------------- | ---------------------------- |
| Language        | Rust                         |
| Async runtime   | tokio                        |
| HTTP framework  | axum / actix-web (TBD)       |
| Serialization   | serde + serde_json           |
| SSE             | tokio-stream / custom        |
| CLI             | clap                         |
| Web UI          | React + Tailwind (embedded)  |
| Config          | CLI + Environment Variables  |
| Database        | SQLite (rusqlite)            |

---

## 14. Project Structure (Proposed)

```text
fustapi/
├── Cargo.toml
├── src/
│   ├── main.rs                 # CLI entry point
│   ├── server.rs               # HTTP server setup
│   ├── config.rs               # Configuration loading
│   ├── router/
│   │   ├── mod.rs              # Router logic
│   │   └── model_mapping.rs    # Model → provider mapping
│   ├── protocol/
│   │   ├── mod.rs              # Protocol dispatch
│   │   ├── openai.rs           # OpenAI format handling
│   │   └── anthropic.rs        # Anthropic format handling
│   ├── capability/
│   │   ├── mod.rs              # Capability layer
│   │   ├── tool.rs             # Tool calling (native + emulated)
│   │   └── image.rs            # Image input handling
│   ├── provider/
│   │   ├── mod.rs              # Provider trait + registry
│   │   ├── omlx.rs             # omlx adapter
│   │   ├── lmstudio.rs         # LM Studio adapter
│   │   ├── sglang.rs           # SGLang adapter
│   │   └── cloud/
│   │       ├── mod.rs          # Cloud provider dispatch
│   │       ├── deepseek.rs     # DeepSeek adapter
│   │       └── openai.rs       # OpenAI cloud adapter
│   └── streaming/
│       ├── mod.rs              # Streaming engine
│       └── sse.rs              # SSE serialization
├── web/                        # Web UI source (React + Tailwind)
│   ├── package.json
│   └── src/
├── config.example.toml         # Example configuration
└── docs/
    └── specs/
        └── design.md           # This document
```

---

## 15. v1 Scope Boundaries

### In Scope (v1)

- ✅ Local providers: omlx, LM Studio, SGLang
- ✅ Cloud fallback: DeepSeek, OpenAI
- ✅ Streaming on all paths
- ✅ Tool calling (native + emulated)
- ✅ Multi-modal image input (passthrough + degrade)
- ✅ OpenAI API compatibility
- ✅ Anthropic API compatibility
- ✅ AI IDE compatibility (Claude Code, OpenCode)
- ✅ Basic Web UI (provider management, model mapping)
- ✅ CLI (`serve`, `providers`, `routes`)
- ✅ Single binary distribution with embedded Web UI
- ✅ CI/CD with multi-platform releases

### Out of Scope (v1)

- ❌ Authentication / API key management for clients
- ❌ Rate limiting
- ❌ Usage metering / billing
- ❌ Model fine-tuning or training
- ❌ Distributed deployment / clustering
- ❌ Plugin system
- ❌ Advanced load balancing across providers

---

## 16. Risks and Mitigations

| Risk                                  | Mitigation                                          |
| ------------------------------------- | --------------------------------------------------- |
| Emulated tool calling accuracy        | Strict prompt engineering; validate against tool schema |
| Provider API instability              | Isolate adapter logic; fail fast with clear errors  |
| Streaming backpressure                | Backpressure-aware tokio streams                    |
| Memory usage under concurrent load    | Zero-copy design; monitor with benchmarks           |
| Protocol incompatibility edge cases   | Comprehensive integration tests against real APIs   |

---

## 17. Success Metrics

- All v1 acceptance criteria met (see §1.3)
- Streaming latency within 5ms of direct provider access
- Single binary < 20MB
- Memory usage < 50MB idle
- Zero-copy verified via benchmarks

---

## 18. Observability & Telemetry

### 18.1 High-performance Metrics

FustAPI implements a real-time observability pipeline designed for zero impact on request throughput and latency.

| Principle | Implementation |
|-----------|----------------|
| **Zero-impact** | Metrics collection uses non-blocking `mpsc` channels and lock-free telemetry. |
| **Best-effort** | Performance and low latency are prioritized over absolute accuracy. |
| **Non-blocking** | Telemetry data is offloaded to background tasks for aggregation. |

### 18.2 Accuracy Disclaimer

FustAPI metrics follow a high-performance "best-effort" model:

- **Approximate Precision**: FustAPI does NOT guarantee exact token-level precision.
- **Provider-Reported**: Usage data (tokens) is derived from provider responses when available.
- **Batch Processing**: Metrics are computed and aggregated only at request completion to avoid overhead during active streaming.

### 18.3 UI Integration

The embedded Web UI provides a real-time dashboard visualizing:
- **QPS (Requests per Second)**: Aggregated across all active providers.
- **Average Latency**: Full request-response cycle duration.
- **TTFT (Time to First Token)**: Latency until the first chunk is received.
- **Generation Speed**: Token throughput (tokens per second) during active generation.
- **Token Usage**: Aggregate prompt and completion token counts.

