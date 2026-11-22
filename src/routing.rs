// src/routing.rs
//
// The routing table: which data types go where, read from routes.toml.
// Config, not code. A new data type is a new [[routes]] entry, never a
// new match arm.
//
// Everything is checked at load time, so a bad table fails at startup:
//   - every dodo, nest and hop name referenced exists
//   - a dodo may only be routed into nests on its own allowlist
//   - each data_type maps to exactly one route
//
// Hops are converted once, into a bank keyed by name. Routes reference
// them by name, so a hop shared by thousands of routes is written once.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Context};
use serde::Deserialize;

use crate::dodo::Dodo;
use crate::nest::Nest;
use crate::registry::Registry;
use crate::route::{Hop, Route, Stop};

// ── File shape ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoutesFile {
    #[serde(default)]
    dodos: HashMap<String, DodoDef>,
    #[serde(default)]
    hops: HashMap<String, HopDef>,
    #[serde(default)]
    defaults: Defaults,
    #[serde(default)]
    routes: Vec<RouteDef>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DodoDef {
    home_nest: String,
    available_nests: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Defaults {
    #[serde(default)]
    after: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteDef {
    data_type: String,
    dodo: String,
    nest: String,
    #[serde(default)]
    hops: Vec<String>,
    #[serde(default)]
    skip_defaults: bool,
    #[serde(default)]
    watchers: Vec<String>,
}

/// One entry in the hop bank. Exactly one of run / store / log is set.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HopDef {
    run: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    store: Option<bool>,
    log: Option<String>,
    emit: Option<bool>,
    link: Option<LinkDef>,
}

/// `link = true` or `link = "add"`: links only add.
/// `link = "replace"`: the envelope's links become the node's full set.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum LinkDef {
    Flag(bool),
    Mode(String),
}

impl HopDef {
    fn to_hop(&self, name: &str, vault_root: &Path) -> anyhow::Result<Hop> {
        let set = [self.run.is_some(), self.store.is_some(), self.log.is_some(), self.emit.is_some(), self.link.is_some()]
            .iter()
            .filter(|b| **b)
            .count();
        if set != 1 {
            bail!("hop '{name}' must set exactly one of run, store, log, emit, link (sets {set})");
        }

        if let Some(command) = &self.run {
            // A command with a slash is a path inside the vault
            // (scripts/format-todo); a bare name resolves on PATH.
            let command = if command.contains('/') {
                vault_root.join(command).to_string_lossy().into_owned()
            } else {
                command.clone()
            };
            return Ok(Hop::RunScript { command, args: self.args.clone() });
        }

        if let Some(path) = &self.log {
            return Ok(Hop::Log { path: path.into() });
        }

        if self.emit == Some(true) {
            return Ok(Hop::Emit);
        }

        if let Some(link) = &self.link {
            return match link {
                LinkDef::Flag(true) => Ok(Hop::Link),
                LinkDef::Mode(m) if m == "add" => Ok(Hop::Link),
                LinkDef::Mode(m) if m == "replace" => Ok(Hop::LinkReplace),
                _ => bail!("hop '{name}': link must be true, \"add\" or \"replace\""),
            };
        }

        if self.store != Some(true) {
            bail!("hop '{name}': store must be true when set");
        }
        Ok(Hop::Store)
    }
}

// ── Resolved table ────────────────────────────────────────────────────────────

/// A route with its hop names already turned into Hops and its
/// defaults already appended.
#[derive(Debug, Clone)]
pub struct ResolvedRoute {
    pub data_type: String,
    pub dodo_id: String,
    pub nest_id: String,
    pub hops: Vec<Hop>,
    pub watchers: Vec<String>,
}

impl ResolvedRoute {
    /// A fresh one-Stop Route, ready for Dodo::run_route.
    pub fn to_route(&self) -> Route {
        Route::new(
            self.dodo_id.clone(),
            vec![Stop {
                nest_id: self.nest_id.clone(),
                hops: self.hops.clone(),
            }],
        )
        .watched_by(self.watchers.clone())
    }
}

/// Dodo has no Debug impl, so RoutingTable implements Debug by hand
/// (see below) rather than deriving it.
pub struct RoutingTable {
    dodos: HashMap<String, Dodo>,
    routes: HashMap<String, ResolvedRoute>,
}

impl std::fmt::Debug for RoutingTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoutingTable")
            .field("dodos", &self.dodos.len())
            .field("routes", &self.routes.len())
            .finish()
    }
}

