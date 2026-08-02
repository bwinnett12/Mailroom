// src/route.rs
//
// A Route is a journey: which Dodo is making it, which Stations it
// visits (in order, as Stops), and — for anything that isn't
// instantaneous — where the Dodo currently stands.
//
// Two Rust ideas worth calling out since this file exists partly as a
// learning exercise:
//
//   Enums as state machines. RouteStatus isn't just a label — each
//   variant carries exactly the data that's meaningful for that state
//   (InProgress needs to know *which* stop; NotStarted/Completed don't
//   need anything). This is a common Rust pattern: instead of a status
//   string plus a separate "current stop" field that could be nonsense
//   when status is Completed, the enum makes the invalid combination
//   unrepresentable — you literally cannot construct
//   `RouteStatus::Completed` with a stop index attached, because that
//   variant has no fields to put one in.
//
//   Vec<T> for "a sequence of things where order matters and the
//   length isn't known up front." A Route's stops are always processed
//   in order, and different routes have different numbers of stops —
//   exactly what Vec is for, versus e.g. a fixed-size array.

use crate::envelope::Envelope;
use crate::move_branch::MergeStrategy;

/// A single step within a Stop. One Stop can have several Hops —
/// e.g. "run the classification script" then "re-address via
/// move_branch" are two Hops at the same Stop, not two Stops.
#[derive(Debug, Clone)]
pub enum Hop {
    /// Shell out to an external script — the actual domain logic (a
    /// classifier, a converter, whatever) lives outside Rust entirely.
    /// Same philosophy as inference.rs's task table: Mailroom
    /// orchestrates, an external tool does the specialized work.
    RunScript { command: String, args: Vec<String> },

    /// Re-address this Stop's nest once something upstream (usually
    /// the preceding RunScript Hop) produced a real id for it.
    MoveBranch { merge_strategy: MergeStrategy },

    /// Write the envelope being delivered into this Stop's nest — the
    /// same write store::store() already performs directly for simple
    /// form submissions. Needs an actual Envelope to be passed to
    /// Route::run(); a Route whose Stops include a Store hop but is run
    /// with envelope: None fails clearly rather than silently no-op'ing.
    Store,
}

/// One Station a Route visits, and what to do once there.
#[derive(Debug, Clone)]
pub struct Stop {
    pub nest_id: String,
    pub hops: Vec<Hop>,
}

/// Where a Dodo currently stands on a Route. Exists because not every
/// Route finishes instantly — "review the last three weeks of notes,
/// deliver to an LLM for a set of tasks" can take real wall-clock time,
/// and something needs to be able to answer "where's the Dodo right
/// now" while that's in flight.
#[derive(Debug, Clone, PartialEq)]
pub enum RouteStatus {
    /// No Hop has run yet.
    NotStarted,
    /// Currently working through the Stop at this index into
    /// `Route::stops` (0-based).
    InProgress { stop_index: usize },
    /// Every Stop finished successfully.
    Completed,
    /// Stopped partway through — which Stop, and why, so a failure
    /// doesn't just vanish silently.
    Failed { stop_index: usize, reason: String },
}

/// A journey: the Dodo making it, the Stops it visits in order, and
/// its current status.
#[derive(Debug, Clone)]
pub struct Route {
    /// Which Dodo this Route belongs to — see Dodo::authorize(), which
    /// checks this against the Dodo actually trying to run it before
    /// a single Hop executes.
    pub dodo_id: String,
    pub stops: Vec<Stop>,
    pub status: RouteStatus,
}

impl Route {
    /// A freshly created Route always starts NotStarted — there's no
    /// meaningful way to construct one already partway through.
    pub fn new(dodo_id: impl Into<String>, stops: Vec<Stop>) -> Self {
        Route {
            dodo_id: dodo_id.into(),
            stops,
            status: RouteStatus::NotStarted,
        }
    }

