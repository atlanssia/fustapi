# FustAPI

**Local-first, high-performance LLM API aggregation gateway**

[![License](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
[![Rust](https://img.shields.io/badge/Rust-1.85%2B-brightred.svg)](https://www.rust-lang.org)

FustAPI is a single-binary gateway that provides a unified entry point for AI IDEs and applications to interact with multiple LLM backends through **OpenAI-compatible** and **Anthropic-compatible** APIs.

## ✨ Features

- **Local-first inference** — Prioritizes local providers (omlx, LM Studio, SGLang) with cloud fallback (DeepSeek, OpenAI)
- **Multi-protocol** — OpenAI (`/v1/chat/completions`) and Anthropic (`/v1/messages`) API formats
- **Streaming-native** — All request paths support SSE streaming with zero-copy forwarding
- **Tool calling** — Native and emulated tool calling for AI IDE workflows
- **Image input** — Multi-modal support with graceful degradation
- **Single binary** — No external dependencies; embedded Web UI
- **AI IDE ready** — Compatible with Claude Code, OpenCode, and other tools

## 🚀 Quick Start

### Install

FustAPI provides pre-compiled binaries for major platforms through [GitHub Releases](https://github.com/atlanssia/fustapi/releases).

```bash
# Or build from source
git clone https://github.com/atlanssia/fustapi.git
cd fustapi
make build

# Install system-wide
sudo make install
```

### Configure

FustAPI uses a **configuration-first, database-backed** architecture. Runtime settings (providers, routes) are managed via the Web UI or CLI and stored in SQLite.

```bash
# List configured providers
fustapi providers list

# Add a local provider (omlx example)
fustapi providers add my-omlx --type omlx --endpoint http://localhost:11434

# Add a model route
fustapi routes add gpt-4 --providers my-omlx
```

### Run

```bash
# Start the server (defaults to 127.0.0.1:8080)
fustapi serve

# Optional: customize address and data directory
fustapi serve --host 0.0.0.0 --port 9000 --data-dir /path/to/data
```

- **Web UI**: Open `http://localhost:8080/ui` to manage providers and routes.
- **Health Check**: `curl http://localhost:8080/health` → `{"status":"ok"}`

### Monitoring & Logging

FustAPI outputs structured logs to **standard output (stdout)**. You can control the verbosity using the `RUST_LOG` environment variable:

```bash
# Debug logging
RUST_LOG=debug fustapi serve

# Warning and Error only
RUST_LOG=warn fustapi serve
```


### Test

```bash
# OpenAI-compatible endpoint
curl -X POST http://localhost:8080/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"gpt-4","messages":[{"role":"user","content":"Hello"}]}'

# Anthropic-compatible endpoint
curl -X POST http://localhost:8080/v1/messages \
  -H 'Content-Type: application/json' \
  -d '{"model":"claude-3","messages":[{"role":"user","content":"Hello"}],"max_tokens":1024}'
```

## 📖 Documentation

- [**User Manual**](docs/specs/user-manual.md) — Installation, configuration, CLI, API, provider setup, deployment, troubleshooting
- [**Design Specification**](docs/specs/design.md) — Architecture, provider design, streaming, router, performance
- [**Deployment Guide**](docs/deployment.md) — Production deployment with systemd, Docker, reverse proxy

## 🏗️ Architecture

```
Client Request
    → Protocol Parser (OpenAI / Anthropic)
    → Capability Layer (tools, images)
    → Router (model → provider)
    → Provider Adapter (local or cloud)
    → Streaming Engine (SSE forwarder)
    → Client Response
```

## 🛠️ Supported Providers

| Provider   | Type   | Default Endpoint              |
|------------|--------|-------------------------------|
| omlx       | Local  | `http://localhost:11434`      |
| LM Studio  | Local  | `http://localhost:1234`       |
| SGLang     | Local  | `http://localhost:30000`      |
| DeepSeek   | Cloud  | `https://api.deepseek.com`    |
| OpenAI     | Cloud  | `https://api.openai.com`      |

## 📁 Persistence & Bootstrap

FustAPI stores runtime data in a SQLite database. Bootstrap parameters can be set via CLI flags or environment variables.

| Parameter | CLI Flag | Env Var | Default |
|-----------|----------|---------|---------|
| Host | `--host` | `FUSTAPI_HOST` | `127.0.0.1` |
| Port | `--port` | `FUSTAPI_PORT` | `8080` |
| Data Dir | `--data-dir` | `FUSTAPI_DATA_DIR` | `~/.fustapi` |

**Note**: `config.toml` is no longer used. All persistent state resides in `{data-dir}/fustapi.db`.

## 🔧 Development

```bash
# Full validation (clippy + tests)
make all

# Build release binary
make build

# Run tests
make test

# Run clippy (fails on warnings)
make clippy

# Run in development mode
make run

# Format code
make format

# Clean build artifacts
make clean
```

## 📄 License

Apache-2.0 — see [LICENSE](LICENSE) for details.

## 🤝 Contributing

Issues, questions, or contributions — please visit the [GitHub repository](https://github.com/atlanssia/fustapi).
