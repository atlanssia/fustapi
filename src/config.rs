//! Configuration loading, in-memory config, and SQLite persistence.
//!
//! Architecture:
//!
//!
//! **Hard rule: the request path NEVER touches SQLite.**

pub mod db;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

// ── Config Types ──────────────────────────────────────────────────────

/// Top-level configuration (in-memory, atomically swappable).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub router: HashMap<String, Vec<String>>,
    pub providers: HashMap<String, ProviderConfig>,
}

/// Server listening configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

/// A single provider endpoint configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderConfig {
    pub endpoint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Provider type (e.g., "openai", "omlx", "lmstudio", "sglang").
    #[serde(default = "default_type")]
    pub r#type: String,
}

fn default_type() -> String {
    "openai".to_string()
}

// ── Defaults ──────────────────────────────────────────────────────────

fn default_host() -> String {
    "127.0.0.1".to_string()
}
fn default_port() -> u16 {
    8080
}

/// Return a default AppConfig with no I/O.
pub fn default_config() -> AppConfig {
    AppConfig {
        server: ServerConfig {
            host: default_host(),
            port: default_port(),
        },
        router: HashMap::new(),
        providers: HashMap::new(),
    }
}
// ── Config Error ────────────────────────────────────────────────────

/// Errors that can occur when loading or validating configuration.
#[derive(Debug)]
pub enum ConfigError {
    NotFound(PathBuf),
    ParseError(toml::de::Error),
    IoError(std::io::Error),
    DbError(rusqlite::Error),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::NotFound(path) => write!(f, "config file not found: {}", path.display()),
            ConfigError::ParseError(err) => write!(f, "failed to parse config file: {err}"),
            ConfigError::IoError(err) => write!(f, "config file I/O error: {err}"),
            ConfigError::DbError(err) => write!(f, "database error: {err}"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::ParseError(err) => Some(err),
            ConfigError::IoError(err) => Some(err),
            ConfigError::DbError(err) => Some(err),
            ConfigError::NotFound(_) => None,
        }
    }
}
// ── Paths ─────────────────────────────────────────────────────────────

/// Path to the legacy TOML config file: ~/.fustapi/config.toml.
pub fn config_path() -> PathBuf {
    dirs::home_dir()
        .expect("could not determine home directory")
        .join(".fustapi")
        .join("config.toml")
}

/// Path to the SQLite database: ~/.fustapi/fustapi.db.
pub fn db_path() -> PathBuf {
    dirs::home_dir()
        .expect("could not determine home directory")
        .join(".fustapi")
        .join("fustapi.db")
}

// ── TOML Bootstrap (backward-compatible) ──────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
struct TomlConfig {
    server: TomlServerConfig,
    #[serde(default)]
    router: HashMap<String, Vec<String>>,
    #[serde(default)]
    providers: HashMap<String, ProviderConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct TomlServerConfig {
    #[serde(default = "default_host")]
    host: String,
    #[serde(default = "default_port")]
    port: u16,
}

/// Load legacy TOML config from disk.
pub fn load_toml_config() -> Result<AppConfig, ConfigError> {
    let path = config_path();
    if !path.exists() {
        return Err(ConfigError::NotFound(path));
    }
    let contents = std::fs::read_to_string(&path).map_err(ConfigError::IoError)?;
    let toml_cfg: TomlConfig = toml::from_str(&contents).map_err(ConfigError::ParseError)?;
    Ok(AppConfig {
        server: ServerConfig {
            host: toml_cfg.server.host,
            port: toml_cfg.server.port,
        },
        router: toml_cfg.router,
        providers: toml_cfg.providers,
    })
}

// ── In-Memory Config Store ────────────────────────────────────────────

/// Global in-memory configuration store.
pub struct AppConfigStore {
    inner: ArcSwap<AppConfig>,
}

impl AppConfigStore {
    /// Create a new store with the given initial config.
    pub fn new(config: AppConfig) -> Self {
        Self {
            inner: ArcSwap::new(config.into()),
        }
    }

    /// Atomically swap in a new config. Returns the old config.
    pub fn swap(&self, new_config: AppConfig) -> AppConfig {
        let old = self.inner.swap(new_config.into());
        (*old).clone()
    }

