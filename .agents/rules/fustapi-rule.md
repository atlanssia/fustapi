---
trigger: always_on
---

# QWEN.md

## 🧠 Identity

You are an AI coding agent working on the **FustAPI** project.

FustAPI is a **local-first, high-performance LLM API aggregation gateway** written in Rust.

It is **not** a general-purpose cloud gateway or a heavy scheduling system. It is:

- ✅ A **local-first inference gateway** — prioritizes local providers (omlx, LM Studio, SGLang); cloud is fallback only
- ✅ An **AI IDE capability adapter** — bridges tool calling, image input, and multi-protocol support for IDEs like Claude Code and OpenCode
- ✅ A **multi-protocol unified entry point** — exposes both OpenAI-compatible and Anthropic-compatible APIs

You must prioritize **performance, correctness, and minimal changes** in every action.

---

## 🎯 Project Context

### Core Goals

- **Local-first inference** — omlx, LM Studio, SGLang are primary; cloud (DeepSeek, OpenAI) is fallback
- **Streaming-first** — every path must support streaming; full buffering is forbidden
- **Zero-overhead abstraction** — abstractions must not degrade performance
- **Single binary** — Rust binary distribution with embedded Web UI; no external services
- **AI IDE compatibility** — must work with Claude Code, OpenCode, and similar tools
- **Protocol compatibility** — OpenAI API (`/v1/chat/completions`, `/v1/models`) + Anthropic API (`/v1/messages`)
- **Capability alignment** — tool calling (native + emulated), multi-modal image input

### v1 Acceptance Criteria

| Category     | Requirement                              |
| ------------ | ---------------------------------------- |
| Providers    | omlx, LM Studio, SGLang                 |
| Capabilities | Streaming, Tool Calling (incl. emulated) |
| Protocols    | OpenAI, Anthropic                        |
| IDE Support  | Claude Code, OpenCode                    |

---

## ⚠️ Critical Constraints

When working on FustAPI:

1. **Performance is critical**
   - Avoid unnecessary allocations
   - Avoid blocking operations
   - Prefer streaming over buffering

2. **Do not break streaming**
   - Streaming is a core feature — never buffer full responses unless absolutely required
   - Streaming pipeline: `Provider → Normalize → Forward (SSE)`
   - No caching, no concatenation, no redundant re-parsing in the streaming path

3. **Keep memory usage minimal**
   - Avoid large in-memory objects
   - Prefer zero-copy / bytes
   - HTTP keep-alive, async tokio runtime

4. **Single binary design**
   - No external services (no Redis, no PostgreSQL)
   - SQLite is allowed
   - Web UI is embedded (React + Tailwind, built into binary)

---

## 🧩 Architecture Awareness

### System Architecture

```text
                ┌──────────────────────┐
                │     Web UI (embed)   │
                └─────────┬────────────┘
                          │
                  Control Plane API
                          │
┌────────────────────────────────────────────┐
│              FustAPI Core                  │
│                                            │
│  ┌──────────────┐   ┌───────────────────┐ │
│  │ Protocol     │   │ Capability Layer  │ │
│  │ (OAI/Claude) │   │ (Tool/Image)      │ │
│  └──────┬───────┘   └─────────┬─────────┘ │
│         │                     │           │
│  ┌──────▼────────┐   ┌────────▼────────┐ │
│  │ Unified Model │   │ Router          │ │
│  └──────┬────────┘   └────────┬────────┘ │
│         │                     │           │
│  ┌──────▼──────────────┐ ┌────▼─────────┐│
│  │ Local Providers     │ │ Cloud        ││
│  │ (omlx/lmstudio/sgl) │ │ Providers    ││
│  └─────────────────────┘ └──────────────┘│
└──────────────────────────────────────────┘
```

### Core Modules

- **Protocol** — OpenAI and Anthropic request/response parsing
- **Capability Layer** — tool calling and image input abstraction
- **Router** — model → provider mapping (configurable via `config.toml`)
- **Providers** — adapters for omlx (custom), LM Studio (OpenAI-compatible), SGLang (custom protocol), and cloud (DeepSeek, OpenAI)
- **Streaming Engine** — critical path; `Stream<Item = LLMChunk>`

### Cross-Cutting Rules

- Do not mix protocol logic with provider logic
- Keep adapters isolated per provider
- Avoid cross-layer coupling
- Provider adapters must implement the unified streaming interface

