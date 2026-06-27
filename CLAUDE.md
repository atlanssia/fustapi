# CLAUDE.md

Behavioral guidelines derived from Andrej Karpathy's observations on LLM coding pitfalls, merged with fustapi's project-specific architecture constraints.

**Tradeoff:** These guidelines bias toward caution over speed. For trivial tasks, use judgment.

---

## Project Identity

fustapi is a **transparent LLM API aggregation gateway** (Rust, axum, tokio, reqwest). It sits between LLM clients and upstream providers, proxying requests with minimal intervention.

**Core principle: pass-through by default, transform only when necessary, shortest path, no unnecessary error branches.**

Three entry protocols: OpenAI Chat Completions (`/v1/chat/completions`), Anthropic Messages (`/v1/messages`), OpenAI Responses (`/v1/responses`). All upstreams are OpenAI-compatible (Chat Completions), except when `supports_responses=true` (Responses).

Protocol routing: `protocol/mod.rs` `detect_protocol` by path. Dispatch: `openai_handler` / `anthropic_handler` / `responses_handler_impl`. Internal canonical form: `UnifiedRequest` (provider/mod.rs). Six `ProviderError` variants (Connection, Request, Upstream, ModelNotFound, Internal, Stream).

**Hard rule: the request path NEVER touches SQLite.** Config loaded at startup into `ArcSwap<RealRouter>`. Control plane writes DB; data plane reads atomic snapshot.

Tests: `cargo test` (src-internal `#[cfg(test)]` + `tests/api_tests.rs` with `tower::ServiceExt::oneshot`). Clippy: `cargo clippy --all-targets -- -D warnings`. Format: `cargo fmt --check`.

---

## 1. Think Before Coding

**Don't assume. Don't hide confusion. Surface tradeoffs.**

Before implementing:
- State assumptions explicitly. If uncertain about upstream behavior, ask.
- If multiple protocol mappings exist, present them — don't pick silently.
- If something can be passed through instead of transformed, say so. The default is passthrough.
- If a change introduces a new error branch, stop and ask whether it's truly necessary.
- When touching protocol dispatch (`protocol/mod.rs`), verify the full match is exhaustive across `Protocol::OpenAI | Anthropic | Responses`.

---

## 2. Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what was asked. No "future-proofing" abstractions.
- A single implementation = a hypothetical seam. Don't introduce traits for one adapter.
- No error variants that aren't constructed. Every error branch must be reachable.
- If a non-streaming request can skip the SSE parser, it should.
- Passthrough mode (`Passthrough` / `NonStreaming`) always preferred over Normalized when format permits.
- `UnifiedRequest` normalization is only justified when cross-protocol conversion is needed.

Ask: "Would a senior Rust engineer say this is overcomplicated?" If yes, simplify.

---

## 3. Surgical Changes

**Touch only what you must. Clean up only your own mess.**

When editing existing code:
- Don't "improve" adjacent code, comments, or formatting (cargo fmt handles formatting).
- Don't refactor things that aren't broken. This gateway was already deepened.
- Match existing patterns: handlers mirror each other (`chat_completions_handler` ↔ `messages_handler` ↔ `responses_handler`), stream state machines mirror each other (`AnthropicStreamState` ↔ `ResponsesStreamState`).
- If you notice unrelated dead code, mention it — don't delete it.

When your changes create orphans:
- Remove imports/variables/functions that YOUR changes made unused.
- Run `cargo clippy` — it catches most orphans.
- If a `match` becomes non-exhaustive, add the missing arms.

The test: Every changed line should trace directly to the user's request.

---

## 4. Goal-Driven Execution

**Define success criteria. Loop until verified.**

Transform tasks into verifiable goals:
- "Add protocol" → "Write `detect_protocol` test that fails, then add variant + branch"
- "Fix bug" → "Write a test that reproduces it, then make it pass"
- "Add conversion" → "Write boundary tests (400s), then wire conversion pipeline"

For multi-step tasks:
1. [Step] → verify: `cargo test <name> --lib`
2. [Step] → verify: `cargo test <name> --lib && cargo clippy`
3. [Step] → verify: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`

**Hard gates before claiming completion:**
- `cargo test` — all tests pass (currently 369)
- `cargo clippy --all-targets -- -D warnings` — zero warnings
- `cargo fmt --check` — clean
- No `unwrap()` in production code (use `?`, `.map_err()`, or `.expect()` with a message)

---

## Architecture Invariants (do not violate)

1. **Passthrough requires format match.** `allow_passthrough = protocol == Protocol::OpenAI` because all upstreams are CC-format. AM entry must always convert. RP passthrough is gated on `supports_responses`.
2. **No state in the gateway.** `previous_response_id` / `store:true` must be rejected (400), not silently dropped. The gateway forwards bytes, not conversations.
3. **SQLite = control plane only.** `ArcSwap<RealRouter>` is the data plane's only config source. Request handlers never import `db.rs`.
4. **StreamMode has three variants.** `Normalized` (LLMChunk-based conversion), `Passthrough` (byte-level forwarding), `NonStreaming` (raw JSON passthrough). Every `match` on it must be exhaustive.
5. **`Protocol` enum is exhaustive.** Adding a protocol requires updating `detect_protocol`, `dispatch_request`, and every `match protocol` in the codebase.
6. **Provider capabilities drive dispatch.** `supports_responses`, `tool_calling`, `image_input`, `streaming` — all in `ProviderCapabilities`. Config override via `ProviderConfig` optional fields.
7. **Route is a two-layer alias, NOT a passthrough of upstream model names.** A route's `model` field is the client-facing **alias** the client must send (e.g. `sonnet`). `provider_ids` is the fallback chain. `upstream_models` (keyed by provider id) holds each provider's **real** model name (e.g. `sonnet → [deepseek] → {deepseek: deepseek-v4-flash}`). The gateway resolves alias → provider → real model name; the client never sees upstream names. **Never** register a provider's real model name (e.g. `deepseek-v4-flash`) as a route `model` key — that misuses the abstraction (value-as-key), is redundant with the alias that already maps to it, and gets wiped on the next `save_to_db` (full `DELETE`+`INSERT` overwrite). If a client sends a raw upstream name, the fix is on the client side (send an alias) or by configuring an alias route — not by creating a same-named route key.

---

## Commit Conventions

- `fix:` — bug fix
- `feat(module):` — new feature (with module scope)
- `refactor:` — code change without behavior change
- `perf:` — performance improvement
- `test:` — test addition/update
- `docs:` — documentation only
- `style:` — formatting (cargo fmt)
- `release:` — version bump
- `merge:` — merge commit (use `--no-ff`)

All commits end with `Co-Authored-By: Claude <noreply@anthropic.com>`.

---

**These guidelines are working if:** diffs are minimal and focused, passthrough is the default path, error branches are provably reachable, new protocols mirror existing patterns without reinvention, and `cargo test && cargo clippy && cargo fmt` passes before every commit.
