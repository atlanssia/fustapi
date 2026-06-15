# OpenAI Responses API 支持实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 fustapi 透传网关新增 `POST /v1/responses` 入口，支持 OpenAI Responses API（双模式：纯透传 + 协议转换）。

**Architecture:** Responses 入口按 `supports_responses` 分流。上游支持 Responses（OpenAI 原生）→ 原始 body 字节透传（`Passthrough`/`NonStreaming`）。上游是 Chat Completions（DeepSeek 等）→ 协议转换，复用现有 Normalized 管线（请求 `parse_responses_request` 入，响应 `ResponsesStreamState` 出），与 Anthropic 序列化对称。前置修正候选 #4 的透传判定 bug。

**Tech Stack:** Rust 2024, axum, reqwest, tokio, serde_json, async-trait。测试：`cargo test`（src 内 `#[cfg(test)]` 单元测试 + `tests/api_tests.rs` 集成测试用 `tower::ServiceExt::oneshot`）。

**Spec:** `docs/superpowers/specs/2026-06-16-responses-api-design.md`

---

## File Structure

| 文件 | 责任 | 操作 |
|------|------|------|
| `src/protocol/mod.rs` | `Protocol` 枚举、`detect_protocol`、`dispatch_request`、`forward_streaming`、`responses_handler` | 修改 |
| `src/protocol/responses.rs` | Responses 请求解析（`input` → `UnifiedRequest`）+ 非流式响应序列化 | 新建 |
| `src/protocol/serializer.rs` | `ResponsesStreamState`（流式 `LLMChunk` → `response.*` 事件）| 修改 |
| `src/protocol/stream_dispatch.rs` | usage 提取协议感知（`input_tokens` vs `prompt_tokens`）| 修改 |
| `src/provider/mod.rs` | `ProviderCapabilities::supports_responses`、`Provider::responses_passthrough` 默认方法 | 修改 |
| `src/provider/cloud/openai.rs` | `OpenAIConfig::supports_responses`、`OpenAIProvider::responses_passthrough` 实现 | 修改 |
| `src/config.rs` | `ProviderConfig::supports_responses` 可选字段 | 修改 |
| `src/router/mod.rs` | （转换模式复用 `chat_stream`，无 Responses 专属逻辑）| 修改（分流在 handler）|
| `src/server.rs` | `POST /v1/responses` 路由 | 修改 |
| `tests/api_tests.rs` | 端到端集成测试 | 修改 |

---

## Task 1: 修正透传判定（候选 #4 前置 bug fix）

**Files:**
- Modify: `src/protocol/mod.rs`（`forward_streaming` 的 `allow_passthrough`）
- Test: `src/protocol/mod.rs`（单元测试）

候选 #4 把 `allow_passthrough` 改为无条件 `true`，导致 AM 入口透传到 CC-up 产出混合格式垃圾。恢复 protocol-aware 判定。

- [ ] **Step 1: 写失败测试**

在 `src/protocol/mod.rs` 的 `#[cfg(test)] mod tests` 内加（若已有 `MockRouter` 测试模式则复用，否则新增）：

```rust
// 捕获 allow_passthrough 参数的 mock router
struct PassthroughCaptureRouter {
    allow_passthrough: std::sync::Mutex<Option<bool>>,
}
#[async_trait::async_trait]
impl Router for PassthroughCaptureRouter {
    fn resolve(&self, _model: &str) -> Result<String, RouterError> { Ok("mock".into()) }
    fn resolve_upstream_model(&self, _model: &str) -> Option<String> { None }
    fn list_models(&self) -> Vec<String> { vec![] }
    fn list_providers(&self) -> Vec<String> { vec![] }
    async fn chat_stream(
        &self,
        _request: crate::provider::UnifiedRequest,
        allow_passthrough: bool,
    ) -> Result<crate::streaming::StreamMode, RouterError> {
        *self.allow_passthrough.lock().unwrap() = Some(allow_passthrough);
        // 返回空 Normalized 流，让 forward_streaming 立即结束
        Ok(crate::streaming::StreamMode::Normalized(Box::pin(
            tokio_stream::iter(vec![] as Vec<Result<crate::streaming::LLMChunk, crate::streaming::StreamError>>),
        )))
    }
}

#[tokio::test]
async fn forward_streaming_anthropic_forces_conversion_not_passthrough() {
    // AM 入口 → CC-up 必须转换，不能透传（候选 #4 回归）
    let router = PassthroughCaptureRouter { allow_passthrough: Default::default() };
    let req = crate::provider::UnifiedRequest {
        model: "m".into(), messages: vec![], tools: None, stream: true,
        temperature: None, max_tokens: None, extra_params: serde_json::Map::new(),
    };
    let tracker = crate::metrics::StreamTracker::for_test();
    let _ = forward_streaming(&router, req, "m", Protocol::Anthropic, tracker).await;
    assert_eq!(*router.allow_passthrough.lock().unwrap(), Some(false),
        "Anthropic entry must force conversion (allow_passthrough=false), not passthrough");
}

#[tokio::test]
async fn forward_streaming_openai_allows_passthrough() {
    let router = PassthroughCaptureRouter { allow_passthrough: Default::default() };
    let req = crate::provider::UnifiedRequest {
        model: "m".into(), messages: vec![], tools: None, stream: true,
        temperature: None, max_tokens: None, extra_params: serde_json::Map::new(),
    };
    let tracker = crate::metrics::StreamTracker::for_test();
    let _ = forward_streaming(&router, req, "m", Protocol::OpenAI, tracker).await;
    assert_eq!(*router.allow_passthrough.lock().unwrap(), Some(true));
}
```

