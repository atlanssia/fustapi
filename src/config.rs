//! Configuration loading and validation.
//!
//! Handles reading `~/.fustapi/config.toml`, parsing TOML into typed structs,
//! and providing default configuration values.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// Top-level configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub server: ServerConfig,
    #[serde(default)]
    pub router: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
}

/// Server listening configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

/// A single provider endpoint configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderConfig {
    pub endpoint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

// ── Defaults ──────────────────────────────────────────────────────────

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    8080
}

// ── ConfigError ───────────────────────────────────────────────────────

/// Errors that can occur when loading or validating configuration.
#[derive(Debug)]
pub enum ConfigError {
    /// The config file was not found at the expected location.
    NotFound(PathBuf),
    /// The config file could not be parsed as valid TOML.
    ParseError(toml::de::Error),
    /// An I/O error occurred while reading or writing the config file.
    IoError(std::io::Error),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::NotFound(path) => {
                write!(f, "config file not found: {}", path.display())
            }
            ConfigError::ParseError(err) => {
                write!(f, "failed to parse config file: {err}")
            }
            ConfigError::IoError(err) => {
                write!(f, "config file I/O error: {err}")
            }
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::ParseError(err) => Some(err),
            ConfigError::IoError(err) => Some(err),
            ConfigError::NotFound(_) => None,
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────

/// Returns the path to the user config file: `~/.fustapi/config.toml`.
pub fn config_path() -> PathBuf {
    let home = dirs::home_dir()
        .expect("could not determine home directory");
    home.join(".fustapi").join("config.toml")
}

/// Load configuration from disk. Returns [`ConfigError::NotFound`] if the file does not exist.
pub fn load() -> Result<Config, ConfigError> {
    let path = config_path();

    if !path.exists() {
        return Err(ConfigError::NotFound(path));
    }

    let contents = std::fs::read_to_string(&path)
        .map_err(ConfigError::IoError)?;

    let config: Config = toml::from_str(&contents)
        .map_err(ConfigError::ParseError)?;

    Ok(config)
}

/// Return a [`Config`] with sensible defaults (no file I/O).
pub fn default_config() -> Config {
    Config {
        server: ServerConfig {
            host: default_host(),
            port: default_port(),
        },
        router: HashMap::new(),
        providers: HashMap::new(),
    }
}

/// Write the default configuration to the given path. Creates parent directories if needed.
pub fn save_default(path: &Path) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let contents = toml::to_string_pretty(&default_config())
        .map_err(std::io::Error::other)?;
    std::fs::write(path, contents)?;

    Ok(())
}

/// Initialise a default config file at the standard location. If a file already exists, prints a warning and returns `Ok`.
pub fn init_config() -> Result<(), ConfigError> {
    let path = config_path();

    if path.exists() {
        println!("Config file already exists at {:?}", path);
        return Ok(());
    }

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

        let relative = path.strip_prefix(&home).unwrap();
        assert!(relative.starts_with(".fustapi"));
    }

    #[test]
    fn test_save_and_load_default() {
        let dir = std::env::temp_dir().join("fustapi_test_config");

        // Clean up any leftover test file. 
        let _ = std::fs::remove_file(dir.join("config.toml"));

        save_default(&dir.join("config.toml")).expect("save_default failed");

        // Read raw TOML to verify it's valid. 
        let contents = std::fs::read_to_string(dir.join("config.toml"))
            .expect("read test config");

        let parsed: Config = toml::from_str(&contents)
            .expect("parsed test config");

        assert_eq!(parsed.server.host, "127.0.0.1");

        // Clean up. 
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
}