    /// Load a snapshot of the current config (lock-free Arc clone).
    pub fn load(&self) -> AppConfig {
        let ptr = self.inner.load();
        (**ptr).clone()
    }
}
// ── Database Loading ──────────────────────────────────────────────────

/// Load configuration from SQLite database into an AppConfig.
pub fn load_from_db(db_path: &Path) -> Result<AppConfig, ConfigError> {
    use db::{load_providers, load_routes, seed_if_empty};
    let mut conn = db::init_db(db_path).map_err(ConfigError::DbError)?;
    seed_if_empty(&mut conn).map_err(ConfigError::DbError)?;
    let provider_records = load_providers(&conn).map_err(ConfigError::DbError)?;
    let mut providers = HashMap::new();
    for rec in &provider_records {
        providers.insert(
            rec.id.clone(),
            ProviderConfig {
                endpoint: rec.base_url.clone(),
                api_key: rec.api_key.clone(),
                r#type: rec.r#type.clone(),
            },
        );
    }
    let route_records = load_routes(&conn).map_err(ConfigError::DbError)?;
    let mut router = HashMap::new();
    for rec in &route_records {
        router.insert(rec.model.clone(), rec.provider_ids.clone());
    }
    info!(
        providers = providers.len(),
        routes = router.len(),
        "Loaded configuration from SQLite"
    );
    Ok(AppConfig {
        server: ServerConfig {
            host: default_host(),
            port: default_port(),
        },
        router,
        providers,
    })
}

/// Load configuration by merging TOML bootstrap + SQLite runtime data.
pub fn load_merged(db_path: &Path) -> Result<AppConfig, ConfigError> {
    match load_from_db(db_path) {
        Ok(db_config) => match load_toml_config() {
            Ok(toml_config) => {
                info!("Merged TOML server settings with SQLite runtime config");
                Ok(AppConfig {
                    server: toml_config.server,
                    router: db_config.router,
                    providers: db_config.providers,
                })
            }
            Err(ConfigError::NotFound(_)) => Ok(db_config),
            Err(e) => {
                warn!("TOML config parse error ({e}), using SQLite-only config");
                Ok(db_config)
            }
        },
        Err(ConfigError::NotFound(_)) => {
            warn!("SQLite database not found, falling back to TOML config");
            load_toml_config()
        }
        Err(e) => Err(e),
    }
}
// ── Provider Factory ──────────────────────────────────────────────────

/// Create a provider instance from a provider config entry.
pub fn create_provider(_name: &str, cfg: &ProviderConfig) -> Box<dyn crate::provider::Provider> {
    match cfg.r#type.as_str() {
        "omlx" => Box::new(crate::provider::omlx::OmlxProvider::new(
            crate::provider::omlx::OmlxConfig {
                endpoint: cfg.endpoint.clone(),
            },
        )),
        "lmstudio" => Box::new(crate::provider::lmstudio::LmStudioProvider::new(
            crate::provider::lmstudio::LmStudioConfig {
                endpoint: cfg.endpoint.clone(),
            },
        )),
        "sglang" => Box::new(crate::provider::sglang::SglProvider::new(
            crate::provider::sglang::SglConfig {
                endpoint: cfg.endpoint.clone(),
            },
        )),
        "deepseek" => Box::new(crate::provider::cloud::deepseek::DeepSeekProvider::new(
            crate::provider::cloud::deepseek::DeepSeekConfig {
                endpoint: cfg.endpoint.clone(),
                api_key: cfg.api_key.clone().unwrap_or_default(),
            },
        )),
        "openai" => Box::new(crate::provider::cloud::openai::OpenAIProvider::new(
            crate::provider::cloud::openai::OpenAIConfig {
                endpoint: cfg.endpoint.clone(),
                api_key: cfg.api_key.clone().unwrap_or_default(),
            },
        )),
        _ => Box::new(crate::provider::omlx::OmlxProvider::default_provider()),
    }
}

// ── Persistence (Control Plane Write Path) ────────────────────────────