注：若 `StreamTracker` 无 `for_test()` 构造，参考 `src/protocol/mod.rs` 现有测试如何构造 tracker，沿用同一模式。

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test forward_streaming_ --lib`
Expected: FAIL — `Some(true) != Some(false)`（当前 `allow_passthrough = true` 无条件）

- [ ] **Step 3: 最小实现**

`src/protocol/mod.rs` `forward_streaming`：

```rust
// Before:
let allow_passthrough = true;
// After:
let allow_passthrough = protocol == Protocol::OpenAI;
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test forward_streaming_ --lib && cargo test`
Expected: PASS（含全量回归）

- [ ] **Step 5: Commit**

```bash
git add src/protocol/mod.rs
git commit -m "fix: restore protocol-aware passthrough (candidate #4 regression)

AM entry to CC-up upstream must convert, not passthrough — otherwise
wrap_anthropic produces mixed-format garbage (message_start + OpenAI
chunk + message_stop). Passthrough requires format match."
```

---

## Task 2: Protocol::Responses + detect_protocol

**Files:**
- Modify: `src/protocol/mod.rs`
- Test: `src/protocol/mod.rs`

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn detect_protocol_responses_path() {
    let headers = axum::http::HeaderMap::new();
    assert_eq!(detect_protocol("/v1/responses", &headers), Protocol::Responses);
}

#[test]
fn detect_protocol_responses_does_not_clobber_others() {
    let headers = axum::http::HeaderMap::new();
    assert_eq!(detect_protocol("/v1/chat/completions", &headers), Protocol::OpenAI);
    assert_eq!(detect_protocol("/v1/messages", &headers), Protocol::Anthropic);
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test detect_protocol_responses --lib`
Expected: FAIL — `Protocol::Responses` 不存在（编译错误）

- [ ] **Step 3: 实现**

`Protocol` 枚举加变体；`detect_protocol` 加分支：

```rust
pub enum Protocol {
    OpenAI,
    Anthropic,
    Responses,
}

pub fn detect_protocol(path: &str, headers: &axum::http::HeaderMap) -> Protocol {
    if path.starts_with("/v1/responses") {
        Protocol::Responses
    } else if path.starts_with("/v1/messages") || headers.get("anthropic-version").is_some() {
        Protocol::Anthropic
    } else {
        Protocol::OpenAI
    }
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test detect_protocol --lib`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/protocol/mod.rs
git commit -m "feat(protocol): add Protocol::Responses variant and detection"
```

---

## Task 3: ProviderCapabilities::supports_responses + config 覆盖

**Files:**
- Modify: `src/provider/mod.rs`（`ProviderCapabilities` + `create_provider`）
- Modify: `src/provider/cloud/openai.rs`（`OpenAIConfig` + `Default`）
- Modify: `src/config.rs`（`ProviderConfig`）
- Test: `src/provider/mod.rs`

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn openai_provider_capabilities_supports_responses_by_default() {
    let p = crate::provider::cloud::openai::OpenAIProvider::new(
        crate::provider::cloud::openai::OpenAIConfig::default(),
    );
    assert!(p.capabilities().supports_responses);
}

#[test]
fn deepseek_capabilities_do_not_support_responses() {
    use crate::config::ProviderConfig;
    let cfg = ProviderConfig {
        endpoint: "http://localhost/v1".into(), api_key: None, model: None,
        r#type: "deepseek".into(), supports_responses: None,
    };
    let p = create_provider("ds", &cfg);
    assert!(!p.capabilities().supports_responses);
}

#[test]
fn config_override_enables_responses_on_compatible() {
    use crate::config::ProviderConfig;
    let cfg = ProviderConfig {
        endpoint: "http://localhost/v1".into(), api_key: None, model: None,
        r#type: "openai_compatible".into(), supports_responses: Some(true),
    };
    let p = create_provider("proxy", &cfg);
    assert!(p.capabilities().supports_responses);
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test supports_responses --lib`
Expected: FAIL — `supports_responses` 字段不存在（编译错误）

- [ ] **Step 3a: ProviderCapabilities 加字段**

`src/provider/mod.rs`：

```rust
pub struct ProviderCapabilities {
    pub tool_calling: ToolCallingSupport,
    pub image_input: bool,
    pub streaming: bool,
    pub supports_responses: bool,
}
```

修复所有 `ProviderCapabilities { ... }` 字面构造（router 测试、openai.rs capabilities() 等）加 `supports_responses: false`（除 OpenAIProvider 加 `supports_responses: self.config.supports_responses`）。

- [ ] **Step 3b: OpenAIConfig 加字段**

`src/provider/cloud/openai.rs`：

```rust
pub struct OpenAIConfig {
    // ... existing fields ...
    pub balance_strategy: BalanceStrategy,
    /// Whether upstream supports the Responses API (/v1/responses).
    pub supports_responses: bool,
}
```

`Default`：`supports_responses: true`（默认 OpenAI 端点）。

`capabilities()`：
```rust
fn capabilities(&self) -> crate::provider::ProviderCapabilities {
    crate::provider::ProviderCapabilities {
        tool_calling: self.config.tool_calling,
        image_input: self.config.image_input,
        streaming: self.config.streaming,
        supports_responses: self.config.supports_responses,
    }
}
```

- [ ] **Step 3c: create_provider 按 type 设置**

`src/provider/mod.rs` `create_provider`：`Pt::OpenAI | Pt::OpenAICompatible` 分支设 `supports_responses: cfg.supports_responses.unwrap_or(pt == Pt::OpenAI)`（OpenAI 默认 true，Compatible 默认 false，可被 config 覆盖）。其余分支 `supports_responses: false`。

