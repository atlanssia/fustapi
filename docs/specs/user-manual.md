# FustAPI User Manual

> **Version:** 0.1.0
> **Last Updated:** 2026-04-29

---

## Table of Contents

1. [Overview](#overview)
2. [Prerequisites](#prerequisites)
3. [Installation](#installation)
4. [Configuration](#configuration)
5. [Running the Server](#running-the-server)
6. [Using the Web UI](#using-the-web-ui)
7. [CLI Reference](#cli-reference)
8. [API Reference](#api-reference)
9. [Provider Setup](#provider-setup)
10. [Tool Calling](#tool-calling)
11. [Image Input](#image-input)
12. [Deployment](#deployment)
13. [Troubleshooting](#troubleshooting)
14. [Performance Tuning](#performance-tuning)

---

## Overview

FustAPI is a **local-first, high-performance LLM API aggregation gateway** written in Rust. It provides a single entry point for AI IDEs and applications to interact with multiple LLM backends through **OpenAI-compatible** and **Anthropic-compatible** APIs.

### Key Features

- **Local-first inference** — Prioritizes local providers (omlx, LM Studio, SGLang) with cloud fallback (DeepSeek, OpenAI)
- **Multi-protocol** — Supports both OpenAI and Anthropic API formats
- **Streaming-native** — All request paths support SSE streaming with zero-copy forwarding
- **Tool calling** — Native and emulated tool calling for AI IDE workflows
- **Image input** — Multi-modal support with graceful degradation
- **Single binary** — No external dependencies; embedded Web UI
- **AI IDE ready** — Compatible with Claude Code, OpenCode, and other tools

### Architecture

```
Client Request
    → Protocol Parser (OpenAI / Anthropic)
    → Capability Layer (tools, images)
    → Router (model → provider)
    → Provider Adapter (local or cloud)
    → Streaming Engine (SSE forwarder)
    → Client Response
```

---

## Prerequisites

### System Requirements

| Requirement | Minimum | Recommended |
|-------------|---------|-------------|
| OS | macOS 12+, Linux (x86_64/arm64), Windows | macOS 14+, Linux (x86_64/arm64) |
| RAM | 512 MB | 1 GB+ |
| Disk | 50 MB | 200 MB (for local models) |
| CPU | Any modern CPU | Multi-core for concurrent requests |

### Build Requirements (from source)

- **Rust 1.85+** (edition 2024)
- **Cargo** package manager
- **Build tools:** `gcc` or `clang`

```bash
# Install Rust via rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# Verify installation
rustc --version   # 1.85+
cargo --version   # 1.85+
```

### Runtime Requirements

- No external services required (no Redis, no PostgreSQL)
- Local inference providers must be running separately (e.g., LM Studio, omlx, SGLang)

---

## Installation

### Option 1: Pre-built Binary (Recommended)

Download the release tarball from the project's release page:

```bash
# Download and extract
tar -xzf fustapi-0.1.0.tar.gz

# Verify the binary
./fustapi --version
```

### Option 2: Build from Source

```bash
# Clone the repository
git clone https://github.com/atlanssia/fustapi.git
cd fustapi

# Build release binary (optimized, stripped, LTO)
make build

# Or use cargo directly
cargo build --release
```

The release binary will be at `target/release/fustapi`.

### Option 3: Install System-wide

```bash
# Build and install to /usr/local/bin (requires sudo)
sudo make install

# Verify installation
fustapi --version
```

### Option 4: Using Makefile Targets

```bash
# Full validation (clippy + tests)
make all

# Build only
make build

# Run tests only
make test

# Run clippy (fails on warnings)
make clippy

# Format code
make format

# Clean build artifacts
make clean
```

---

## Configuration

### Configuration File Location

FustAPI stores its configuration in a platform-specific config directory:

| Platform | Path |
|----------|------|
| macOS | `~/.fustapi/config.toml` |
| Linux | `~/.fustapi/config.toml` |
| Windows | `%APPDATA%\fustapi\config.toml` |

### Initialize Default Configuration

```bash
fustapi config init
```

This creates a default `config.toml` with placeholder values:

```toml
[server]
host = "127.0.0.1"
port = 8080

[router]
"gpt-4" = ["omlx"]
"claude-3" = ["sglang"]

[providers.omlx]
endpoint = "http://localhost:11434"

[providers.lmstudio]
endpoint = "http://localhost:1234"

[providers.sglang]
endpoint = "http://localhost:30000"

[providers.deepseek]
api_key = "sk-..."
endpoint = "https://api.deepseek.com"

[providers.openai]
api_key = "sk-..."
endpoint = "https://api.openai.com"
```

### Configuration Sections

#### Server Settings

```toml
[server]
host = "127.0.0.1"   # Bind address (0.0.0.0 for all interfaces)
port = 8080           # Port number (1-65535)
```

#### Router Mapping

Maps model names to provider backends in priority order:

```toml
[router]
"gpt-4" = ["omlx"]                    # Use omlx for gpt-4 models
"claude-3" = ["sglang"]               # Use sglang for claude models  
"deepseek-chat" = ["deepseek", "openai"]  # Fallback chain: deepseek → openai
```

**Rules:**

- Model names are matched by exact string match
- Provider lists are ordered by priority (first = preferred)
- If the primary provider fails, FustAPI falls through to the next provider in the list
- Unknown model names return a clear error message

#### Provider Configuration

Each provider section defines connection details:

```toml
[providers.<provider_name>]
endpoint = "http://localhost:<port>"   # Required: provider API endpoint
api_key = "sk-..."                    # Optional: API key for cloud providers
```

**Local Providers:**

| Provider   | Typical Endpoint              | Notes |
|------------|-------------------------------|-------|
| omlx       | `http://localhost:11434`      | High-performance local inference |
| lmstudio   | `http://localhost:1234`       | OpenAI-compatible local backend |
| sglang     | `http://localhost:30000`      | High-performance local streaming |

**Cloud Providers:**

| Provider   | Endpoint                      | Notes |
|------------|-------------------------------|-------|
| deepseek   | `https://api.deepseek.com`    | Requires `api_key` |
| openai     | `https://api.openai.com`      | Requires `api_key` |

### Security Notes

- API keys are stored in plain text in the config file — protect file permissions:

```bash
chmod 600 ~/.fustapi/config.toml
```

- Never commit your config file to version control (it is listed in `.gitignore`)

---

## Running the Server

### Start the Server (Development)

```bash
# Default settings (127.0.0.1:8080)
fustapi serve

# Custom host and port
fustapi serve --host 0.0.0.0 --port 3000

# Or via cargo (slower, debug mode)
cargo run -- serve --host 0.0.0.0 --port 8080
```

### Start the Server (Release)

```bash
# Build first, then run release binary make run-release make build && ./target/release/fustapi serve --host 0.0.0.0 --port 8080 ``` ### Health Check Verify the server is running: ```bash curl http://localhost:8080/health # Expected response: {"status":"ok"} ``` ### Server Logs FustAPI uses `tracing-subscriber` with `env-filter` for structured logging: ```bash # Set log level via RUST_LOG environment variable RUST_LOG=debug fustapi serve RUST_LOG=info fustapi serve # Default log levels: # info — production default # debug — detailed request/response logging # trace — full diagnostic output ``` --- ## Using the Web UI ### Accessing the Web UI The Web UI is embedded in the binary and served at `/ui/`: ```bash open http://localhost:8080/ui ``` ### Features The Web UI provides a tabbed interface for: #### Providers Tab - View configured providers and their status - See provider endpoints and connection details #### Models Tab - View available models across all providers - See model-to-provider mappings #### Control Plane API The Web UI consumes these API endpoints: - `GET /api/providers` — List all configured providers - `GET /api/models` — List all available models --- ## CLI Reference ### Commands #### `fustapi serve` Start the gateway server. ```bash fustapi serve [--host HOST] [--port PORT] fustapi serve --host 0.0.0.0 --port 8080 ``` **Options:** - `--host HOST` — Bind address (default: `127.0.0.1`) - `--port PORT` — Port number (default: `8080`) #### `fustapi config init` Initialize a default configuration file at `~/.fustapi/config.toml`. ```bash fustapi config init ``` #### `fustapi providers` List configured providers and their status. ```bash fustapi providers ``` ### Help Display usage information: ```bash fustapi --help fustapi serve --help fustapi config --help ``` --- ## API Reference ### OpenAI-Compatible Endpoints #### Chat Completions ```bash POST /v1/chat/completions Content-Type: application/json { "model": "gpt-4", "messages": [ {"role": "user", "content": "Hello"} ], "stream": true } ``` #### List Models ```bash GET /v1/models ``` ### Anthropic-Compatible Endpoints #### Messages ```bash POST /v1/messages Content-Type: application/json { "model": "claude-3", "messages": [ {"role": "user", "content": "Hello"} ], "stream": true, "max_tokens": 1024 } ``` ### Health Check ```bash GET /health # Response: {"status":"ok"} ``` ### Control Plane API #### List Providers ```bash GET /api/providers # Response: [{"name":"omlx","endpoint":"http://localhost:11434",...}] ``` #### List Models ```bash GET /api/models # Response: [{"model":"gpt-4","provider":"omlx"},...] ``` --- ## Provider Setup ### LM Studio 1. Install [LM Studio](https://lmstudio.ai/) 2. Download a model in LM Studio 3. Start the local server (usually on port 1234) 4. Verify configuration in `~/.fustapi/config.toml`: ```toml [providers.lmstudio] endpoint = "http://localhost:1234" ``` ### omlx 1. Ensure omlx is running on your system 2. Configure endpoint in `~/.fustapi/config.toml`: ```toml [providers.omlx] endpoint = "http://localhost:11434" ``` ### SGLang 1. Install and start SGLang server: ```bash pip install sglang python -m sglang.launch_server --model lmsys/vicuna-7b-v1.5 --port 30000 ``` 2. Configure endpoint in `~/.fustapi/config.toml`: ```toml [providers.sglang] endpoint = "http://localhost:30000" ``` ### DeepSeek (Cloud Fallback) 1. Obtain an API key from [DeepSeek](https://deepseek.com/) 2. Configure in `~/.fustapi/config.toml`: ```toml [providers.deepseek] api_key = "sk-your-api-key-here" endpoint = "https://api.deepseek.com" ``` ### OpenAI (Cloud Fallback) 1. Obtain an API key from [OpenAI](https://platform.openai.com/) 2. Configure in `~/.fustapi/config.toml`: ```toml [providers.openai] api_key = "sk-your-api-key-here" endpoint = "https://api.openai.com" ``` --- ## Tool Calling ### Native Tool Calling When a provider supports native tool calling, FustAPI passes tool definitions directly to the provider and forwards tool call responses as-is. **Example request:** ```bash curl -X POST http://localhost:8080/v1/chat/completions \ -H "Content-Type: application/json" \ -d '{ "model": "gpt-4", "messages": [{"role": "user", "content": "What is the weather?"}], "tools": [{ "type": "function", "function": { "name": "get_weather", "description": "Get current weather for a location", "parameters": { "type": "object", "properties": { "location": { "type": "string", "description": "City name or coordinates" } }, "required": ["location"] } } }] }' ``` ### Emulated Tool Calling For providers without native tool support, FustAPI injects tool schemas into the system prompt and parses LLM output to extract structured tool calls. This works transparently — clients receive the same unified `ToolCall` format regardless of mode. --- ## Image Input ### Supported Workflow FustAPI accepts multi-modal inputs with base64-encoded images at the protocol layer (both OpenAI and Anthropic formats). Image handling depends on provider capability: - **Provider supports images:** Images are passed through directly - **Provider does not support images:** A clear error is returned ### Example Request (OpenAI Format) ```bash curl -X POST http://localhost:8080/v1/chat/completions \ -H "Content-Type: application/json" \ -d '{ "model": "gpt-4", "messages": [{ "role": "user", "content": [ {"type": "text", "text": "What is in this image?"}, {"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,/9j/4AAQSk..."}} ] }] }' ``` --- ## Deployment ### Linux Systemd Service Create a systemd service file at `/etc/systemd/system/fustapi.service`: ```ini [Unit] Description=FustAPI LLM Gateway After=network.target [Service] Type=simple User=fustapi ExecStart=/usr/local/bin/fustapi serve --host 127.0.0.1 --port 8080 Restart=on-failure RestartSec=5 Environment=RUST_LOG=info WorkingDirectory=/home/fustapi [Install] WantedBy=multi-user.target ``` Enable and start the service: ```bash sudo systemctl daemon-reload sudo systemctl enable fustapi sudo systemctl start fustapi sudo systemctl status fustapi ``` ### Docker (Future) Docker support is planned for future releases. Monitor the project repository for updates. ### Environment Variables FustAPI supports these environment variables for runtime configuration: | Variable | Description | Default ||----------|-------------|---------|| `RUST_LOG` | Log level (`info`, `debug`, `trace`) | `info` || `FUSTAPI_HOST` | Override bind address from config | Config value || `FUSTAPI_PORT` | Override port from config | Config value ### Firewall Configuration If running behind a firewall, allow inbound traffic on the configured port: ```bash # iptables example sudo iptables -A INPUT -p tcp --dport 8080 -j ACCEPT # UFW example sudo ufw allow 8080/tcp ``` --- ## Troubleshooting ### Common Issues #### Server Won't Start **Port already in use:** ```bash # Check what's using the port lsof -i :8080 # Kill the process or use a different port fustapi serve --port 3001 ``` **Config file errors:** Check server logs for specific error messages about TOML parsing or missing fields. #### Provider Connection Failed **Verify provider is running:** ```bash # Test provider endpoint directly curl http://localhost:1234/v1/models # LM Studio default endpoint ``` **Check config endpoint:** Ensure the endpoint URL in `~/.fustapi/config.toml` matches your provider's actual address. #### Tool Calling Not Working **Check provider support:** Some local providers may not support tool calling natively. FustAPI falls back to emulated mode automatically, but accuracy depends on the model's ability to follow structured output instructions. **Validate tool schema:** Ensure tool definitions follow JSON Schema strictly with valid field names and types. #### High Memory Usage FustAPI is designed for minimal memory usage (<50MB idle). If memory usage is high: - Check for memory leaks in provider adapters - Reduce concurrent request load - Review streaming behavior (ensure clients consume streams properly) #### Streaming Issues If streaming responses are delayed or incomplete: - Verify provider supports SSE streaming - Check network connectivity to provider endpoints - Enable debug logging to trace chunk flow: ```bash RUST_LOG=debug fustapi serve ``` ### Log Levels Use `RUST_LOG` to control verbosity: ```bash # Production default — info level RUST_LOG=info fustapi serve # Debug — detailed request/response logging RUST_LOG=debug fustapi serve # Trace — full diagnostic output RUST_LOG=trace fustapi serve # Per-module logging RUST_LOG=fustapi::router=debug,fustapi::provider=info fustapi serve ``` --- ## Performance Tuning ### Build Optimizations The release profile includes these optimizations by default: - **Strip symbols** (`strip = true`) — Reduces binary size - **Size optimization** (`opt-level = z`) — Minimizes binary footprint - **Link-time optimization** (`lto = true`) — Cross-crate optimizations - **Single codegen unit** (`codegen-units = 1`) — Maximum optimization at cost of build time ### Runtime Tuning #### Connection Reuse FustAPI uses HTTP keep-alive by default for connections to local providers, reducing connection setup overhead for concurrent requests. #### Concurrency The tokio async runtime handles concurrency efficiently by default. For high-throughput deployments, consider tuning tokio's worker threads: ```bash # Set number of tokio worker threads export TOKIO_WORKER_THREADS=4 fustapi serve ``` #### Bind Address For local-only access, use `127.0.0.1`. For network access, use `0.0.0.0`: ```bash # Local only (default, most secure) fustapi serve --host 127.0.0.1 # Network accessible fustapi serve --host 0.0.0.0 --port 8080 ``` --- ## Support & Contributing For issues, questions, or contributions, please visit the project repository on GitHub. ### Quick Reference Card ```bash # Install fustapi sudo make install # Initialize config fustapi config init # Edit config $EDITOR ~/.fustapi/config.toml # Start server fustapi serve # Health check curl http://localhost:8080/health # Open Web UI open http://localhost:8080/ui # Test OpenAI endpoint curl -X POST http://localhost:8080/v1/chat/completions \ -H 'Content-Type: application/json' \ -d '{"model":"gpt-4","messages":[{"role":"user","content":"Hello"}]}' # Test Anthropic endpoint curl -X POST http://localhost:8080/v1/messages \ -H 'Content-Type: application/json' \ -d '{"model":"claude-3","messages":[{"role":"user","content":"Hello"}],"max_tokens":1024}' # Stop server Ctrl+C ``` --- *This manual covers FustAPI v0.1.x features and configuration options.* 
