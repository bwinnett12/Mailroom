use anyhow::Result;
use axum::{extract::{Path, State}, http::StatusCode, Json, Router, routing::get};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use tokio::fs;

use crate::router::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/jd",           get(list_areas))
        .route("/jd/:address",  get(lookup))
}

// ---------- Data model ----------

/// The in-memory Johnny Decimal index.
/// Loaded once at startup from the `jd_root` directory.
#[derive(Debug, Clone, Default)]
pub struct JohnnyDecimalIndex {
    /// Map of JD address (e.g. "12.34") → entry
    pub entries: HashMap<String, JdEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JdEntry {
    /// Full JD address, e.g. "12.34"
    pub address: String,
    /// Human-readable title
    pub title: String,
    /// Optional description / notes
    pub description: Option<String>,
    /// Which agent (by name) should handle items filed here
    pub agent: Option<String>,
    /// Tags for further routing logic
    pub tags: Vec<String>,
}

impl JohnnyDecimalIndex {
    /// Load from a directory containing a `index.json` or `index.toml`.
    /// Falls back to empty index if the directory doesn't exist yet.
    pub async fn load(root: &str) -> Result<Self> {
        let json_path = format!("{}/index.json", root);
        match fs::read_to_string(&json_path).await {
            Ok(raw) => {
                let entries: Vec<JdEntry> = serde_json::from_str(&raw)?;
                let map = entries.into_iter().map(|e| (e.address.clone(), e)).collect();
                Ok(Self { entries: map })
            }
            Err(_) => {
                tracing::warn!(path = json_path, "JD index not found — starting empty");
                Ok(Self::default())
            }
        }
    }

    /// Look up an address, also accepting partial prefixes ("12" matches all "12.*").
    pub fn lookup(&self, address: &str) -> Vec<&JdEntry> {
        self.entries
            .values()
            .filter(|e| e.address.starts_with(address))
            .collect()
    }

    /// Find which agent should handle an envelope destined for `address`.
    pub fn agent_for(&self, address: &str) -> Option<&str> {
        self.entries.get(address).and_then(|e| e.agent.as_deref())
    }
}

// ---------- HTTP handlers ----------

pub async fn list_areas(State(state): State<Arc<AppState>>) -> Json<Vec<JdEntry>> {
    let mut entries: Vec<JdEntry> = state.jd.entries.values().cloned().collect();
    entries.sort_by(|a, b| a.address.cmp(&b.address));
    Json(entries)
}

pub async fn lookup(
    State(state): State<Arc<AppState>>,
    Path(address): Path<String>,
) -> Result<Json<Vec<JdEntry>>, StatusCode> {
    let results: Vec<JdEntry> = state.jd.lookup(&address).into_iter().cloned().collect();
    if results.is_empty() {
        Err(StatusCode::NOT_FOUND)
    } else {
        Ok(Json(results))
    }
}