- [ ] **Step 3d: ProviderConfig 加可选字段**

`src/config.rs`：

```rust
pub struct ProviderConfig {
    pub endpoint: String,
    pub api_key: Option<String>,
    pub model: Option<String>,
    #[serde(default = "default_type")]
    pub r#type: String,
    /// Override: declare upstream supports Responses API (/v1/responses).
    /// Defaults inferred from type (OpenAI=true, others=false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_responses: Option<bool>,
}
```

修复所有 `ProviderConfig { ... }` 构造（config.rs 内多处 default_config）加 `supports_responses: None`。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test supports_responses --lib && cargo build`
Expected: PASS，编译无错

- [ ] **Step 5: Commit**

```bash
git add src/provider/mod.rs src/provider/cloud/openai.rs src/config.rs
git commit -m "feat(provider): add supports_responses capability + config override"
```

---

## Task 4: Provider::responses_passthrough + OpenAIProvider 实现

**Files:**
- Modify: `src/provider/mod.rs`（trait 默认方法）
- Modify: `src/provider/cloud/openai.rs`（实现）
- Test: `src/provider/cloud/openai.rs`

透传模式核心：原始 body 转发到 `{endpoint}/responses`，按 `stream` 返回 `Passthrough`（流式字节）或 `NonStreaming`（完整 JSON）。

- [ ] **Step 1: 写失败测试**

`src/provider/cloud/openai.rs` 单元测试。因涉及网络，用 `mockito`（已在依赖？检查 Cargo.toml；若无，用 `wiremock` 或跳过实网测试，改为验证 URL 构造的纯函数测试）。若无 mock 依赖，改为构造一个返回固定 SSE 的本地 server（参考现有测试是否有 mock 模式）。

简化：先测 URL 与 body 透传构造（纯函数），网络行为靠 Task 5 集成测试覆盖。

```rust
#[test]
fn responses_passthrough_url_is_endpoint_slash_responses() {
    // 验证透传目标 URL 构造：{endpoint}/responses
    let cfg = OpenAIConfig { endpoint: "http://localhost:11434/v1".into(), ..Default::default() };
    let p = OpenAIProvider::new(cfg);
    // responses_target_url 是新增的 pub(crate) 辅助函数
    assert_eq!(p.responses_target_url(), "http://localhost:11434/v1/responses");
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test responses_passthrough_url --lib`
Expected: FAIL — `responses_target_url` 方法不存在

- [ ] **Step 3a: trait 默认方法**

`src/provider/mod.rs` `Provider` trait 加：

```rust
/// Forward a raw Responses API request body to an upstream that supports
/// the Responses API. Returns Passthrough (streaming) or NonStreaming.
/// Default: unsupported. Override in providers that speak Responses.
async fn responses_passthrough(
    &self,
    _body: String,
    _stream: bool,
) -> Result<StreamMode, ProviderError> {
    Err(ProviderError::Internal("responses_passthrough not supported".into()))
}
```

- [ ] **Step 3b: OpenAIProvider 实现 + URL 辅助**

`src/provider/cloud/openai.rs`：

```rust
impl OpenAIProvider {
    /// Target URL for Responses API: `{endpoint}/responses`.
    pub(crate) fn responses_target_url(&self) -> String {
        format!("{}/responses", self.config.endpoint)
    }
}

#[async_trait::async_trait]
impl Provider for OpenAIProvider {
    // ... existing chat_stream, capabilities, name ...

    async fn responses_passthrough(
        &self,
        body: String,
        stream: bool,
    ) -> Result<StreamMode, ProviderError> {
        let url = self.responses_target_url();
        let mut builder = self.client.post(&url);
        if !self.config.api_key.is_empty() {
            builder = builder.header("Authorization", format!("Bearer {}", self.config.api_key));
        }
        // 原始 body 透传，不重新序列化
        let builder = builder.header("Content-Type", "application/json").body(body);
        let resp = send_with_tcp_retry(builder).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            return Err(if status.as_u16() >= 400 && status.as_u16() < 500 {
                ProviderError::Upstream { status: status.as_u16(), message: err_text }
            } else {
                ProviderError::Request(format!("provider error {status}: {err_text}"))
            });
        }

        if stream {
            let byte_stream = futures::StreamExt::map(resp.bytes_stream(), |res| {
                res.map_err(|e| crate::streaming::StreamError::Connection(e.to_string()))
            });
            Ok(StreamMode::Passthrough(Box::pin(byte_stream)))
        } else {
            let full = resp.bytes().await
                .map_err(|e| ProviderError::Internal(e.to_string()))?;
            let json: serde_json::Value = serde_json::from_slice(&full)
                .map_err(|e| ProviderError::Internal(e.to_string()))?;
            Ok(StreamMode::NonStreaming(json))
        }
    }
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test responses_passthrough_url --lib && cargo build`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/provider/mod.rs src/provider/cloud/openai.rs
git commit -m "feat(provider): add responses_passthrough for byte-level forwarding"
```

---

## Task 5: /v1/responses 路由 + responses_handler（透传模式）

**Files:**
- Modify: `src/server.rs`（路由）
- Modify: `src/protocol/mod.rs`（`responses_handler` + `dispatch_request` 分支）
- Test: `tests/api_tests.rs`

先只接透传模式（supports_responses=true）。转换模式在 Task 9 接入。

- [ ] **Step 1: 写失败测试**

`tests/api_tests.rs`：

```rust
#[tokio::test]
async fn responses_endpoint_exists() {
    // /v1/responses 路由存在（即使无 provider 配置，也应返回非 404 的协议错误）
    let req = json_request("POST", "/v1/responses", json!({"model":"x","input":"hi"}));
    let (status, _body) = oneshot(req).await;
    assert_ne!(status, StatusCode::NOT_FOUND, "/v1/responses must be routed");
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test responses_endpoint_exists --test api_tests`
Expected: FAIL — 404（路由未注册）

- [ ] **Step 3a: 路由**

`src/server.rs` `build_app`，在 `/v1/messages` 路由后加：

```rust
.route(
    "/v1/responses",
    post({
        let router = router_store.clone();
        let emitter = metrics_emitter.clone();
        move |headers, body| responses_handler(headers, body, router, emitter)
    }),
)
```

并加 `responses_handler`（仿 `chat_completions_handler`）：

```rust
async fn responses_handler(
    headers: axum::http::HeaderMap,
    body: String,
    router: RouterStore,
    emitter: MetricsEmitter,
) -> impl IntoResponse {
    let proto = protocol::detect_protocol("/v1/responses", &headers);
    let current_router = router.load_full();
    let (provider_name, model_name) = resolve_provider_and_model(&body, current_router.as_ref());
    let guard = metrics::guard::RequestGuard::start(emitter, &provider_name, &model_name);
    match protocol::dispatch_request(proto, body, current_router.as_ref(), guard).await {
        Ok(response) => response,
        Err(e) => e.into_response(),
    }
}
```

- [ ] **Step 3b: dispatch + handler（仅透传模式）**

`src/protocol/mod.rs` `dispatch_request` 加：

```rust
Protocol::Responses => responses_handler(body, router, guard).await,
```

新增 `responses_handler`（透传模式，转换模式 Task 9 补）：

```rust
async fn responses_handler(
    body: String,
    router: &dyn Router,
    mut guard: crate::metrics::guard::RequestGuard,
) -> Result<Response, ProtocolError> {
    let protocol = Protocol::Responses;
    // 提取 model（最小解析，复用 ModelField 模式）
    let model = extract_model_field(&body);
    if let Some(upstream) = router.resolve_upstream_model(&model) {
        guard.set_model(upstream);
    }

    // 解析 provider
    let provider_name = router.resolve(&model)
        .map_err(|e| { guard.finish_err(); map_router_error(e, protocol) })?;
    // router 需暴露 get_provider —— 见下方说明
    let provider = router.get_provider(&provider_name)
        .ok_or_else(|| { guard.finish_err(); ProtocolError::Internal { message: "provider not found".into(), protocol } })?;

    let caps = provider.capabilities();
    let stream = extract_stream_field(&body);

    if caps.supports_responses {
        // 透传模式
        let stream_mode = provider.responses_passthrough(body, stream).await
            .map_err(|e| { guard.finish_err(); map_router_error(e.into(), protocol) })?;
        // 流式：Passthrough 走 forward_streaming 风格的 SSE 响应；
        // 非流式：NonStreaming 直接返回 JSON
        dispatch_responses_stream_mode(stream_mode, stream, guard, protocol)
    } else {
        // 转换模式 —— Task 9 实现
        Err(ProtocolError::Internal { message: "conversion mode not yet implemented".into(), protocol })
    }
}
```

需要的辅助：`extract_model_field`（解析 `{"model": "..."}`，可复用 server.rs 的 `ModelField` 逻辑，提到 protocol 层或公开）、`extract_stream_field`（解析 `stream` bool）、`router.get_provider(&name) -> Option<&dyn Provider>`。

`RealRouter` 加 `pub fn get_provider(&self, name: &str) -> Option<&dyn Provider>`（暴露现有 `providers` map）。`Router` trait 可不加（handler 用 `RealRouter` 具体类型，或 trait 加方法）。

> 注：此步引入 `ProviderError -> RouterError` 转换需求。`responses_passthrough` 返回 `ProviderError`，handler 里 `map_router_error` 接收 `RouterError`。加 `From<ProviderError> for RouterError` 已存在（router/mod.rs）。用 `.map_err(RouterError::from)` 或直接让 `responses_passthrough` 在 trait 层返回 `ProviderError` 后转。简化：handler 内 `map_router_error(RouterError::from(e), protocol)`。

- [ ] **Step 3c: dispatch_responses_stream_mode**

```rust
fn dispatch_responses_stream_mode(
    stream_mode: crate::streaming::StreamMode,
    stream: bool,
    mut guard: crate::metrics::guard::RequestGuard,
    protocol: Protocol,
) -> Result<Response, ProtocolError> {
    use crate::streaming::StreamMode;
    match stream_mode {
        StreamMode::NonStreaming(json) => {
            // 提取 usage（input_tokens/output_tokens）做计量
            if let Some(usage) = json.get("usage") {
                let u = extract_responses_usage(usage);
                guard.finish(true, Some(u), Some(guard.elapsed_ms()));
            } else {
                guard.finish(true, None, Some(guard.elapsed_ms()));
            }
            Ok((StatusCode::OK, Json(json)).into_response())
        }
        StreamMode::Passthrough(bytes) => {
            let tracker = guard.into_tracker();
            // 复用 stream_dispatch 的 passthrough SSE 响应
            Ok(stream_dispatch::forward_as_sse_response(
                StreamMode::Passthrough(bytes), protocol, "", tracker,
            ))
        }
        StreamMode::Normalized(_) => Err(ProtocolError::Internal {
            message: "passthrough returned Normalized".into(), protocol,
        }),
    }
}
```

`extract_responses_usage(usage: &Value) -> TokenUsage`：读 `input_tokens`/`output_tokens`。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test responses_endpoint_exists --test api_tests && cargo build`
Expected: PASS，编译通过

- [ ] **Step 5: Commit**

```bash
git add src/server.rs src/protocol/mod.rs src/router/mod.rs
git commit -m "feat(responses): add /v1/responses route + passthrough mode handler"
```

---

## Task 6: parse_responses_request（请求转换）

**Files:**
- Create: `src/protocol/responses.rs`
- Modify: `src/protocol/mod.rs`（`pub mod responses;`）
- Test: `src/protocol/responses.rs`

`input`（字符串或 items 数组）+ `instructions` → `UnifiedRequest.messages`；function tools → `request.tools`。内置工具 / `previous_response_id` / `store:true` → 错误（Task 9 在 handler 层校验，这里 parse 不报错但提取标志）。

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn parse_string_input_to_user_message() {
    let body = r#"{"model":"m","input":"hello"}"#;
    let req = parse_responses_request(body).unwrap();
    assert_eq!(req.model, "m");
    assert_eq!(req.messages.len(), 1);
    assert_eq!(req.messages[0].role, crate::provider::Role::User);
    assert_eq!(req.messages[0].content, "hello");
}

#[test]
fn parse_instructions_to_system_message() {
    let body = r#"{"model":"m","input":"hi","instructions":"be brief"}"#;
    let req = parse_responses_request(body).unwrap();
    let sys = req.messages.iter().find(|m| m.role == crate::provider::Role::System).unwrap();
    assert_eq!(sys.content, "be brief");
}

#[test]
fn parse_input_array_items_to_messages() {
    let body = r#"{"model":"m","input":[
        {"role":"user","content":[{"type":"input_text","text":"q"}]},
        {"role":"assistant","content":[{"type":"output_text","text":"a"}]}
    ]}"#;
    let req = parse_responses_request(body).unwrap();
    assert_eq!(req.messages.iter().filter(|m| m.role == crate::provider::Role::User).count(), 1);
}

#[test]
fn parse_function_tools() {
    let body = r#"{"model":"m","input":"x","tools":[
        {"type":"function","name":"f","description":"d","parameters":{"type":"object"}}
    ]}"#;
    let req = parse_responses_request(body).unwrap();
    assert!(req.tools.is_some());
    assert_eq!(req.tools.unwrap().len(), 1);
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test parse_ --lib`
Expected: FAIL — 模块/函数不存在

- [ ] **Step 3: 实现**

`src/protocol/responses.rs`：

```rust
//! Responses API request parsing: input/instructions → UnifiedRequest.

use crate::capability::ToolDefinition;
use crate::provider::{Message, Role, UnifiedRequest};

#[derive(serde::Deserialize)]
struct ResponsesRequest {
    model: String,
    #[serde(default)]
    input: serde_json::Value, // string or array of items
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    tools: Vec<serde_json::Value>,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

/// Parse a Responses API request body into a UnifiedRequest.
pub fn parse_responses_request(body: &str) -> Result<UnifiedRequest, String> {
    let req: ResponsesRequest = serde_json::from_str(body).map_err(|e| e.to_string())?;

    let mut messages = Vec::new();
    if let Some(sys) = req.instructions {
        messages.push(Message { role: Role::System, content: sys, images: None,
            tool_calls: None, tool_call_id: None, extras: None });
    }

    match req.input {
        serde_json::Value::String(s) => {
            messages.push(Message { role: Role::User, content: s, images: None,
                tool_calls: None, tool_call_id: None, extras: None });
        }
        serde_json::Value::Array(items) => {
            for item in items {
                if let Ok(m) = parse_input_item(&item) { messages.push(m); }
            }
        }
        _ => {}
    }

    // function tools only; built-in tools filtered (caller validates/rejects)
    let tools: Vec<ToolDefinition> = req.tools.iter()
        .filter_map(|t| {
            if t.get("type").and_then(|v| v.as_str()) == Some("function") {
                Some(ToolDefinition {
                    name: t.get("name")?.as_str()?.to_string(),
                    description: t.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    parameters: t.get("parameters").cloned().unwrap_or(serde_json::json!({"type":"object"})),
                })
            } else { None }
        }).collect();
    let tools = if tools.is_empty() { None } else { Some(tools) };

    Ok(UnifiedRequest {
        model: req.model, messages, tools, stream: req.stream,
        temperature: req.extra.get("temperature").and_then(|v| v.as_f64()).map(|f| f as f32),
        max_tokens: req.extra.get("max_output_tokens").and_then(|v| v.as_u64()).map(|u| u as u32),
        extra_params: req.extra,
    })
}

fn parse_input_item(item: &serde_json::Value) -> Result<Message, String> {
    let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("user");
    let role = match role {
        "assistant" => Role::Assistant,
        "system" => Role::System,
        _ => Role::User,
    };
    // content: string or array of {type, text}
    let content = match item.get("content") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => parts.iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str())).collect::<Vec<_>>().join(""),
        _ => String::new(),
    };
    Ok(Message { role, content, images: None, tool_calls: None, tool_call_id: None, extras: None })
}
```

`src/protocol/mod.rs` 加 `pub mod responses;`。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test parse_ --lib`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/protocol/responses.rs src/protocol/mod.rs
git commit -m "feat(responses): parse_responses_request (input → UnifiedRequest)"
```

---

## Task 7: serialize_responses_response（非流式响应转换）

**Files:**
- Modify: `src/protocol/responses.rs`
- Test: `src/protocol/responses.rs`

Chat Completions 非流式响应（`choices[].message`）→ Responses `output[]` JSON。

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn serialize_non_stream_text_response() {
    let cc = serde_json::json!({
        "id":"cc-1","model":"m","choices":[{"index":0,
            "message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],
        "usage":{"prompt_tokens":3,"completion_tokens":2}
    });
    let out = serialize_responses_response(&cc, "m").unwrap();
    assert_eq!(out["object"], "response");
    assert_eq!(out["output"][0]["type"], "message");
    assert_eq!(out["output"][0]["content"][0]["type"], "output_text");
    assert_eq!(out["output"][0]["content"][0]["text"], "hi");
    assert_eq!(out["usage"]["input_tokens"], 3);
    assert_eq!(out["usage"]["output_tokens"], 2);
}

#[test]
fn serialize_non_stream_reasoning_and_tool_call() {
    let cc = serde_json::json!({
        "id":"cc-1","model":"m","choices":[{"index":0,"message":{
            "role":"assistant","content":"ans",
            "reasoning_content":"thinking",
            "tool_calls":[{"id":"tc1","type":"function","function":{"name":"f","arguments":"{}"}}]
        },"finish_reason":"tool_calls"}],
        "usage":{"prompt_tokens":1,"completion_tokens":1}
    });
    let out = serialize_responses_response(&cc, "m").unwrap();
    let types: Vec<&str> = out["output"].as_array().unwrap().iter()
        .map(|o| o["type"].as_str().unwrap()).collect();
    assert!(types.contains(&"reasoning"));
    assert!(types.contains(&"function_call"));
    assert!(types.contains(&"message"));
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test serialize_responses_response --lib`
Expected: FAIL — 函数不存在

- [ ] **Step 3: 实现**

`src/protocol/responses.rs` 加：

```rust
/// Convert a Chat Completions non-streaming JSON response into Responses format.
pub fn serialize_responses_response(cc: &serde_json::Value, model: &str) -> Result<serde_json::Value, String> {
    let msg = cc.get("choices").and_then(|c| c.as_array()).and_then(|a| a.first())
        .and_then(|c| c.get("message"));
    let usage = cc.get("usage");
    let prompt = usage.and_then(|u| u.get("prompt_tokens")).and_then(|v| v.as_u64()).unwrap_or(0);
    let completion = usage.and_then(|u| u.get("completion_tokens")).and_then(|v| v.as_u64()).unwrap_or(0);

    let mut output: Vec<serde_json::Value> = Vec::new();

    // reasoning item (if reasoning_content present)
    if let Some(rc) = msg.and_then(|m| m.get("reasoning_content")).and_then(|v| v.as_str()) {
        if !rc.is_empty() {
            output.push(serde_json::json!({
                "type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":rc}]
            }));
        }
    }
    // message item with output_text
    if let Some(content) = msg.and_then(|m| m.get("content")).and_then(|v| v.as_str()) {
        output.push(serde_json::json!({
            "type":"message","id":"msg_1","role":"assistant",
            "content":[{"type":"output_text","text":content,"annotations":[]}],
            "status":"completed"
        }));
    }
    // function_call items
    if let Some(tcs) = msg.and_then(|m| m.get("tool_calls")).and_then(|v| v.as_array()) {
        for tc in tcs {
            let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("fc_1");
            let name = tc.get("function").and_then(|f| f.get("name")).and_then(|v| v.as_str()).unwrap_or("");
            let args = tc.get("function").and_then(|f| f.get("arguments")).and_then(|v| v.as_str()).unwrap_or("{}");
            output.push(serde_json::json!({
                "type":"function_call","id":id,"call_id":id,"name":name,"arguments":args,"status":"completed"
            }));
        }
    }

    let finish = cc.get("choices").and_then(|c| c.as_array()).and_then(|a| a.first())
        .and_then(|c| c.get("finish_reason")).and_then(|v| v.as_str()).unwrap_or("stop");
    let status = match finish { "length" => "incomplete", _ => "completed" };

    Ok(serde_json::json!({
        "id": format!("resp_{}", cc.get("id").and_then(|v| v.as_str()).unwrap_or("gw")),
        "object": "response",
        "created_at": 0,
        "status": status,
        "model": model,
        "output": output,
        "usage": {
            "input_tokens": prompt, "output_tokens": completion,
            "total_tokens": prompt + completion
        }
    }))
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test serialize_responses_response --lib`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/protocol/responses.rs
git commit -m "feat(responses): serialize_responses_response (choices → output[])"
```

---

## Task 8: ResponsesStreamState（流式响应转换）

**Files:**
- Modify: `src/protocol/serializer.rs`
- Test: `src/protocol/serializer.rs`

`LLMChunk` → `response.*` SSE 事件，对标 `AnthropicStreamState`。

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn responses_stream_emits_created_then_text_delta() {
    let mut s = ResponsesStreamState::new();
    let c1 = LLMChunk { content: Some("he".into()), ..Default::default() };
    let c2 = LLMChunk { content: Some("llo".into()), done: true,
        usage: Some(crate::metrics::TokenUsage{prompt_tokens:1,completion_tokens:1}), ..Default::default() };
    let out1 = s.serialize_chunk(&c1, "m");
    assert!(out1.contains("response.created"));
    assert!(out1.contains("response.output_item.added"));
    assert!(out1.contains("response.output_text.delta"));
    let out2 = s.serialize_chunk(&c2, "m");
    assert!(out2.contains("response.output_text.delta"));
    assert!(out2.contains("response.completed"));
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test responses_stream --lib`
Expected: FAIL — `ResponsesStreamState` 不存在

- [ ] **Step 3: 实现**

`src/protocol/serializer.rs`，对标 `AnthropicStreamState` 结构（参考其 `serialize_chunk` 的 block 切换逻辑）：

```rust
pub struct ResponsesStreamState {
    started: bool,
    message_open: bool,
    text_part_open: bool,
    reasoning_open: bool,
    usage: Option<crate::metrics::TokenUsage>,
    response_id: String,
}

impl ResponsesStreamState {
    pub fn new() -> Self { Self { started:false, message_open:false, text_part_open:false,
        reasoning_open:false, usage:None, response_id: format!("resp_{}", id_short()) } }

    pub fn serialize_chunk(&mut self, chunk: &LLMChunk, model: &str) -> String {
        let mut events = String::new();
        if !self.started {
            self.started = true;
            events.push_str(&sse("response.created", &serde_json::json!({
                "type":"response.created","response": response_skeleton(&self.response_id, model, "in_progress")
            })));
        }
        // reasoning item
        if let Some(rc) = &chunk.reasoning_content {
            if !rc.is_empty() {
                if self.text_part_open { events.push_str(&sse("response.output_text.done",&json!({"type":"response.output_text.done","text":""}))); self.text_part_open=false; }
                if self.message_open { events.push_str(&close_message()); self.message_open=false; }
                if !self.reasoning_open {
                    self.reasoning_open = true;
                    events.push_str(&sse("response.output_item.added",&json!({"type":"response.output_item.added","item":{"type":"reasoning","id":"rs_1","summary":[]}})));
                }
                events.push_str(&sse("response.reasoning_summary_text.delta",&json!({"type":"response.reasoning_summary_text.delta","delta":rc})));
            }
        }
        // text content
        if let Some(text) = &chunk.content {
            if !text.is_empty() {
                if self.reasoning_open { events.push_str(&close_item("rs_1","reasoning")); self.reasoning_open=false; }
                if !self.message_open {
                    self.message_open = true;
                    events.push_str(&sse("response.output_item.added",&json!({"type":"response.output_item.added","item":{"type":"message","id":"msg_1","role":"assistant","status":"in_progress","content":[]}})));
                }
                if !self.text_part_open {
                    self.text_part_open = true;
                    events.push_str(&sse("response.content_part.added",&json!({"type":"response.content_part.added","item_id":"msg_1","part":{"type":"output_text","text":"","annotations":[]}})));
                }
                events.push_str(&sse("response.output_text.delta",&json!({"type":"response.output_text.delta","item_id":"msg_1","delta":text})));
            }
        }
        // tool_call (function_call item) — simplified: open item + arguments delta
        if let Some(tc) = &chunk.tool_call {
            if self.text_part_open { events.push_str(&sse("response.output_text.done",&json!({"type":"response.output_text.done","item_id":"msg_1","text":""}))); self.text_part_open=false; }
            if self.message_open { events.push_str(&close_message()); self.message_open=false; }
            let id = tc.id.clone().unwrap_or_else(|| "fc_1".into());
            events.push_str(&sse("response.output_item.added",&json!({"type":"response.output_item.added","item":{"type":"function_call","id":&id,"call_id":&id,"name":&tc.name,"arguments":""}})));
            events.push_str(&sse("response.function_call_arguments.delta",&json!({"type":"response.function_call_arguments.delta","item_id":&id,"delta":tc.arguments.to_string()})));
            events.push_str(&sse("response.function_call_arguments.done",&json!({"type":"response.function_call_arguments.done","item_id":&id,"arguments":tc.arguments.to_string()})));
            events.push_str(&close_item(&id,"function_call"));
        }
        if chunk.done {
            if self.text_part_open { events.push_str(&sse("response.output_text.done",&json!({"type":"response.output_text.done","item_id":"msg_1","text":""}))); self.text_part_open=false; }
            if self.message_open { events.push_str(&close_message()); self.message_open=false; }
            if let Some(u) = &chunk.usage { self.usage = Some(u.clone()); }
            let (inp,outp) = self.usage.as_ref().map(|u|(u.prompt_tokens,u.completion_tokens)).unwrap_or((0,0));
            events.push_str(&sse("response.completed",&serde_json::json!({
                "type":"response.completed","response": response_skeleton(&self.response_id, model, "completed"),
                "usage":{"input_tokens":inp,"output_tokens":outp,"total_tokens":inp+outp}
            })));
        }
        events
    }
}
```

辅助 `sse(event, json) -> String`（格式 `event: {event}\ndata: {json}\n\n`）、`response_skeleton`、`close_message`、`close_item`、`id_short` 在同文件实现（简单字符串拼接，参考现有 `serialize_openai_chunk` 的 SSE 格式）。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test responses_stream --lib`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/protocol/serializer.rs
git commit -m "feat(responses): ResponsesStreamState (LLMChunk → response.* events)"
```

---

## Task 9: 转换模式接入 + 边界校验

**Files:**
- Modify: `src/protocol/mod.rs`（`responses_handler` 转换分支 + 边界校验）
- Test: `tests/api_tests.rs`

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test]
async fn responses_previous_response_id_rejected() {
    let req = json_request("POST","/v1/responses",
        json!({"model":"x","input":"hi","previous_response_id":"resp_abc"}));
    let (status, body) = oneshot(req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("previous_response_id") || body.contains("input"));
}

#[tokio::test]
async fn responses_builtin_tool_rejected() {
    let req = json_request("POST","/v1/responses",
        json!({"model":"x","input":"hi","tools":[{"type":"web_search_preview"}]}));
    let (status, _body) = oneshot(req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test responses_previous_response_id_rejected --test api_tests`
Expected: FAIL — 当前转换分支返回 500（"not yet implemented"）

- [ ] **Step 3: 实现**

`src/protocol/mod.rs` `responses_handler` 的转换分支替换 Task 5 的占位：

```rust
// 边界校验（无状态原则）
if has_previous_response_id(&body) || has_store_true(&body) {
    guard.finish_err();
    return Err(ProtocolError::Parse {
        message: "previous_response_id / store not supported in conversion mode; send full input array".into(),
        protocol,
    });
}
if has_builtin_tools(&body) {
    guard.finish_err();
    return Err(ProtocolError::Parse {
        message: "built-in tools (web_search/file_search/computer_use) not supported by upstream".into(),
        protocol,
    });
}

// 转换：parse → router.chat_stream (Normalized) → Responses 序列化
let mut unified = responses::parse_responses_request(&body)
    .map_err(|e| { guard.finish_err(); ProtocolError::Parse { message: e, protocol } })?;
if let Some(upstream) = router.resolve_upstream_model(&model) { unified.model = upstream; }

let stream_mode = router.chat_stream(unified, false).await  // 强制 Normalized（转换）
    .map_err(|e| { guard.finish_err(); map_router_error(e, protocol) })?;

dispatch_responses_conversion(stream_mode, &model, guard, protocol)
```

辅助：`has_previous_response_id`（解析 `previous_response_id` 非空）、`has_store_true`（`store == true`）、`has_builtin_tools`（tools 含非 function 类型）。用 `serde_json::from_str::<serde_json::Value>` 检查。

`dispatch_responses_conversion`：`Normalized` 流 → 用 `ResponsesStreamState` 序列化为 SSE 响应（流式）；`NonStreaming` → `serialize_responses_response` 转 JSON（非流式）。参考 `forward_streaming` 的 Normalized 分支结构，把 `AnthropicStreamState` 换成 `ResponsesStreamState`、`wrap_anthropic` 不需要（Responses 无外层包装）。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test responses_ --test api_tests && cargo test`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/protocol/mod.rs
git commit -m "feat(responses): conversion mode + boundary validation (400s)"
```

---

## Task 10: usage 协议感知提取 + 全量验证

**Files:**
- Modify: `src/protocol/stream_dispatch.rs`（`extract_usage_from_sse_bytes` 协议感知）或 `serializer.rs`
- Modify: `src/protocol/mod.rs`（`extract_responses_usage` 已在 Task 5 加，此处确认覆盖流式）
- Test: `src/protocol/stream_dispatch.rs` / `serializer.rs`

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn extract_usage_handles_responses_field_names() {
    let sse = b"data: {\"type\":\"response.completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":7,\"total_tokens\":12}}\n\n";
    let u = extract_usage_from_sse_bytes(sse, Protocol::Responses);
    assert_eq!(u.prompt_tokens, 5);
    assert_eq!(u.completion_tokens, 7);
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test extract_usage_handles_responses --lib`
Expected: FAIL — `extract_usage_from_sse_bytes` 不接受 Protocol 参数 / 不识别 input_tokens

- [ ] **Step 3: 实现**

`serializer.rs` 的 `extract_usage_from_sse_bytes` 加 `protocol` 参数；按协议读字段：

```rust
pub fn extract_usage_from_sse_bytes(buf: &[u8], protocol: super::Protocol) -> Option<crate::metrics::TokenUsage> {
    let txt = std::str::from_utf8(buf).ok()?;
    let (pfield, cfield) = match protocol {
        super::Protocol::Responses => ("input_tokens", "output_tokens"),
        _ => ("prompt_tokens", "completion_tokens"),
    };
    // 扫描最后一个 usage 块（现有逻辑 + 字段名分支）
    // ... 现有 JSON 扫描，改用 pfield/cfield ...
}
```

更新所有调用点（`stream_dispatch.rs` passthrough）传 `protocol`。

- [ ] **Step 4: 全量验证**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: 全 PASS，0 warning

- [ ] **Step 5: Commit + tag**

```bash
git add src/protocol/serializer.rs src/protocol/stream_dispatch.rs
git commit -m "feat(responses): protocol-aware usage extraction"
```

---

## Self-Review

**Spec coverage:**
- ✅ 双模式分流（supports_responses）→ Task 3, 5, 9
- ✅ 纯透传（Passthrough/NonStreaming）→ Task 4, 5
- ✅ 协议转换（复用 Normalized）→ Task 6, 7, 8, 9
- ✅ 协议矩阵 / 候选 #4 前置修正 → Task 1
- ✅ 转换边界（无状态 / 内置工具 / function / reasoning）→ Task 9（+ Task 6/7/8 映射）
- ✅ usage 协议感知 → Task 10
- ✅ detect_protocol + 路由 → Task 2, 5
- ✅ ProviderConfig 覆盖 → Task 3

**Placeholder scan:** 无 TBD/TODO；复杂点（ResponsesStreamState 辅助函数、MockRouter 测试构造）有明确实现或指引参考现有模式（AnthropicStreamState）。

**Type consistency:** `supports_responses`（capability + config + OpenAIConfig 三处一致）；`responses_passthrough(body, stream)`（trait + 实现一致）；`ResponsesStreamState::serialize_chunk(&mut self, &LLMChunk, &str) -> String`（与 `AnthropicStreamState::serialize_chunk` 签名一致）。

**Scope:** 单一实现计划，10 个任务顺序依赖清晰。Task 1 是独立 bug fix（可单独 merge），Task 2-10 构成 Responses 功能。

---

## Execution Handoff

计划已保存到 `docs/superpowers/plans/2026-06-16-responses-api.md`。两种执行方式：

**1. Subagent-Driven（推荐）** — 每个 task 派发独立 subagent，task 间审查，快速迭代

**2. Inline Execution** — 在当前会话用 executing-plans 批量执行，带 checkpoint 审查

选哪种？
