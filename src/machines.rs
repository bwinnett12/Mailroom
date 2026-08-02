// src/machines.rs
//
// The machines registry: maps a machine's short name (as used in Nix
// priority lists, e.g. "island") to its real network address — currently
// its Tailscale hostname/IP. This is the one place that answers "how do I
// actually reach a given machine," so that failover logic for any
// multi-machine service (LocalAI today, whatever else needs the same
// pattern later) builds on top of one shared registry instead of each
// service hardcoding its own addresses.
//
// Loaded from a simple TOML file for now — a minimal stand-in for the
// backlogged machines-registry item (eventually Postgres-backed, with
// live presence resolution via node_exporter). Shape:
//
//   [machines]
//   island     = "100.x.x.x"
//   loom       = "100.x.x.y"
//   locomotive = "100.106.125.87"

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct MachineRegistry {
    pub machines: HashMap<String, String>,
}

impl MachineRegistry {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let registry: MachineRegistry = toml::from_str(&text)?;
        Ok(registry)
    }

    pub fn empty() -> Self {
        MachineRegistry { machines: HashMap::new() }
    }

    pub fn host_for(&self, name: &str) -> Option<&str> {
        self.machines.get(name).map(|s| s.as_str())
    }
}

/// Tries each machine name in `priority` order (looked up via `registry`),
/// probing `http://{host}:{port}{health_path}` on each. Returns the base
/// URL (`http://{host}:{port}`) of the first one that responds
/// successfully, or `None` if every candidate is unreachable/unhealthy.
///
/// Uses a short per-request timeout — a down machine on a tailnet should
/// fail fast, not hang on the OS's default TCP timeout (which would make
/// failing over across 3 candidates take minutes instead of seconds).
pub async fn resolve_available(
    registry: &MachineRegistry,
    priority: &[String],
    port: u16,
    health_path: &str,
) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .ok()?;

    for name in priority {
        let Some(host) = registry.host_for(name) else {
            tracing::warn!(machine = %name, "not found in machines registry — skipping");
            continue;
        };

        let base_url = format!("http://{host}:{port}");
        let health_url = format!("{base_url}{health_path}");

        match client.get(&health_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                tracing::info!(machine = %name, url = %health_url, "resolved available machine");
                return Some(base_url);
            }
            Ok(resp) => {
                tracing::warn!(machine = %name, status = %resp.status(), "unhealthy — trying next");
            }
            Err(e) => {
                tracing::warn!(machine = %name, error = %e, "unreachable — trying next");
            }
        }
    }

    None
}