    /// The Stop the Dodo is currently at, if the Route is actually in
    /// progress. Returns None for NotStarted/Completed/Failed — there's
    /// no "current stop" that makes sense in those states, which is
    /// exactly why this returns Option<&Stop> rather than assuming the
    /// caller already knows the status is InProgress.
    pub fn current_stop(&self) -> Option<&Stop> {
        match self.status {
            RouteStatus::InProgress { stop_index } => self.stops.get(stop_index),
            _ => None,
        }
    }

    /// Run every remaining Stop on this Route, updating `status` as it
    /// goes. If the Route was already Completed, or previously Failed,
    /// this is a no-op that returns immediately — there's nothing left
    /// to (re)run automatically; a Failed route needs a human decision
    /// to retry, not a silent re-attempt.
    ///
    /// `envelope` is the thing being delivered, if this Route includes
    /// any `Hop::Store` — `None` for Routes that only do
    /// classification/re-addressing work on an already-existing item
    /// (nothing new is arriving, so there's nothing to store).
    ///
    /// Design choice worth naming explicitly rather than leaving
    /// implicit: a Hop failure aborts the *whole* Route, it doesn't
    /// skip ahead to the next Stop. Reasoning: if a classification
    /// script fails partway through, letting a later Stop's
    /// MoveBranch Hop run anyway risks acting on stale or wrong data.
    /// Safer to stop everything and let a human look at `status`.
    ///
    /// async because Hop::Store calls store::store(), which is itself
    /// async (it does real file I/O via tokio). Hop::RunScript and
    /// Hop::MoveBranch stay synchronous underneath (std::process,
    /// std::fs) — blocking briefly inside an async fn is fine for
    /// occasional, administrative-style operations like these, not the
    /// same concern it'd be on a hot request path.
    pub async fn run(
        &mut self,
        vault_root: &std::path::Path,
        envelope: Option<&Envelope>,
    ) -> anyhow::Result<()> {
        let start_index = match self.status {
            RouteStatus::NotStarted => 0,
            RouteStatus::InProgress { stop_index } => stop_index,
            RouteStatus::Completed | RouteStatus::Failed { .. } => return Ok(()),
        };

        for stop_index in start_index..self.stops.len() {
            self.status = RouteStatus::InProgress { stop_index };

            // Cloned rather than borrowed: we need &mut self (to update
            // self.status) and a reference into self.stops at the same
            // time, which the borrow checker won't allow — a clone
            // sidesteps the conflict entirely. Stop derives Clone for
            // exactly this reason.
            let stop = self.stops[stop_index].clone();

            if let Err(e) = run_stop(&stop, vault_root, envelope).await {
                self.status = RouteStatus::Failed {
                    stop_index,
                    reason: e.to_string(),
                };
                return Err(e);
            }
        }

        self.status = RouteStatus::Completed;
        Ok(())
    }
}