/// Save current in-memory config back to SQLite database.
pub fn save_to_db(config: &AppConfig, db_path: &Path) -> Result<(), ConfigError> {
    use db::{init_db, upsert_provider, upsert_route};
    let mut conn = init_db(db_path).map_err(ConfigError::DbError)?;
    let tx = conn.transaction().map_err(ConfigError::DbError)?;
    for (id, cfg) in &config.providers {
        let rec = db::ProviderRecord {
            id: id.clone(),
            r#type: cfg.r#type.clone(),
            base_url: cfg.endpoint.clone(),
            api_key: cfg.api_key.clone(),
            is_local: true,
        };
        upsert_provider(&tx, &rec).map_err(ConfigError::DbError)?;
    }
    for (model, provider_ids) in &config.router {
        let rec = db::RouteRecord {
            model: model.clone(),
            provider_ids: provider_ids.clone(),
        };
        upsert_route(&tx, &rec).map_err(ConfigError::DbError)?;
    }
    tx.commit().map_err(ConfigError::DbError)?;
    info!(
        "Saved {} providers and {} routes to SQLite",
        config.providers.len(),
        config.router.len()
    );
    Ok(())
}
// ── Legacy API (backward-compatible) ──────────────────────────────────

#[deprecated(since = "0.2.0", note = "Use load_merged() instead")]
pub fn load() -> Result<AppConfig, ConfigError> {
    load_toml_config()
}

#[allow(deprecated)]
#[deprecated(since = "0.2.0", note = "Use SQLite seed_if_empty() instead")]
pub fn save_default(path: &Path) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let contents = toml::to_string_pretty(&default_config()).map_err(std::io::Error::other)?;
    std::fs::write(path, contents)?;
    Ok(())
}

#[deprecated(since = "0.2.0", note = "SQLite auto-seeds on first run")]
pub fn init_config() -> Result<(), ConfigError> {
    let path = config_path();
    if path.exists() {
        println!("Config file already exists at {:?}", path);
        return Ok(());
    }
    #[allow(deprecated)]
    save_default(&path).map_err(ConfigError::IoError)?;
    println!("Config file created at {:?}", path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_values() {
        let cfg = default_config();
        assert_eq!(cfg.server.host, "127.0.0.1");
        assert_eq!(cfg.server.port, 8080);
        assert!(cfg.router.is_empty());
        assert!(cfg.providers.is_empty());
    }

    #[test]
    fn test_config_path_format() {
        let path = config_path();
        assert!(path.ends_with("config.toml"));
        let home = dirs::home_dir().expect("home dir");
        assert!(path.starts_with(&home));
    }

    #[test]
    fn test_db_path_format() {
        let path = db_path();
        assert!(path.ends_with("fustapi.db"));
    }

    #[test]
    fn test_save_and_load_default() {
        let dir = std::env::temp_dir().join("fustapi_test_config_save");
        let _ = std::fs::remove_file(dir.join("config.toml"));
        #[allow(deprecated)]
        save_default(&dir.join("config.toml")).expect("save_default failed");
        let contents = std::fs::read_to_string(dir.join("config.toml")).expect("read test config");
        let parsed: AppConfig = toml::from_str(&contents).expect("parsed test config");
        assert_eq!(parsed.server.host, "127.0.0.1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_config_error_display() {
        let err = ConfigError::NotFound(PathBuf::from("/no/such/file.toml"));
        let msg = format!("{err}");
        assert!(msg.contains("not found"));
        let err = ConfigError::IoError(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "access denied",
        ));
        let msg = format!("{err}");
        assert!(msg.contains("I/O error"));
    }

    #[test]
    fn test_app_config_store_swap() {
        let initial = default_config();
        let store = AppConfigStore::new(initial);
        let loaded = store.load();
        assert_eq!(loaded.server.host, "127.0.0.1");
        let mut updated = loaded;
        updated.server.port = 3000;
        store.swap(updated);
        let reloaded = store.load();
        assert_eq!(reloaded.server.port, 3000);
    }

    #[test]
    fn test_load_from_db_seeds_defaults() {
        let dir = std::env::temp_dir().join("fustapi_test_load_db_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("test.db");
        let cfg = load_from_db(&db_path).expect("load_from_db failed");
        assert!(!cfg.providers.is_empty());
        assert!(!cfg.router.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
