# OpenAI Responses API 支持 — 设计文档

- 日期：2026-06-16
- 状态：待实现
- 相关：架构重构（候选 #1 非流式直通、#4 透传解耦协议）已为此铺路

## 背景

fustapi 是透传网关。当前支持两个入口协议：

- OpenAI Chat Completions（`POST /v1/chat/completions`）
- Anthropic Messages（`POST /v1/messages`）

两者都归一化到 `UnifiedRequest`，经 `router.chat_stream` 转发上游，响应走 Normalized / Passthrough / NonStreaming 三种流模式。

OpenAI 在 2025 年推出 Responses API（`POST /v1/responses`），语义模型与 Chat Completions 不同：

- **有状态**：`previous_response_id` + `store` 提供服务端多轮记忆
- **事件驱动流式**：`response.created`、`response.output_text.delta`、`response.completed` 等命名事件（非 `chat.completion.chunk` + `delta`）
- **内置工具上游执行**：`web_search` / `file_search` / `computer_use` / `code_interpreter`
- **`output[]` 多类型 item**：`message` / `function_call` / `reasoning` / `web_search_call` 等
- **计量字段**：`usage.input_tokens` / `output_tokens` / `total_tokens`（非 `prompt_tokens` / `completion_tokens`）

Responses API 不应归一化到 `UnifiedRequest`——会丢失状态链、内置工具语义、reasoning item，并引入大量错误分支。

## 目标

1. Responses API 作为第三入口协议（`POST /v1/responses`）
2. **上游支持 Responses**（OpenAI 原生）→ 纯透传
3. **上游为 Chat Completions**（DeepSeek 等）→ 协议转换
4. 转换与上游 Chat Completions 行为等价（无状态、无内置工具）

## 非目标（YAGNI）

- 不支持反向：Chat Completions 客户端 → Responses 上游
- 不实现网关侧 response 状态缓存（保持无状态）
- 不实现 Responses 内置工具（web_search / file_search 等）
- 不做 Responses → Anthropic Messages 跨协议转换

## 架构：双模式分流

`POST /v1/responses` 入口。`detect_protocol` 增加 `Protocol::Responses` 分支。`responses_handler` 根据 `provider.capabilities().supports_responses` 分流：

```
supports_responses == true  →  纯透传模式
supports_responses == false →  协议转换模式
```

### 模式 1：纯透传

绕过 `UnifiedRequest` 归一化层。原始 body 直接转发到上游 `/v1/responses`。

- 流式（请求含 `stream: true`）→ `StreamMode::Passthrough`（字节透传，8KB 滑动窗口提取 `response.usage`）
- 非流式 → `StreamMode::NonStreaming`（原始 JSON 透传）

复用架构重构建立的协议无关透传通道（候选 #1 / #4）。零解析、零转换、零新错误分支。

### 模式 2：协议转换

复用现有 Normalized 管线，两端加 Responses 格式适配。与 Anthropic 流式序列化（`AnthropicStreamState`）完全对称。

**请求侧** `protocol/responses.rs::parse_responses_request`：
- `input`（字符串或 items 数组）+ `instructions` → `UnifiedRequest.messages`
- function tools → `UnifiedRequest.tools`
- 内置工具 / `previous_response_id` / `store: true` → `400` 拒绝

**响应侧** 走 `router.chat_stream(req, allow_passthrough=false)` → Normalized `LLMChunk` 流：
- 流式：`ResponsesStreamState`（`LLMChunk` → `response.*` SSE 事件）
- 非流式：`serialize_responses_response`（累积 `LLMChunk` → `output[]` JSON）

转换层很薄——`UnifiedRequest` ↔ `LLMChunk` 归一化链 Chat Completions 上游已在用，转换层只在该链两端加 Responses 入口 parser 和出口序列化器。

### 数据流

```
纯透传模式:
  /v1/responses → responses_handler → ModelField → resolve provider
    → supports_responses == false → 转换模式（见下）
    → true → provider.responses_passthrough(原始 body)
    → 上游 SSE 字节流 → Passthrough(提取 response.usage)
    → 原样返回 SSE

协议转换模式:
  /v1/responses → responses_handler → ModelField → resolve provider
    → supports_responses == false
    → parse_responses_request: input → UnifiedRequest.messages
    → router.chat_stream(req, allow_passthrough=false)
    → 上游 Chat Completions → Normalized LLMChunk 流
    → ResponsesStreamState: LLMChunk → response.* 事件
    → 客户端
```

## 转换边界

按"保持与上游 Chat Completions 行为等价"原则取舍：

| Responses 特性 | 处理 | 理由 |
|---------------|------|------|
| `previous_response_id` / `store: true` | `400` 明确报错，要求完整 `input` | Chat Completions 无状态，网关也保持无状态 |
| 内置工具（web_search / file_search / computer_use / code_interpreter）| `400` 拒绝 | 上游无此能力，无法等价提供；静默剥离会误导客户端 |
| function tools | 映射到 Chat Completions `tools` | DeepSeek 等支持 function calling |
| reasoning | DeepSeek `reasoning_content` → Responses reasoning output item | 忠实映射 |
| `input` 字符串 | → 单条 user message | 标准映射 |
| `input` items 数组 | → 多条 messages（role/content 按类型映射）| 标准映射 |
| `instructions` | → system message | 标准映射 |

## 流式转换状态机