/// Run every Hop at one Stop, in order.
///
/// Two pieces of running state live only inside this function, not on
/// Route/Stop themselves, because they only matter transiently while
/// hops are actually executing:
///
///   current_nest_id — which id we're actually acting on right now. It
///   starts as the Stop's own nest_id, but a MoveBranch Hop can change
///   it, so any *later* Hop at this same Stop needs to act on the
///   post-move id, not the original one.
///
///   last_script_output — the most recent RunScript Hop's stdout,
///   trimmed. This is how a classification script's output (expected to
///   be just the new id, nothing else) reaches the MoveBranch Hop that
///   follows it — None until a RunScript Hop has actually produced
///   something.
async fn run_stop(
    stop: &Stop,
    vault_root: &std::path::Path,
    envelope: Option<&Envelope>,
) -> anyhow::Result<()> {
    let mut current_nest_id = stop.nest_id.clone();
    let mut last_script_output: Option<String> = None;

    for hop in &stop.hops {
        match hop {
            Hop::RunScript { command, args } => {
                let output = std::process::Command::new(command)
                    .args(args)
                    .output()?;

                if !output.status.success() {
                    anyhow::bail!(
                        "script '{command}' exited with {}: {}",
                        output.status,
                        String::from_utf8_lossy(&output.stderr)
                    );
                }

                let stdout = String::from_utf8(output.stdout)?;
                last_script_output = Some(stdout.trim().to_string());
            }

            Hop::MoveBranch { merge_strategy } => {
                let to_id = last_script_output.as_ref().ok_or_else(|| {
                    anyhow::anyhow!(
                        "MoveBranch hop needs a preceding RunScript hop's \
                         output to know the new id — none was produced"
                    )
                })?;

                let report = crate::move_branch::move_branch(
                    vault_root,
                    &current_nest_id,
                    to_id,
                    *merge_strategy,
                    false, // not a dry run — this is a Route actually executing
                )?;

                // Any further Hop at this Stop should act on wherever
                // the nest actually ended up, not its old id.
                current_nest_id = report.to_id.clone();
            }

            Hop::Store => {
                let envelope = envelope.ok_or_else(|| {
                    anyhow::anyhow!(
                        "Store hop needs an envelope to deliver, but this \
                         Route was run with envelope: None"
                    )
                })?;

                // Looks the nest up fresh rather than trusting a cached
                // path — reuses the same vault-wide search move_branch
                // already does for the same reason (a nest's id is the
                // only thing guaranteed stable; its physical location
                // can change).
                let (_, manifest) = crate::move_branch::find_nest_by_id(vault_root, &current_nest_id)?
                    .ok_or_else(|| {
                        anyhow::anyhow!("Store hop: no nest found with id '{current_nest_id}'")
                    })?;

                // vault_root doubles as library_root here — every test
                // and every real run this session has used the same
                // path for both; see the known caveat already flagged
                // elsewhere (attendant.rs/move_branch.rs) if a real
                // deployment ever needs them to genuinely diverge.
                crate::store::store(envelope, vault_root, &manifest.effective_path()).await?;
            }
        }
    }

    Ok(())
}

