//! Configuration loading and SQLite persistence.
//!
//! Architecture:
//! - Bootstrap parameters (host, port, data-dir) come from CLI flags + env vars
//! - Runtime data (providers, routes) lives exclusively in SQLite
//! - **Hard rule: the request path NEVER touches SQLite.**

pub mod db;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::info;

// ── Config Types ──────────────────────────────────────────────────────

/// Runtime configuration loaded from SQLite.
/// Contains only business data — no server/bootstrap parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub router: HashMap<String, Vec<String>>,
    pub providers: HashMap<String, ProviderConfig>,
}

/// A single provider endpoint configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderConfig {
    pub endpoint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Optional upstream model name override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Provider type (e.g., "openai", "omlx", "lmstudio", "sglang", "deepseek").
    #[serde(default = "default_type")]
    pub r#type: String,
}

fn default_type() -> String {
    "openai".to_string()
}

// ── Bootstrap Config ──────────────────────────────────────────────────

/// Bootstrap parameters resolved at startup from CLI flags / env vars.
/// These never enter the database.
#[derive(Debug, Clone)]
pub struct BootstrapConfig {
    pub host: String,
    pub port: u16,
    pub data_dir: PathBuf,
}

pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 8800;

impl Default for BootstrapConfig {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.to_string(),
            port: DEFAULT_PORT,
            data_dir: default_data_dir(),
        }
    }
}

impl BootstrapConfig {
    /// Path to the SQLite database within data_dir.
    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("fustapi.db")
    }
}

/// Default data directory: ~/.fustapi
fn default_data_dir() -> PathBuf {
    dirs::home_dir()
        .expect("could not determine home directory")
        .join(".fustapi")
}

// ── Defaults ──────────────────────────────────────────────────────────

/// Return a default AppConfig with no providers or routes.
pub fn default_config() -> AppConfig {
    AppConfig {
        router: HashMap::new(),
        providers: HashMap::new(),
    }
}

// ── Config Error ──────────────────────────────────────────────────────

/// Errors that can occur when loading or saving configuration.
#[derive(Debug)]
pub enum ConfigError {
    IoError(std::io::Error),
    DbError(rusqlite::Error),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::IoError(err) => write!(f, "config I/O error: {err}"),
            ConfigError::DbError(err) => write!(f, "database error: {err}"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::IoError(err) => Some(err),
            ConfigError::DbError(err) => Some(err),
        }
    }
}

// ── Database Loading ──────────────────────────────────────────────────