`ResponsesStreamState`（`protocol/serializer.rs`）维护 output item / content part 的开闭，把 `LLMChunk` 流展开为 Responses 事件序列。与 `AnthropicStreamState` 的 block 切换逻辑同构。

核心事件序列：

1. 流首（首个 chunk 前）：`response.created`（含 response 对象骨架，status: `in_progress`）
2. message item 打开：`response.output_item.added`（type: `message`, role: `assistant`）
3. content part 打开：`response.content_part.added`（type: `output_text`）
4. text delta：`response.output_text.delta`（每个 content chunk）
5. reasoning item（若 `reasoning_content` 非空）：切到 reasoning item，`response.reasoning_summary_text.delta`
6. function_call item（若 `tool_call` 非空）：`response.function_call_arguments.delta`
7. 流末：`response.output_text.done` → `response.content_part.done` → `response.output_item.done` → `response.completed`（含 usage）

item 类型切换时关闭前一个 item（`output_item.done`）再开新 item。

## 组件变更清单

| 文件 | 变更 |
|------|------|
| `streaming/mod.rs` | 无（复用 Passthrough / NonStreaming / Normalized）|
| `protocol/mod.rs` | `Protocol` 加 `Responses`；`detect_protocol` 加 `/v1/responses` 分支；`dispatch_request` 加 responses handler 路由 |
| `protocol/responses.rs`（新）| `parse_responses_request`：`input` + `instructions` → `UnifiedRequest`；内置工具 / 状态字段校验拒绝 |
| `protocol/serializer.rs` | `ResponsesStreamState`（流式）+ `serialize_responses_response`（非流式）|
| `protocol/stream_dispatch.rs` | `NonStreaming` arm + usage 提取协议感知（input_tokens vs prompt_tokens）|
| `server.rs` | `POST /v1/responses` 路由 + `responses_handler` |
| `router/mod.rs` | 转换模式复用现有 `chat_stream`（无 Responses 专属逻辑）；双模式分流在 `responses_handler` 层完成 |
| `provider/mod.rs` | `ProviderCapabilities::supports_responses: bool`；`Provider` trait 加 `responses_passthrough(body, stream)` 默认方法（透传专用，独立于 `chat_stream`）|
| `provider/cloud/openai.rs` | 实现 `responses_passthrough`：原始 body 发上游 `/v1/responses`，返回 `Passthrough` / `NonStreaming` |
| `config.rs` | `ProviderConfig::supports_responses` 可选字段（配置覆盖）；`create_provider` 按 type 默认设置 |
| `web.rs` | provider 配置 UI/API 暴露 `supports_responses` 字段 |

## 上游能力判定

- `create_provider` 按 `ProviderType` 默认：`OpenAI` = `true`，其余 = `false`
- `ProviderConfig` 增加可选 `supports_responses` 字段，用户可对 `OpenAICompatible` 上游（自建代理、OpenRouter）显式覆盖
- 网关路由时据此分流：`true` → 透传；`false` → 转换

## 错误处理

| 场景 | 状态码 | 处理 |
|------|--------|------|
| 转换：`previous_response_id` / `store: true` | `400` | 明确错误，指引用完整 `input` |
| 转换：内置工具 | `400` | 明确错误，指明上游不支持 |
| 上游 4xx/5xx（两模式一致）| 透传 | 状态码 + body 透传 |
| 上游流式错误事件 | 透传 | 字节透传，不解析 |

分流规则：`supports_responses=true` → 透传；`false` → 转换。不存在"拒绝"分支——`false` 总是走转换。纯透传模式下若上游实际不支持 Responses（配置与上游行为不符），上游返回的 4xx 由"上游错误透传"行覆盖，网关不额外判定。

## 计量

- 纯透传流式：从 `response.completed` 事件提取 `response.usage`
- 纯透传非流式：顶层 `usage`
- 转换模式：`LLMChunk.usage` 已带 Chat Completions 的 `prompt_tokens` / `completion_tokens`，序列化时映射为 Responses 的 `input_tokens` / `output_tokens` / `total_tokens`

字段映射集中在 `stream_dispatch` 一处。

## 测试策略

**纯透传模式**
- 流式：原始 SSE 字节完整保留
- 非流式：原始 JSON 完整保留
- usage 提取（`input_tokens` / `output_tokens`）
- 上游 4xx 透传

**协议转换模式**
- 请求：`input` 字符串 → messages；`input` 数组 → messages；`instructions` → system
- 响应非流式：`choices[].message` → `output[]` items
- 响应流式：`LLMChunk` → `response.*` 事件序列（created / output_text.delta / completed）
- reasoning 映射：`reasoning_content` → reasoning output item
- function_call 映射：tool_call → function_call item

**边界**
- `previous_response_id` → `400`
- 内置工具 → `400`
- 纯透传上游不支持 → 透传其 4xx
- model 路由复用现有 route 表

## 实现顺序（建议）

1. `Protocol::Responses` + `detect_protocol` 分支
2. `ProviderCapabilities::supports_responses` + `create_provider` 默认 + `ProviderConfig` 覆盖
3. 纯透传模式：`responses_handler` + `/v1/responses` 路由 + 上游 body 转发 + usage 提取
4. 协议转换模式 - 请求侧：`parse_responses_request`
5. 协议转换模式 - 响应侧：`ResponsesStreamState`（流式）+ `serialize_responses_response`（非流式）
6. 边界校验（`previous_response_id` / 内置工具 → 400）
7. 测试（按上述策略）

## 开放问题

无。所有边界已按"保持上游原有行为"原则确定。
