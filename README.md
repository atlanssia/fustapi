# FustAPI

**Local-first, high-performance LLM API aggregation gateway**

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
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

```bash
# Clone and build
git clone https://github.com/atlanssia/fustapi.git
cd fustapi
make build

# Or install system-wide
sudo make install
```

### Configure

```bash
# Initialize default configuration
fustapi config init

# Edit the config file
$EDITOR ~/.fustapi/config.toml
```

### Run

```bash
# Start the server
fustapi serve

# Health check
curl http://localhost:8080/health
# → {"status":"ok"}

# Open the Web UI
open http://localhost:8080/ui
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

## 📁 Configuration

Configuration is stored in a platform-specific directory:

| Platform | Path |
|----------|------|
| macOS / Linux | `~/.fustapi/config.toml` |
| Windows | `%APPDATA%\fustapi\config.toml` |

See [config.example.toml](config.example.toml) for a complete example.

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

MIT — see [LICENSE](LICENSE) for details.

## 🤝 Contributing

Issues, questions, or contributions — please visit the [GitHub repository](https://github.com/atlanssia/fustapi).
