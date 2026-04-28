# Plan 03 — Foundation: Configuration System

## Status: PASS

## Tasks Completed

### Task 1: TOML config structs with serde deserialization
- **File:** `src/config.rs`
- Defined `Config`, `ServerConfig`, `ProviderConfig` structs with serde `Deserialize`/`Serialize`
- Defined `ConfigError` enum with `NotFound`, `ParseError`, `IoError` variants and `Display` impl
- Implemented `config_path()` → `~/.fustapi/config.toml`
- Implemented `load()` → reads and parses config file with clear error messages
- Implemented `default_config()` → returns sensible defaults (host=127.0.0.1, port=8080)
- Implemented `save_default()` → writes default config TOML to given path
- Added 4 unit tests (all passing)

### Task 2: Config init subcommand wired into main.rs
- **File:** `src/main.rs`
- Implemented CLI subcommands via clap derive: `config init` and `serve`
- `config init` calls `config::init_config()` — creates file or warns if exists
- `serve` loads config via `config::load()`, falls back to defaults on NotFound, passes resolved config to `server::run()`

## Verification Results

| Check | Result |
|-------|--------|
| `cargo check` | PASS |
| `cargo clippy` | PASS (clean) |
| `cargo build` | PASS |
| `cargo test` | PASS (4/4) |
| `cargo run -- config init` (fresh) | PASS — created `~/.fustapi/config.toml` |
| Config file contents | PASS — valid TOML with `[server]`, `[router]`, `[providers]` sections |
| `cargo run -- config init` (re-run) | PASS — printed "already exists" warning, did not overwrite |
| `cargo run -- serve` (with config) | PASS — server started, `/health` returned `{"status":"healthy"}` |
| `cargo run -- serve` (no config) | PASS — server started with defaults, `/health` returned `{"status":"healthy"}` |

## Files Modified
- `/Users/mw/workspace/repo/github.com/atlanssia/fustapi/src/config.rs` — full implementation (was stub)
- `/Users/mw/workspace/repo/github.com/atlanssia/fustapi/src/main.rs` — CLI subcommands wired up

## Bugs Fixed During Execution
1. **toml::ser::Error not convertible to std::io::Error** — used `.map_err(std::io::Error::other)` in `save_default()`
2. **Clippy redundant_closure warning** — replaced closure with `std::io::Error::other` function reference
3. **Test: use of moved value** — borrowed `home` (`&home`) in `test_config_path_format` to avoid double-move