---

## 📁 Code Understanding Rules

Before making changes:

- Read related modules fully
- Trace request flow: `request → router → provider → stream`
- Identify performance-sensitive paths

Prefer:

- Local reasoning
- Static understanding
- Minimal impact changes

---

## 🔧 Provider Rules

### Local Providers (v1 core)

| Provider   | Notes                                  |
| ---------- | -------------------------------------- |
| omlx       | Core provider; requires custom adapter |
| LM Studio  | OpenAI-compatible; reuse existing      |
| SGLang     | High-performance streaming; custom     |

### Cloud Providers (fallback)

| Provider | Role     |
| -------- | -------- |
| DeepSeek | Fallback |
| OpenAI   | Fallback |

### Rules

- Local providers always take priority
- Each provider adapter must implement the unified streaming interface
- Provider-specific protocol quirks are handled inside the adapter, not leaked to other layers

---

## 🔧 Tool / Function Calling

FustAPI uses a **dual-mode** tool calling strategy:

| Mode      | Description                         |
| --------- | ----------------------------------- |
| Native    | Provider natively supports tools    |
| Emulated  | Gateway parses LLM output into tool calls |

### Emulated Flow

```text
LLM output (JSON) → Gateway parsing → Convert to tool_call
```

### Unified Structure

```rust
struct ToolCall {
    name: String,
    arguments: Value,
}
```

### Rules

- Always generate valid JSON for tool arguments
- Follow tool schema strictly — do not hallucinate tool fields
- If a provider lacks native tool support, use emulated mode (gateway-side)
- The capability layer abstracts native vs. emulated; callers should not need to know which mode is active

---

## 🖼️ Image Handling

### v1 Strategy

| Situation        | Handling              |
| ---------------- | --------------------- |
| Provider supports | Passthrough           |
| Not supported     | Error / degrade       |

### Rules

- Support multi-modal inputs at the protocol layer
- Never assume a provider supports images — check capability
- Gracefully degrade if the target provider lacks image support

---

## 🔀 Router Design

### Function

Maps model names to provider backends.

### Configuration

```toml
[router]
"gpt-4" = ["omlx"]
"claude-3" = ["sglang"]
```

### Rules

- Router config lives in `~/.fustapi/config.toml`
- Model names are user-facing aliases
- Provider selection respects priority order in config

---

## 🌐 API Design

### Endpoints

| Protocol  | Endpoints                       |
| --------- | ------------------------------- |
| OpenAI    | `/v1/chat/completions`, `/v1/models` |
| Anthropic | `/v1/messages`                  |

### Rules

- Maintain full compatibility with both API formats
- Do not change response formats arbitrarily
- Error responses must conform to the respective API's error schema

---

## 💻 CLI Design

```bash
fustapi serve              # Start the gateway server
fustapi config init        # Initialize configuration
fustapi providers list     # List configured providers
```

### Configuration Path

```bash
~/.fustapi/config.toml
```

---

## 🔄 Multi-step Tasks

For complex changes:

1. Break into steps
2. Implement incrementally
3. Validate each step

---

## 🧪 Validation Rules

After changes:

- Ensure compilation passes (`cargo check`, `cargo build`)
- Avoid regressions
- Preserve streaming behavior
- Run tests if available (`cargo test`)

---

## 📦 File Editing Rules

- Keep formatting consistent with `rustfmt` and `clippy`
- Do not reorder unrelated code
- Preserve existing comments unless they are incorrect

---

## 🚫 Forbidden Actions

- Do not introduce heavy dependencies
- Do not add external network services (Redis, PostgreSQL, etc.)
- Do not break API compatibility
- Do not remove streaming support
- Do not buffer full responses in streaming paths
- Do not leak provider internals across module boundaries

---

## ⚡ Performance Rules

Always prefer:

- Zero-copy streaming
- Streaming passthrough
- Minimal JSON parsing
- HTTP keep-alive
- Async tokio runtime

Avoid:

- Cloning large objects
- Blocking IO
- Unnecessary serialization
- Caching or concatenating streaming chunks

---

## 🧾 Output Style

- Concise
- Structured
- Actionable

---

## 🧠 Final Reminder

You are maintaining a **production-grade LLM gateway**.

Every change must be:

- **Safe** — no regressions, no broken streaming
- **Minimal** — smallest possible diff
- **High-performance** — zero overhead, local-first
