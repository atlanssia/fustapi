# FustAPI User Manual

> **Version:** 1.0.3
> **Last Updated:** 2026-05-01

---

## Table of Contents

1. [Overview](#overview)
2. [Prerequisites](#prerequisites)
3. [Installation](#installation)
4. [Bootstrap & Persistence](#bootstrap--persistence)
5. [Configuration](#configuration)
6. [Running the Server](#running-the-server)
7. [Using the Web UI](#using-the-web-ui)
8. [CLI Reference](#cli-reference)
9. [API Reference](#api-reference)
10. [Provider Setup](#provider-setup)
11. [Tool Calling](#tool-calling)
12. [Image Input](#image-input)
13. [Deployment](#deployment)
14. [Troubleshooting](#troubleshooting)
15. [Performance Tuning](#performance-tuning)

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
- **Database-backed** — Persistence via SQLite; no more complex configuration files

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
```

---

## Installation

### Option 1: Pre-built Binary (Recommended)

Download the release binary for your platform from the [GitHub Releases](https://github.com/atlanssia/fustapi/releases) page.

```bash
# Download and extract
tar -xzf fustapi-1.0.0-x86_64-unknown-linux-gnu.tar.gz

# macOS Users: If Gatekeeper blocks the binary ("Apple could not verify..."), run:
xattr -d com.apple.quarantine fustapi

# Verify the binary
./fustapi --version
```

### Option 2: Build from Source

```bash
# Clone the repository
git clone https://github.com/atlanssia/fustapi.git
cd fustapi

# Build release binary
make build
```

The release binary will be at `target/release/fustapi`.

### Option 3: Install System-wide

```bash
# Build and install to /usr/local/bin (requires sudo)
sudo make install

# Verify installation
fustapi --version
```

---

## Bootstrap & Persistence

FustAPI has moved away from file-based configuration (`config.toml`). Instead, it uses a **bootstrap model** for server parameters and a **database model** for runtime data.

### Bootstrap Parameters

These settings define how the server starts and where it finds its data. They can be set via CLI flags or environment variables.

| Parameter | CLI Flag | Env Var | Default |
|-----------|----------|---------|---------|
| Host | `--host` | `FUSTAPI_HOST` | `127.0.0.1` |
| Port | `--port` | `FUSTAPI_PORT` | `6000` |
| Data Dir | `--data-dir` | `FUSTAPI_DATA_DIR` | `~/.fustapi` |
| Log Level | `-v` / `-vv` | `RUST_LOG` | `info` |

### Persistence

All runtime configuration (providers, routes) is stored in a SQLite database located at `{data-dir}/fustapi.db`. By default, this is `~/.fustapi/fustapi.db`.

---

## Configuration

Configuration is now managed entirely through the **CLI** or the **Web UI**.

### Managing Providers via CLI

```bash
# List configured providers
fustapi providers list

# Add a local provider (omlx)
fustapi providers add my-omlx --type omlx --endpoint http://localhost:11434/v1

# Add a cloud provider (DeepSeek)
fustapi providers add ds --type deepseek --endpoint https://api.deepseek.com/v1 --api-key sk-...
```

### Managing Routes via CLI

Routes map model names (e.g., `gpt-4`) to a priority list of providers.

```bash
# List configured routes
fustapi routes list

# Create a route with fallback
fustapi routes add gpt-4 --providers my-omlx,ds
```

**Rules:**

- Model names are matched by exact string match.
- Provider lists are ordered by priority (first = preferred).
- If the primary provider fails, FustAPI falls through to the next provider in the list.

### Managing via Web UI

1. Start the server: `fustapi serve`
2. Open `http://localhost:6000/ui` in your browser.
3. Use the **Providers** and **Models** tabs to add, edit, or remove configurations.

### Security Notes

- API keys are stored in the SQLite database. Ensure the data directory has restricted permissions:

```bash
chmod 700 ~/.fustapi
```

---

## Running the Server

### Start the Server

```bash
# Default settings (127.0.0.1:6000)
fustapi serve

# Custom host and port
fustapi serve --host 0.0.0.0 --port 3000

# Custom data directory
fustapi serve --data-dir /var/lib/fustapi
```

### Health Check

Verify the server is running:

```bash
curl http://localhost:6000/health
# Expected response: {"status":"ok"}
```

### Server Logs & Monitoring

FustAPI uses a structured logging system based on `tracing`. By default, all logs are streamed to **standard output (stdout)**.

#### Log Levels

You can control the verbosity using the `RUST_LOG` environment variable.

| Level | Description |
|-------|-------------|
| `error` | Only critical failures |
| `warn` | Potential issues and non-fatal errors |
| `info` | (Default) Normal operational events (startup, incoming requests) |
| `debug` | Detailed internal state (provider mapping, request routing) |
| `trace` | Extremely granular data (including low-level HTTP traffic) |

#### Usage Examples

```bash
# High verbosity for development/debugging
RUST_LOG=debug fustapi serve

# Production-standard logging
RUST_LOG=info fustapi serve

# Silence all but errors
RUST_LOG=error fustapi serve
```

#### Persisting Logs to a File

If you need to keep a permanent record of logs, use Shell redirection:

```bash
# Save both stdout and stderr to a file
fustapi serve > fustapi.log 2>&1
```

---

## Using the Web UI

The Web UI is a single-page application embedded in the binary and served at `/ui/`.

### Accessing the Web UI

```bash
open http://localhost:6000/ui
```

### Features

- **Providers Tab**: Manage backend LLM services (omlx, LM Studio, SGLang, DeepSeek, OpenAI).
- **Models Tab**: Manage model names and their routing priority (fallback chains).
- **Control Plane API**: The UI interacts with internal API endpoints (`/api/providers`, `/api/routes`).

---

## CLI Reference

### `fustapi serve`

Start the gateway server.

- `--host HOST`: Bind address (default: `127.0.0.1`)
- `--port PORT`: Port number (default: `6000`)
- `--data-dir DIR`: Directory for SQLite database (default: `~/.fustapi`)

### `fustapi providers`

Manage LLM providers.

- `list`: Show all providers and their status.
- `add <NAME> --type <TYPE> --endpoint <URL> [--api-key <KEY>]`: Add a new provider.

### `fustapi routes`

Manage model routing.

- `list`: Show all model routes.
- `add <MODEL> --providers <NAME1,NAME2>`: Add or update a model route.

---

## API Reference

### OpenAI-Compatible Endpoints

#### Chat Completions
`POST /v1/chat/completions`

#### List Models
`GET /v1/models`

### Anthropic-Compatible Endpoints

#### Messages
`POST /v1/messages`

---

## Provider Setup

### Local Providers
- **omlx**: Default `http://localhost:11434/v1`
- **LM Studio**: Default `http://localhost:1234/v1`
- **SGLang**: Default `http://localhost:30000/v1`

### Cloud Providers
- **DeepSeek**: `https://api.deepseek.com/v1` (Requires API Key)
- **OpenAI**: `https://api.openai.com/v1` (Requires API Key)

---

## Tool Calling

### Native Tool Calling
When a provider supports native tool calling, FustAPI passes tool definitions directly to the provider and forwards tool call responses as-is.

### Emulated Tool Calling
For providers without native tool support, FustAPI injects tool schemas into the system prompt and parses LLM output to extract structured tool calls.

---

## Image Input

### Supported Workflow
FustAPI accepts multi-modal inputs with base64-encoded images at the protocol layer. Image handling depends on provider capability:
- **Provider supports images:** Images are passed through directly.
- **Provider does not support images:** A clear error is returned.

---

## Deployment

### Linux Systemd Service

Create a systemd service file at `/etc/systemd/system/fustapi.service`:

```ini
[Unit]
Description=FustAPI LLM Gateway
After=network.target

[Service]
Type=simple
User=fustapi
ExecStart=/usr/local/bin/fustapi serve --host 0.0.0.0 --port 6000
Restart=on-failure
RestartSec=5
Environment=RUST_LOG=info
WorkingDirectory=/home/fustapi

[Install]
WantedBy=multi-user.target
```

---

## Troubleshooting

### macOS Gatekeeper Warning ("Apple could not verify...")
When running a downloaded release binary on macOS (especially Apple Silicon), Gatekeeper may block the execution. To resolve this, remove the quarantine attribute:
```bash
xattr -d com.apple.quarantine /path/to/fustapi
```

### DB Initialization Failures
Ensure FustAPI has write permissions to the data directory (default `~/.fustapi`).

### Connection Refused
- Check if the provider service is running.
- Verify the endpoint URL in the Providers configuration.
- Check firewall settings for the configured port.

### Accessing Logs
Detailed operational logs are essential for diagnosing issues. 

- **Stdout**: Normal runtime logs (info, debug).
- **Stderr**: Critical bootstrap errors and unhandled server panics.

Always run with `RUST_LOG=debug` when investigating connectivity or routing issues:
```bash
RUST_LOG=debug fustapi serve
```

---

## Performance Tuning

### Build Optimizations
The release profile includes:
- **Strip symbols**
- **Size optimization**
- **Link-time optimization (LTO)**
- **Single codegen unit**

### Runtime Tuning
- **Connection Reuse**: HTTP keep-alive is used by default.
- **Concurrency**: Managed by the tokio async runtime.

---

*This manual covers FustAPI v1.0.3 features.*