// ── route.rs tests — REPLACE the existing `mod tests` block at the end
// of src/route.rs with this one. The signature of run()/run_stop()
// changed (now async, now takes an envelope parameter), so the three
// existing tests need `.await` and a `None` argument added — this
// isn't new breakage, just catching them up to the new shape. One new
// test (store_hop_actually_delivers_the_envelope) is added at the end.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{Envelope, Payload, Source};
    use crate::manifest::{MailroomConfig, Manifest, NodeKind, StoreKind};
    use std::fs;
    use std::path::{Path, PathBuf};

    fn test_manifest(id: &str, name: &str) -> Manifest {
        Manifest {
            id: id.to_string(),
            name: name.to_string(),
            path: None,
            kind: NodeKind::Leaf,
            accepts: vec!["media/book".to_string()],
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

    fn seed_nest(vault_root: &Path, folder_name: &str, manifest: &Manifest) -> PathBuf {
        let dir = vault_root.join(folder_name);
        fs::create_dir_all(dir.join("entries")).unwrap();
        fs::write(dir.join("nest"), toml::to_string_pretty(manifest).unwrap()).unwrap();
        dir
    }

    #[tokio::test]
    async fn a_full_route_reaches_completed_and_moves_the_nest() {
        let vault = tempfile::tempdir().unwrap();
        let manifest = test_manifest("52-B-aaaa1111", "Old Name");
        let old_dir = seed_nest(vault.path(), "52-B-aaaa1111_Old-Name", &manifest);

        let mut route = Route::new(
            "test-dodo",
            vec![Stop {
                nest_id: "52-B-aaaa1111".to_string(),
                hops: vec![
                    // Stands in for a real classification script — just
                    // prints the "decided" new id to stdout.
                    Hop::RunScript {
                        command: "echo".to_string(),
                        args: vec!["663.44".to_string()],
                    },
                    Hop::MoveBranch {
                        merge_strategy: crate::move_branch::MergeStrategy::Compare,
                    },
                ],
            }],
        );

        route.run(vault.path(), None).await.unwrap();

        assert_eq!(route.status, RouteStatus::Completed);
        assert!(!old_dir.exists(), "original location should be gone after the move");
        assert!(vault.path().join("663.44_Old-Name").exists());
    }

    #[tokio::test]
    async fn a_failing_script_marks_the_route_failed_at_the_right_stop() {
        let vault = tempfile::tempdir().unwrap();
        let manifest = test_manifest("52-B-bbbb2222", "Doomed Book");
        seed_nest(vault.path(), "52-B-bbbb2222_Doomed-Book", &manifest);

        let mut route = Route::new(
            "test-dodo",
            vec![Stop {
                nest_id: "52-B-bbbb2222".to_string(),
                // "false" is a real command that always exits non-zero —
                // available on every unix-like system, perfect for
                // deliberately testing the failure path.
                hops: vec![Hop::RunScript {
                    command: "false".to_string(),
                    args: vec![],
                }],
            }],
        );

        let result = route.run(vault.path(), None).await;

        assert!(result.is_err());
        match &route.status {
            RouteStatus::Failed { stop_index, .. } => assert_eq!(*stop_index, 0),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resuming_an_in_progress_route_skips_already_completed_stops() {
        let vault = tempfile::tempdir().unwrap();
        // Deliberately no nest seeded for "already-done" — if run()
        // incorrectly tried to redo stop 0, it would fail immediately
        // with "no nest file found," since nothing was ever created
        // for that id. If it correctly resumes at stop_index 1, this
        // absence is never even noticed.
        let manifest = test_manifest("52-B-cccc3333", "Second Stop");
        seed_nest(vault.path(), "52-B-cccc3333_Second-Stop", &manifest);

        let mut route = Route::new(
            "test-dodo",
            vec![
                Stop {
                    nest_id: "already-done-and-gone".to_string(),
                    hops: vec![Hop::RunScript {
                        command: "false".to_string(), // would fail if actually run
                        args: vec![],
                    }],
                },
                Stop {
                    nest_id: "52-B-cccc3333".to_string(),
                    hops: vec![
                        Hop::RunScript {
                            command: "echo".to_string(),
                            args: vec!["663.45".to_string()],
                        },
                        Hop::MoveBranch {
                            merge_strategy: crate::move_branch::MergeStrategy::Compare,
                        },
                    ],
                },
            ],
        );
        route.status = RouteStatus::InProgress { stop_index: 1 };

        route.run(vault.path(), None).await.unwrap();

        assert_eq!(route.status, RouteStatus::Completed);
        assert!(vault.path().join("663.45_Second-Stop").exists());
    }

    #[tokio::test]
    async fn store_hop_actually_delivers_the_envelope() {
        let vault = tempfile::tempdir().unwrap();
        let manifest = test_manifest("34.2", "Journal");
        seed_nest(vault.path(), "34.2_Journal", &manifest);

        let envelope = Envelope::new(
            Source::Manual,
            "text/journal",
            Payload::Text("delivered via a Store hop".to_string()),
            Some("34.2"),
        );

        let mut route = Route::new(
            "test-dodo",
            vec![Stop {
                nest_id: "34.2".to_string(),
                hops: vec![Hop::Store],
            }],
        );

        route.run(vault.path(), Some(&envelope)).await.unwrap();

        assert_eq!(route.status, RouteStatus::Completed);
        let entries_dir = vault.path().join("34.2_Journal/entries");
        let has_entry = fs::read_dir(&entries_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.path().extension().map(|ext| ext == "md").unwrap_or(false));
        assert!(has_entry, "expected a .md entry written by the Store hop");
    }

    #[tokio::test]
    async fn store_hop_without_an_envelope_fails_clearly() {
        let vault = tempfile::tempdir().unwrap();
        let manifest = test_manifest("34.2", "Journal");
        seed_nest(vault.path(), "34.2_Journal", &manifest);

        let mut route = Route::new(
            "test-dodo",
            vec![Stop {
                nest_id: "34.2".to_string(),
                hops: vec![Hop::Store],
            }],
        );

        let result = route.run(vault.path(), None).await;
        assert!(result.is_err(), "Store hop with no envelope must fail, not silently no-op");
    }
}

