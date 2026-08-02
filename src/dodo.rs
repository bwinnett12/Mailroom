// src/dodo.rs
//
// A Dodo has an identity and a fixed set of Nests it's actually
// permitted to visit — available_nests isn't a cache of nests it
// happens to know about, it's an allowlist. A Dodo has no legitimate
// reason to be sent down some random, unrelated, or private path it
// was never granted access to (think: a book-classification Dodo being
// handed a Route into 35_Health by mistake) — so a Route naming a Stop
// outside this list gets rejected before a single Hop runs, rather than
// discovered partway through.

use crate::nest::Nest;
use crate::route::Route;

// TODO: Dodo types/variants — not every Dodo does the same job. At
// minimum: an "alert" Dodo that sends outbound notifications rather
// than moving content, a "pickup" Dodo that gathers information from a
// Station without necessarily acting on it, and an "event" Dodo that
// waits for a trigger rather than running immediately (the systemd-timer
// dodos already sketched in the backlog are a special case of this).
// Probably an enum discriminating Dodo "kind," or a trait if the
// behavior differs enough per kind to want separate impls — decide once
// a second real kind actually needs building, not before.

pub struct Dodo {
    pub id: String,
    pub home_nest: Nest,
    pub available_nests: Vec<Nest>,
}

impl Dodo {
    pub fn get_home_nest(&self) -> &Nest {
        &self.home_nest
    }

    /// Is this Dodo actually allowed to visit the given nest id?
    /// `.iter().any(...)` short-circuits on the first match — for a
    /// realistic-sized allowlist this is plenty fast without needing
    /// a HashSet.
    pub fn can_visit(&self, nest_id: &str) -> bool {
        self.available_nests
            .iter()
            .any(|n| n.manifest.id == nest_id)
    }

    /// Check that a Route is actually this Dodo's to run — both that
    /// it's assigned to this Dodo specifically, and that every Stop on
    /// it is somewhere this Dodo is authorized to go. Returns Err with
    /// a human-readable reason rather than just `bool`, so a caller can
    /// report *why* a Route was rejected, not just that it was.
    pub fn authorize(&self, route: &Route) -> Result<(), String> {
        if route.dodo_id != self.id {
            return Err(format!(
                "route is assigned to dodo '{}', not '{}'",
                route.dodo_id, self.id
            ));
        }

        for stop in &route.stops {
            if !self.can_visit(&stop.nest_id) {
                return Err(format!(
                    "dodo '{}' is not authorized to visit '{}'",
                    self.id, stop.nest_id
                ));
            }
        }

        Ok(())
    }

    /// The real entry point — call this, not Route::run() directly.
    /// Authorizes the route against this Dodo first (both that the
    /// route is actually assigned to it, and that every Stop is
    /// somewhere it's allowed to go), and only then runs it. Calling
    /// Route::run() straight from outside this method would skip that
    /// check entirely — nothing currently stops you from doing that,
    /// which is exactly the gap this method exists to close.
    ///
    /// `envelope` is passed straight through to Route::run() — see its
    /// own doc comment for when it's needed (Hop::Store) vs. None.
    pub async fn run_route(
        &self,
        route: &mut Route,
        vault_root: &std::path::Path,
        envelope: Option<&crate::envelope::Envelope>,
    ) -> anyhow::Result<()> {
        self.authorize(route).map_err(|e| anyhow::anyhow!(e))?;
        route.run(vault_root, envelope).await
    }
}

// ── dodo.rs tests — REPLACE the existing `mod tests` block at the end
// of src/dodo.rs with this one. Only run_route_never_touches_disk_...
// needed updating (async + envelope param); the other two are unchanged.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{MailroomConfig, Manifest, NodeKind, StoreKind};
    use crate::route::{Hop, RouteStatus, Stop};
    use std::fs;

    fn test_manifest(id: &str, name: &str) -> Manifest {
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
        }
    }

    fn test_nest(id: &str, name: &str) -> Nest {
        Nest { manifest: test_manifest(id, name), children: Vec::new() }
    }

    #[test]
    fn can_visit_only_returns_true_for_nests_on_the_allowlist() {
        let dodo = Dodo {
            id: "classifier".to_string(),
            home_nest: test_nest("82.2", "Unclassified"),
            available_nests: vec![test_nest("52-B", "Books")],
        };

        assert!(dodo.can_visit("52-B"));
        assert!(!dodo.can_visit("35_Health"), "not on the allowlist — must be rejected");
    }

    #[test]
    fn authorize_rejects_a_route_assigned_to_a_different_dodo() {
        let dodo = Dodo {
            id: "classifier".to_string(),
            home_nest: test_nest("82.2", "Unclassified"),
            available_nests: vec![test_nest("52-B", "Books")],
        };

        let route = Route::new("some-other-dodo", vec![Stop {
            nest_id: "52-B".to_string(),
            hops: vec![],
        }]);

        assert!(dodo.authorize(&route).is_err());
    }

    #[tokio::test]
    async fn run_route_never_touches_disk_when_authorization_fails() {
        let vault = tempfile::tempdir().unwrap();
        let manifest = test_manifest("35-A", "Private Health Note");
        let dir = vault.path().join("35-A_Private-Health-Note");
        fs::create_dir_all(dir.join("entries")).unwrap();
        fs::write(dir.join("nest"), toml::to_string_pretty(&manifest).unwrap()).unwrap();

        // This Dodo is only allowed near books — 35-A (Health) is
        // deliberately outside its allowlist.
        let dodo = Dodo {
            id: "classifier".to_string(),
            home_nest: test_nest("82.2", "Unclassified"),
            available_nests: vec![test_nest("52-B", "Books")],
        };

        let mut route = Route::new("classifier", vec![Stop {
            nest_id: "35-A".to_string(),
            hops: vec![Hop::RunScript {
                command: "echo".to_string(),
                args: vec!["would-have-moved-this".to_string()],
            }],
        }]);

        let result = dodo.run_route(&mut route, vault.path(), None).await;

        assert!(result.is_err(), "should be rejected before any Hop runs");
        assert_eq!(route.status, RouteStatus::NotStarted, "must not have started executing");
        assert!(dir.exists(), "the nest must be completely untouched");
    }
}
