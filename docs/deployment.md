# Deployment Guide

Production-ready deployment options for FustAPI.

## Table of Contents

- [Quick Start](#quick-start)
- [Desktop Install](#desktop-install)
- [Linux Server (systemd)](#linux-server-systemd)
- [Docker](#docker)
- [Reverse Proxy](#reverse-proxy)
- [Production Checklist](#production-checklist)

---

## Quick Start

### One-click (macOS / Linux)
```bash
curl -fsSL https://raw.githubusercontent.com/atlanssia/fustapi/main/install.sh | sh
```

### From Source
```bash
# Build and run locally
make build && ./target/release/fustapi serve

# Access the Web UI
open http://localhost:8800/ui
```

---

## Desktop Install

### macOS / Linux

```bash
make build
sudo make install          # installs to /usr/local/bin/fustapi
fustapi serve             # start the gateway
```

### Windows

```powershell
cargo build --release
copy target\release\fustapi.exe "C:\Program Files\fustapi\"
# Data lives at %APPDATA%\fustapi\fustapi.db
```

---

## Linux Server (systemd)

### 1. Create a dedicated user

```bash
sudo useradd -r -s /usr/sbin/nologin fustapi
sudo mkdir -p /home/fustapi/.fustapi
sudo chown fustapi:fustapi /home/fustapi/.fustapi
```

### 2. Write the service unit

```ini
# /etc/systemd/system/fustapi.service
[Unit]
Description=FustAPI LLM Gateway
After=network.target

[Service]
Type=simple
User=fustapi
Group=fustapi
ExecStart=/usr/local/bin/fustapi serve --host 127.0.0.1 --port 8800
Restart=always
RestartSec=3
Environment=RUST_LOG=info
Environment=HOME=/home/fustapi

# Security hardening
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=/home/fustapi/.fustapi

[Install]
WantedBy=multi-user.target
```

### 3. Enable and start

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now fustapi
sudo journalctl -u fustapi -f
```

---

## Docker

### Dockerfile

```dockerfile
FROM rust:1.85-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/fustapi /usr/local/bin/fustapi
RUN useradd -r fustapi
USER fustapi
WORKDIR /home/fustapi
EXPOSE 8800
ENTRYPOINT ["fustapi"]
CMD ["serve", "--host", "0.0.0.0", "--port", "8800"]
```

### Build and run

```bash
docker build -t fustapi:latest .
docker run -d --name fustapi -p 8800:8800 \
  -v fustapi-data:/home/fustapi/.fustapi fustapi:latest
```

### Docker Compose

```yaml
# docker-compose.yml
services:
  fustapi:
    build: .
    ports: ["8800:8800"]
    volumes: [fustapi-data:/home/fustapi/.fustapi]
    restart: unless-stopped
    environment: [RUST_LOG=info]
volumes:
  fustapi-data:
```

---

## Reverse Proxy

FustAPI binds to `127.0.0.1` by default — place it behind a reverse proxy for TLS and public access.

### Nginx

```nginx
server {
    listen 443 ssl http2;
    server_name api.example.com;

    ssl_certificate     /etc/letsencrypt/live/example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/example.com/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:8800;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        # Critical for streaming: disable buffering
        proxy_buffering off;
        proxy_cache off;
        chunked_transfer_encoding on;
        proxy_read_timeout 300s;
    }
}
```

### Caddy

```text
api.example.com {
    reverse_proxy 127.0.0.1:8800 {
        flush_interval -1    # flush immediately for streaming
    }
}
```

Caddy auto-provisions TLS via Let's Encrypt.

---

## Production Checklist

- [ ] Data directory permissions: `chmod 700 ~/.fustapi`
- [ ] API keys set for cloud providers via Web UI or CLI
- [ ] `RUST_LOG=info` (not `debug` or `trace`)
- [ ] Reverse proxy configured with TLS termination
- [ ] `proxy_buffering off` in reverse proxy (required for streaming)
- [ ] Service auto-restart enabled (systemd `Restart=always` or Docker `restart: unless-stopped`)
- [ ] Health check monitored: `curl http://localhost:8800/health` → `{"status":"ok"}`
- [ ] Backup strategy for `~/.fustapi/fustapi.db`