impl RoutingTable {
    pub fn empty() -> Self {
        Self {
            dodos: HashMap::new(),
            routes: HashMap::new(),
        }
    }

    /// Load and validate routes.toml. A missing file gives an empty table
    /// (with a warning) so the server still boots. A file that exists but
    /// is invalid is an error: a half-applied routing table is worse
    /// than none.
    pub fn load(routes_path: &Path, vault_root: &Path, registry: &Registry) -> anyhow::Result<Self> {
        if !routes_path.exists() {
            tracing::warn!(
                path = %routes_path.display(),
                "no routes.toml — routing table is empty"
            );
            return Ok(Self::empty());
        }

        let text = std::fs::read_to_string(routes_path)
            .with_context(|| format!("reading {}", routes_path.display()))?;

        Self::from_toml(&text, vault_root, registry)
            .with_context(|| format!("invalid routes file {}", routes_path.display()))
    }

    pub fn from_toml(text: &str, vault_root: &Path, registry: &Registry) -> anyhow::Result<Self> {
        let file: RoutesFile = toml::from_str(text)?;

        // ── Hop bank ─────────────────────────────────────────────────────────
        let mut hop_bank: HashMap<String, Hop> = HashMap::new();
        for (name, def) in &file.hops {
            hop_bank.insert(name.clone(), def.to_hop(name, vault_root)?);
        }
        let hop = |name: &str| -> anyhow::Result<Hop> {
            hop_bank
                .get(name)
                .cloned()
                .with_context(|| format!("unknown hop '{name}'"))
        };

        // Check defaults up front, so a bad name fails even when no route
        // happens to use it.
        for name in &file.defaults.after {
            hop(name).context("[defaults] after")?;
        }

        // ── Dodos ────────────────────────────────────────────────────────────
        let mut dodos = HashMap::new();
        for (id, def) in &file.dodos {
            if !def.available_nests.contains(&def.home_nest) {
                bail!(
                    "dodo '{id}': home_nest '{}' must be in available_nests",
                    def.home_nest
                );
            }
            let home_nest = nest_from(registry, &def.home_nest)
                .with_context(|| format!("dodo '{id}' home_nest"))?;
            let available_nests = def
                .available_nests
                .iter()
                .map(|n| nest_from(registry, n).with_context(|| format!("dodo '{id}' available_nests")))
                .collect::<anyhow::Result<Vec<_>>>()?;

            dodos.insert(
                id.clone(),
                Dodo {
                    id: id.clone(),
                    home_nest,
                    available_nests,
                },
            );
        }

        // ── Routes ───────────────────────────────────────────────────────────
        let mut routes: HashMap<String, ResolvedRoute> = HashMap::new();
        for def in &file.routes {
            if routes.contains_key(&def.data_type) {
                bail!("duplicate route for data_type '{}'", def.data_type);
            }

            let dodo = dodos.get(&def.dodo).with_context(|| {
                format!("route '{}': unknown dodo '{}'", def.data_type, def.dodo)
            })?;

            if !dodo.can_visit(&def.nest) {
                bail!(
                    "route '{}': dodo '{}' is not allowed to visit nest '{}'",
                    def.data_type,
                    def.dodo,
                    def.nest
                );
            }

            for watcher in &def.watchers {
                registry.get(watcher).with_context(|| {
                    format!("route '{}': watcher nest '{}' not found in registry", def.data_type, watcher)
                })?;
            }

            let mut hop_names: Vec<&str> = def.hops.iter().map(String::as_str).collect();
            if !def.skip_defaults {
                hop_names.extend(file.defaults.after.iter().map(String::as_str));
            }
            let hops = hop_names
                .into_iter()
                .map(|n| hop(n))
                .collect::<anyhow::Result<Vec<_>>>()
                .with_context(|| format!("route '{}'", def.data_type))?;

            routes.insert(
                def.data_type.clone(),
                ResolvedRoute {
                    data_type: def.data_type.clone(),
                    dodo_id: def.dodo.clone(),
                    nest_id: def.nest.clone(),
                    hops,
                    watchers: def.watchers.clone(),
                },
            );
        }

        tracing::info!(
            dodos = dodos.len(),
            routes = routes.len(),
            hops = hop_bank.len(),
            "routing table loaded"
        );

        Ok(Self { dodos, routes })
    }

