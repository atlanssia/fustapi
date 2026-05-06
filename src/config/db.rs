//! SQLite database layer for control plane storage.
//!
//! **Hard rule: this module is NEVER called from the request path.**
//! All data is loaded into memory at startup and served from ArcSwap.
//!
//! Schema:
//! - `providers` — provider definitions (id, type, base_url, api_key, is_local)
//! - `routes` — model-to-provider routing rules (model, provider_ids as JSON array)

use rusqlite::{Connection, Result, Transaction, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A single provider record from the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderRecord {
    pub id: String,
    pub r#type: String,
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_model: Option<String>,
    pub is_local: bool,
}

/// A single route record from the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteRecord {
    pub model: String,
    pub provider_ids: Vec<String>,
}

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS providers (
    id TEXT PRIMARY KEY,
    type TEXT NOT NULL,
    base_url TEXT NOT NULL,
    api_key TEXT,
    upstream_model TEXT,
    is_local BOOLEAN NOT NULL DEFAULT 1
);
CREATE TABLE IF NOT EXISTS routes (
    model TEXT PRIMARY KEY,
    provider_ids TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_routes_model ON routes(model);
"#;

/// Initialize the database at the given path. Enables WAL mode.
pub fn init_db(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path.parent()
        && !parent.exists()
    {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(db_path)?;
    conn.execute_batch(SCHEMA_SQL)?;
    // Simple migration: add upstream_model if missing
    let _ = conn.execute("ALTER TABLE providers ADD COLUMN upstream_model TEXT", []);
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    Ok(conn)
}

/// Load all providers from the database.
pub fn load_providers(conn: &Connection) -> Result<Vec<ProviderRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, type, base_url, api_key, is_local, upstream_model FROM providers ORDER BY id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ProviderRecord {
            id: row.get(0)?,
            r#type: row.get(1)?,
            base_url: row.get(2)?,
            api_key: row.get(3)?,
            is_local: row.get(4)?,
            upstream_model: row.get(5)?,
        })
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Load all routes from the database.
pub fn load_routes(conn: &Connection) -> Result<Vec<RouteRecord>> {
    let mut stmt = conn.prepare("SELECT model, provider_ids FROM routes ORDER BY model")?;
    let rows = stmt.query_map([], |row| {
        let raw: String = row.get(1)?;
        let ids: Vec<String> = serde_json::from_str(&raw).unwrap_or_default();
        Ok(RouteRecord {
            model: row.get(0)?,
            provider_ids: ids,
        })
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Upsert a single provider record.
pub fn upsert_provider(conn: &Transaction, p: &ProviderRecord) -> Result<()> {
    conn.execute("INSERT OR REPLACE INTO providers (id, type, base_url, api_key, is_local, upstream_model) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![&p.id,&p.r#type,&p.base_url,p.api_key.as_deref(),p.is_local, p.upstream_model.as_deref()])?;
    Ok(())
}

/// Upsert a single route record.
pub fn upsert_route(conn: &Transaction, r: &RouteRecord) -> Result<()> {
    let j = serde_json::to_string(&r.provider_ids)
        .map_err(|_e| rusqlite::Error::ExecuteReturnedResults)?;
    conn.execute(
        "INSERT OR REPLACE INTO routes (model, provider_ids) VALUES (?1, ?2)",
        params![&r.model, j],
    )?;
    Ok(())
}

/// Delete a provider by id. Returns true if a row was deleted.
pub fn delete_provider(conn: &Transaction, id: &str) -> Result<bool> {
    let c = conn.execute("DELETE FROM providers WHERE id=?1", params![id])?;
    Ok(c > 0)
}

/// Delete a route by model name. Returns true if a row was deleted.
pub fn delete_route(conn: &Transaction, model: &str) -> Result<bool> {
    let c = conn.execute("DELETE FROM routes WHERE model=?1", params![model])?;
    Ok(c > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_db(test_name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fustapi_test_{}", test_name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("test.db")
    }

    #[test]
    fn test_init_db_creates_tables() {
        let path = temp_db("init");
        let conn = init_db(&path).expect("init_db failed");
        let c: i64 = conn
            .query_row("SELECT COUNT(*) FROM providers", [], |r| r.get(0))
            .unwrap();
        assert_eq!(c, 0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn test_upsert_and_load_providers() {
        let path = temp_db("upsert_providers");
        let mut conn = init_db(&path).expect("init_db failed");
        let tx = conn.transaction().unwrap();
        let p = ProviderRecord {
            id: "test-provider".into(),
            r#type: "omlx".into(),
            base_url: "http://localhost:5000".into(),
            api_key: Some("sk-test".into()),
            upstream_model: Some("gpt-4-test".into()),
            is_local: true,
        };
        upsert_provider(&tx, &p).unwrap();
        tx.commit().unwrap();
        let loaded = load_providers(&conn).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "test-provider");
        assert_eq!(loaded[0].upstream_model.as_deref(), Some("gpt-4-test"));
    }

    #[test]
    fn test_upsert_and_load_routes() {
        let path = temp_db("upsert_routes");
        let mut conn = init_db(&path).expect("init_db failed");
        let tx = conn.transaction().unwrap();
        let r = RouteRecord {
            model: "gpt-4".into(),
            provider_ids: vec!["omlx".into(), "lmstudio".into()],
        };
        upsert_route(&tx, &r).unwrap();
        tx.commit().unwrap();
        let loaded = load_routes(&conn).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].model, "gpt-4");
        assert_eq!(loaded[0].provider_ids.len(), 2);
    }

    #[test]
    fn test_delete_provider() {
        let path = temp_db("delete_provider");
        let mut conn = init_db(&path).expect("init_db failed");
        let tx = conn.transaction().unwrap();
        let p = ProviderRecord {
            id: "to-delete".into(),
            r#type: "omlx".into(),
            base_url: "http://localhost:5000".into(),
            api_key: None,
            upstream_model: None,
            is_local: true,
        };
        upsert_provider(&tx, &p).unwrap();
        tx.commit().unwrap();
        let tx2 = conn.transaction().unwrap();
        assert!(delete_provider(&tx2, "to-delete").unwrap());
        tx2.commit().unwrap();
        assert!(!delete_provider(&conn.transaction().unwrap(), "to-delete").unwrap());
    }

    #[test]
    fn test_delete_route() {
        let path = temp_db("delete_route");
        let mut conn = init_db(&path).expect("init_db failed");
        let tx = conn.transaction().unwrap();
        let r = RouteRecord {
            model: "delete-me".into(),
            provider_ids: vec!["omlx".into()],
        };
        upsert_route(&tx, &r).unwrap();
        tx.commit().unwrap();
        let tx2 = conn.transaction().unwrap();
        assert!(delete_route(&tx2, "delete-me").unwrap());
        tx2.commit().unwrap();
        assert!(!delete_route(&conn.transaction().unwrap(), "delete-me").unwrap());
    }
}