/// Load configuration from SQLite database.
pub fn load_from_db(db_path: &Path) -> Result<AppConfig, ConfigError> {
    use db::{load_providers, load_routes};
    let conn = db::init_db(db_path).map_err(ConfigError::DbError)?;
    let provider_records = load_providers(&conn).map_err(ConfigError::DbError)?;
    let mut providers = HashMap::new();
    for rec in &provider_records {
        providers.insert(
            rec.id.clone(),
            ProviderConfig {
                endpoint: rec.base_url.clone(),
                api_key: rec.api_key.clone(),
                model: rec.upstream_model.clone(),
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
    Ok(AppConfig { router, providers })
}

// ── Provider Factory ──────────────────────────────────────────────────

/// Create a provider instance from a provider config entry.
pub fn create_provider(_name: &str, cfg: &ProviderConfig) -> Box<dyn crate::provider::Provider> {
    match cfg.r#type.as_str() {
        "omlx" => Box::new(crate::provider::omlx::OmlxProvider::new(
            crate::provider::omlx::OmlxConfig {
                endpoint: cfg.endpoint.clone(),
                model: cfg.model.clone(),
            },
        )),
        "lmstudio" => Box::new(crate::provider::lmstudio::LmStudioProvider::new(
            crate::provider::lmstudio::LmStudioConfig {
                endpoint: cfg.endpoint.clone(),
                model: cfg.model.clone(),
            },
        )),
        "sglang" => Box::new(crate::provider::sglang::SglProvider::new(
            crate::provider::sglang::SglConfig {
                endpoint: cfg.endpoint.clone(),
                model: cfg.model.clone(),
            },
        )),
        "deepseek" => Box::new(crate::provider::cloud::deepseek::DeepSeekProvider::new(
            crate::provider::cloud::deepseek::DeepSeekConfig {
                endpoint: cfg.endpoint.clone(),
                api_key: cfg.api_key.clone().unwrap_or_default(),
                model: cfg.model.clone(),
            },
        )),
        "openai" | "openai-compatible" => {
            Box::new(crate::provider::cloud::openai::OpenAIProvider::new(
                crate::provider::cloud::openai::OpenAIConfig {
                    endpoint: cfg.endpoint.clone(),
                    api_key: cfg.api_key.clone().unwrap_or_default(),
                    model: cfg.model.clone(),
                },
            ))
        }
        _ => Box::new(crate::provider::omlx::OmlxProvider::default_provider()),
    }
}

// ── Persistence (Control Plane Write Path) ────────────────────────────

/// Save current in-memory config back to SQLite database.
pub fn save_to_db(config: &AppConfig, db_path: &Path) -> Result<(), ConfigError> {
    use db::{init_db, upsert_provider, upsert_route};
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).map_err(ConfigError::IoError)?;
    }
    let mut conn = init_db(db_path).map_err(ConfigError::DbError)?;
    let tx = conn.transaction().map_err(ConfigError::DbError)?;
    tx.execute("DELETE FROM routes", [])
        .map_err(ConfigError::DbError)?;
    tx.execute("DELETE FROM providers", [])
        .map_err(ConfigError::DbError)?;
    for (id, cfg) in &config.providers {
        let rec = db::ProviderRecord {
            id: id.clone(),
            r#type: cfg.r#type.clone(),
            base_url: cfg.endpoint.clone(),
            api_key: cfg.api_key.clone(),
            upstream_model: cfg.model.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_values() {
        let cfg = default_config();
        assert!(cfg.router.is_empty());
        assert!(cfg.providers.is_empty());
    }

    #[test]
    fn test_bootstrap_default() {
        let boot = BootstrapConfig::default();
        assert_eq!(boot.host, DEFAULT_HOST);
        assert_eq!(boot.port, DEFAULT_PORT);
        assert!(boot.db_path().ends_with("fustapi.db"));
    }

    #[test]
    fn test_bootstrap_db_path() {
        let boot = BootstrapConfig {
            host: "0.0.0.0".into(),
            port: 9090,
            data_dir: PathBuf::from("/tmp/fustapi-test"),
        };
        assert_eq!(
            boot.db_path(),
            PathBuf::from("/tmp/fustapi-test/fustapi.db")
        );
    }

    #[test]
    fn test_config_error_display() {
        let err = ConfigError::IoError(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "access denied",
        ));
        let msg = format!("{err}");
        assert!(msg.contains("I/O error"));
    }

    #[test]
    fn test_load_from_db_is_empty_initially() {
        let temp_dir =
            std::env::temp_dir().join(format!("fustapi_test_load_{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("fustapi.db");
        let cfg = load_from_db(&db_path).unwrap();
        assert!(cfg.providers.is_empty());
        assert!(cfg.router.is_empty());
        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn test_save_to_db_replaces_stale_rows() {
        let dir = std::env::temp_dir().join("fustapi_test_replace_stale_rows");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("test.db");

        let mut first = default_config();
        first.providers.insert(
            "old-provider".into(),
            ProviderConfig {
                endpoint: "http://old".into(),
                api_key: None,
                model: None,
                r#type: "omlx".into(),
            },
        );
        first
            .router
            .insert("old-model".into(), vec!["old-provider".into()]);
        save_to_db(&first, &db_path).expect("first save should work");

        let mut second = default_config();
        second.providers.insert(
            "new-provider".into(),
            ProviderConfig {
                endpoint: "http://new".into(),
                api_key: None,
                model: None,
                r#type: "openai".into(),
            },
        );
        second
            .router
            .insert("new-model".into(), vec!["new-provider".into()]);
        save_to_db(&second, &db_path).expect("second save should work");

        let loaded = load_from_db(&db_path).expect("load should work");
        assert!(loaded.providers.contains_key("new-provider"));
        assert!(!loaded.providers.contains_key("old-provider"));
        assert!(loaded.router.contains_key("new-model"));
        assert!(!loaded.router.contains_key("old-model"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