    /// The dodo and resolved route for a data type, if one is configured.
    /// O(1): a HashMap lookup, however many routes exist.
    pub fn resolve(&self, data_type: &str) -> Option<(&Dodo, &ResolvedRoute)> {
        let route = self.routes.get(data_type)?;
        let dodo = self.dodos.get(&route.dodo_id)?;
        Some((dodo, route))
    }

    pub fn route_count(&self) -> usize {
        self.routes.len()
    }
}

/// Look a nest up in the registry and wrap it as the Nest value Dodo uses.
fn nest_from(registry: &Registry, id: &str) -> anyhow::Result<Nest> {
    let manifest = registry
        .get(id)
        .with_context(|| format!("nest '{id}' not found in registry"))?;
    Ok(Nest {
        manifest: manifest.clone(),
        children: Vec::new(),
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{Envelope, Payload, Source};
    use crate::manifest::{MailroomConfig, Manifest, NodeKind, StoreKind};
    use crate::route::RouteStatus;
    use std::fs;

    fn manifest(id: &str, name: &str) -> Manifest {
        Manifest {
            id: id.to_string(),
            name: name.to_string(),
            path: None,
            kind: NodeKind::Leaf,
            accepts: Vec::new(),
            store: Some(StoreKind::Overwrite),
            routing: None,
            library: None,
            mailroom: Some(MailroomConfig {
                active: true,
                requires_auth: false,
                notify_on_write: false,
                ai_classify: false,
                mints_subnests: false,
                child_addressing: None,
            }),
            about: None,
            known_tags: Vec::new(),
            call_number: None,
            schema_version: None,
        }
    }

    /// A registry with the three nests the fixture routes point at.
    fn registry() -> Registry {
        let mut r = Registry::empty();
        r.insert("22".into(), manifest("22", "upkeep"));
        r.insert("20-C".into(), manifest("20-C", "schedule"));
        r.insert("35-A".into(), manifest("35-A", "Private-Health"));
        r.insert("88".into(), manifest("88", "logs"));
        r
    }

    const GOOD: &str = r#"
        [dodos.upkeep]
        home_nest       = "22"
        available_nests = ["22"]

        [hops.format-todo]
        run = "scripts/format-todo"

        [hops.store]
        store = true

        [hops.log]
        log = "logs/mailroom.jsonl"

        [defaults]
        after = ["log"]

        [[routes]]
        data_type = "text/todo"
        dodo      = "upkeep"
        nest      = "22"
        hops      = ["format-todo", "store"]
    "#;

    #[test]
    fn a_valid_table_resolves_hops_in_order_with_defaults_appended() {
        let table = RoutingTable::from_toml(GOOD, Path::new("/vault"), &registry()).unwrap();

        let (dodo, route) = table.resolve("text/todo").expect("route should exist");
        assert_eq!(dodo.id, "upkeep");
        assert_eq!(route.nest_id, "22");
        // format-todo, store, then the default log
        assert_eq!(route.hops.len(), 3);
        assert!(matches!(route.hops[0], Hop::RunScript { .. }));
        assert!(matches!(route.hops[1], Hop::Store));
        assert!(matches!(route.hops[2], Hop::Log { .. }));
    }

    #[test]
    fn an_unknown_data_type_resolves_to_none() {
        let table = RoutingTable::from_toml(GOOD, Path::new("/vault"), &registry()).unwrap();
        assert!(table.resolve("text/nonsense").is_none());
    }

    #[test]
    fn a_route_into_a_nest_off_the_allowlist_fails_at_load() {
        let bad = GOOD.replace(r#"nest      = "22""#, r#"nest      = "35-A""#);
        let err = RoutingTable::from_toml(&bad, Path::new("/vault"), &registry()).unwrap_err();
        assert!(format!("{err:#}").contains("not allowed to visit"), "got: {err:#}");
    }

    #[test]
    fn a_duplicate_data_type_fails_at_load() {
        let dup = format!(
            "{GOOD}\n[[routes]]\ndata_type = \"text/todo\"\ndodo = \"upkeep\"\nnest = \"22\"\nhops = [\"store\"]\n"
        );
        let err = RoutingTable::from_toml(&dup, Path::new("/vault"), &registry()).unwrap_err();
        assert!(format!("{err:#}").contains("duplicate route"), "got: {err:#}");
    }

    #[test]
    fn an_unknown_hop_name_fails_at_load() {
        let bad = GOOD.replace(r#"hops      = ["format-todo", "store"]"#, r#"hops      = ["no-such-hop"]"#);
        let err = RoutingTable::from_toml(&bad, Path::new("/vault"), &registry()).unwrap_err();
        assert!(format!("{err:#}").contains("unknown hop"), "got: {err:#}");
    }

    #[test]
    fn a_route_can_skip_the_default_hops() {
        let text = GOOD.replace(
            r#"hops      = ["format-todo", "store"]"#,
            "hops      = [\"store\"]\n        skip_defaults = true",
        );
        let table = RoutingTable::from_toml(&text, Path::new("/vault"), &registry()).unwrap();
        let (_, route) = table.resolve("text/todo").unwrap();
        assert_eq!(route.hops.len(), 1);
        assert!(matches!(route.hops[0], Hop::Store));
    }

    #[tokio::test]
    async fn dispatching_a_todo_stores_it_and_logs_it() {
        let vault = tempfile::tempdir().unwrap();
        crate::coffer::init(vault.path()).unwrap();

        // The nest on disk, so the Store hop has somewhere to write.
        let nest_dir = vault.path().join("22_upkeep");
        fs::create_dir_all(nest_dir.join("entries")).unwrap();
        fs::write(
            nest_dir.join("nest"),
            toml::to_string_pretty(&manifest("22", "upkeep")).unwrap(),
        )
        .unwrap();

        // Skip the format-todo script: a plain store-and-log route keeps
        // the test free of external commands.
        let text = GOOD.replace(
            r#"hops      = ["format-todo", "store"]"#,
            r#"hops      = ["store"]"#,
        );
        let table = RoutingTable::from_toml(&text, vault.path(), &registry()).unwrap();

        let envelope = Envelope::new(
            Source::Manual,
            "text/todo",
            Payload::Text("buy milk".to_string()),
            Some("22"),
        );

        let (dodo, resolved) = table.resolve("text/todo").unwrap();
        let mut route = resolved.to_route();
        dodo.run_route(&mut route, vault.path(), Some(&envelope))
            .await
            .unwrap();

        assert_eq!(route.status, RouteStatus::Completed);

        let has_entry = fs::read_dir(nest_dir.join("entries"))
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.path().extension().map(|x| x == "md").unwrap_or(false));
        assert!(has_entry, "store hop should have written an entry");

        let log = fs::read_to_string(vault.path().join("logs/mailroom.jsonl")).unwrap();
        assert_eq!(log.lines().count(), 1, "log hop should append one line");
        assert!(log.contains("text/todo"));
    }

    #[test]
    fn a_watcher_not_in_the_registry_fails_at_load() {
        let bad = GOOD.replace(
            r#"hops      = ["format-todo", "store"]"#,
            "hops      = [\"format-todo\", \"store\"]\n        watchers  = [\"no-such-nest\"]",
        );
        let err = RoutingTable::from_toml(&bad, Path::new("/vault"), &registry()).unwrap_err();
        assert!(format!("{err:#}").contains("not found in registry"), "got: {err:#}");
    }

    #[test]
    fn link_hops_accept_true_add_and_replace() {
        let text = format!(
            "{GOOD}\n[hops.a]\nlink = true\n[hops.b]\nlink = \"add\"\n[hops.c]\nlink = \"replace\"\n\
             [[routes]]\ndata_type = \"text/recipe\"\ndodo = \"upkeep\"\nnest = \"22\"\nhops = [\"a\", \"b\", \"c\"]\nskip_defaults = true\n"
        );
        let table = RoutingTable::from_toml(&text, Path::new("/vault"), &registry()).unwrap();
        let (_, route) = table.resolve("text/recipe").unwrap();
        assert!(matches!(route.hops[0], Hop::Link));
        assert!(matches!(route.hops[1], Hop::Link));
        assert!(matches!(route.hops[2], Hop::LinkReplace));
    }

    #[test]
    fn an_unknown_link_mode_fails_at_load() {
        for bad in ["link = false", "link = \"merge\""] {
            let text = format!("{GOOD}\n[hops.x]\n{bad}\n");
            let err = RoutingTable::from_toml(&text, Path::new("/vault"), &registry()).unwrap_err();
            assert!(format!("{err:#}").contains("link must be"), "{bad}: {err:#}");
        }
    }
}
