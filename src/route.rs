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

use crate::envelope::{Envelope, Payload, Source};
use crate::event::{Channel, Event, EventKind};
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

    /// Append one JSON line of envelope metadata to a file under the vault.
    Log { path: std::path::PathBuf },

    /// Turn the preceding RunScript's receipt, a JSON array of items, into
    /// one envelope per item. The envelopes are kept on Route::emitted for
    /// the caller to dispatch. Every item is built before any is kept, so
    /// one malformed item emits nothing.
    Emit,

    /// Write one Linked event to the ledger for each link the envelope
    /// declares (Envelope::links). This is how edges get into the graph.
    Link,
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
    /// Nests that record this Route's events (see event.rs).
    pub watchers: Vec<String>,
    /// Envelopes produced by Emit hops, waiting for the caller to dispatch
    /// them. Empty until the route has run.
    pub emitted: Vec<Envelope>,
}

impl Route {
    /// A freshly created Route always starts NotStarted — there's no
    /// meaningful way to construct one already partway through.
    pub fn new(dodo_id: impl Into<String>, stops: Vec<Stop>) -> Self {
        Route {
            dodo_id: dodo_id.into(),
            stops,
            status: RouteStatus::NotStarted,
            watchers: Vec::new(),
            emitted: Vec::new(),
        }
    }

    /// Nests that record this Route's events — arrivals to their logs/,
    /// failures to their errors/.
    pub fn watched_by(mut self, watchers: Vec<String>) -> Self {
        self.watchers = watchers;
        self
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

        // Write order, per stop (this is what makes the ledger a
        // write-ahead log):
        //   1. append the arrival to the ledger and fsync
        //   2. run the hops
        //   3. append Completed (or Error) to the ledger and fsync
        // If the process dies between 1 and 3, the next startup finds an
        // arrival with nothing after it and records it as Interrupted
        // (ledger::Ledger::recover). If step 1 itself fails, no hop runs.
        let ledger = crate::ledger::for_vault(vault_root)?;

        // Which bytes this route is carrying. Hashed once here (a book is
        // read once, not once per stop) unless intake already did it.
        let content_hash = match envelope {
            Some(e) => match &e.content_hash {
                Some(hash) => Some(hash.clone()),
                None => crate::store::compute_content_hash(&e.payload).await?,
            },
            None => None,
        };
        let envelope_parent = envelope.and_then(|e| e.caused_by);

        // A new envelope becomes a node in the graph. Record what it is
        // once, before its first stop. A resumed route has already done
        // this, so it skips it.
        if start_index == 0 {
            if let Some(e) = envelope {
                let first_nest = self.stops.first().map(|s| s.nest_id.as_str()).unwrap_or("intake");
                let mut received = Event::new(
                    "intake", first_nest, None, Some(e.id), EventKind::Received, None,
                )
                .about(content_hash.clone(), envelope_parent);
                received.envelope = Some(crate::event::EnvelopeFacts::of(e));
                if let Err(err) = ledger.append(received) {
                    self.status = RouteStatus::Failed { stop_index: 0, reason: err.to_string() };
                    return Err(err);
                }
            }
        }

        // Every arrival is an event, and each event is caused by the one
        // before it. The first stop's cause is the intake itself.
        let envelope_id = envelope.map(|e| e.id);
        let mut previous_nest = if start_index == 0 {
            "intake".to_string()
        } else {
            self.stops[start_index - 1].nest_id.clone()
        };
        let mut last_event: Option<uuid::Uuid> = None;

        for stop_index in start_index..self.stops.len() {
            self.status = RouteStatus::InProgress { stop_index };

            // Cloned rather than borrowed: we need &mut self (to update
            // self.status) and a reference into self.stops at the same
            // time, which the borrow checker won't allow — a clone
            // sidesteps the conflict entirely. Stop derives Clone for
            // exactly this reason.
            let stop = self.stops[stop_index].clone();

            // Recorded before the hops run, so the chain reads in the
            // order things happened.
            let arrival = Event::new(
                &previous_nest, &stop.nest_id, last_event, envelope_id,
                EventKind::Transition, None,
            )
            .about(content_hash.clone(), envelope_parent);
            if let Err(e) = ledger.append(arrival.clone()) {
                self.status = RouteStatus::Failed { stop_index, reason: e.to_string() };
                return Err(e);
            }
            record_to_watchers(vault_root, &self.watchers, Channel::Logs, &arrival);
            last_event = Some(arrival.event_id);

            let emitted = match run_stop(&stop, vault_root, envelope).await {
                Ok(emitted) => emitted,
                Err(e) => {
                    let failure = Event::new(
                        &previous_nest, &stop.nest_id, last_event, envelope_id,
                        EventKind::Error, Some(e.to_string()),
                    )
                    .about(content_hash.clone(), envelope_parent);
                    // The route has already failed; if recording that also
                    // fails, report the original error. Startup recovery
                    // will mark the stop Interrupted.
                    if let Err(le) = ledger.append(failure.clone()) {
                        tracing::error!(error = %le, "could not record a failed stop in the ledger");
                    }
                    record_to_watchers(vault_root, &self.watchers, Channel::Errors, &failure);

                    self.status = RouteStatus::Failed {
                        stop_index,
                        reason: e.to_string(),
                    };
                    return Err(e);
                }
            };

            // Completion goes to the ledger only. The per-nest views keep
            // showing arrivals and errors, as before.
            let completed = Event::new(
                &stop.nest_id, &stop.nest_id, Some(arrival.event_id), envelope_id,
                EventKind::Completed, None,
            )
            .about(content_hash.clone(), envelope_parent);
            if let Err(e) = ledger.append(completed) {
                let reason = format!("stop ran, but its completion could not be recorded: {e}");
                self.status = RouteStatus::Failed { stop_index, reason: reason.clone() };
                return Err(anyhow::anyhow!(reason));
            }
            self.emitted.extend(emitted);

            previous_nest = stop.nest_id.clone();
        }

        self.status = RouteStatus::Completed;
        Ok(())
    }
}

/// Write an event into each watcher nest's channel. A watcher that
/// doesn't exist is a warning, not a failure of the route.
fn record_to_watchers(vault_root: &std::path::Path, watchers: &[String], channel: Channel, event: &Event) {
    for watcher in watchers {
        match crate::move_branch::find_nest_by_id(vault_root, watcher) {
            Ok(Some((nest_file, _))) => {
                if let Some(nest_dir) = nest_file.parent() {
                    crate::event::record(nest_dir, channel, event);
                }
            }
            Ok(None) => tracing::warn!(watcher = %watcher, "watcher nest not found — event not recorded"),
            Err(e) => tracing::warn!(watcher = %watcher, error = %e, "could not look up watcher nest"),
        }
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
/// Normalise and check one declared link.
///   label: "about", "taken at", "Part-Of" -> ABOUT, TAKEN_AT, PART_OF
///   to:    a UUID becomes "env:<uuid>"; anything else is a key, as given
fn link_facts(from: &str, link: &crate::envelope::LinkSpec) -> anyhow::Result<crate::event::LinkFacts> {
    let label = link.label.trim().to_uppercase().replace([' ', '-'], "_");
    if label.is_empty() || !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        anyhow::bail!("link label {:?} must be letters, digits and underscores (e.g. ABOUT, TAKEN_AT)", link.label);
    }
    let to = link.to.trim();
    if to.is_empty() {
        anyhow::bail!("link {label} has an empty target");
    }
    let to = match uuid::Uuid::parse_str(to) {
        Ok(id) => format!("env:{id}"),
        Err(_) => to.to_string(),
    };
    Ok(crate::event::LinkFacts { from: from.to_string(), label, to })
}

async fn run_stop(
    stop: &Stop,
    vault_root: &std::path::Path,
    envelope: Option<&Envelope>,
) -> anyhow::Result<Vec<Envelope>> {
    let mut current_nest_id = stop.nest_id.clone();
    let mut last_script_output: Option<String> = None;
    let mut last_receipt: Option<serde_json::Value> = None;
    let mut emitted: Vec<Envelope> = Vec::new();

    for hop in &stop.hops {
        match hop {
            Hop::RunScript { command, args } => {
                use std::io::Write;

                // Contract: the envelope arrives as JSON on stdin (null when
                // there is none). Stdout is the script's receipt, saved
                // by a later Log hop. Stderr is shown on failure. The exit
                // code decides pass or fail.
                let input = serde_json::to_vec(&envelope)?;

                let mut child = std::process::Command::new(command)
                    .args(args)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()?;

                // Written from a separate thread so a script that prints a
                // lot before reading its input cannot deadlock the pipe.
                let mut stdin = child.stdin.take().expect("stdin was piped");
                let writer = std::thread::spawn(move || stdin.write_all(&input));

                let output = child.wait_with_output()?;
                // A script that exits without reading stdin closes the pipe,
                // so the write may fail. That is not an error by itself:
                // the exit code below is what decides the outcome.
                let _ = writer.join();

                if !output.status.success() {
                    anyhow::bail!(
                        "script '{command}' exited with {}: {}",
                        output.status,
                        String::from_utf8_lossy(&output.stderr)
                    );
                }

                let stdout = String::from_utf8(output.stdout)?.trim().to_string();

                last_receipt = if stdout.is_empty() {
                    None
                } else {
                    match serde_json::from_str::<serde_json::Value>(&stdout) {
                        Ok(value) => Some(value),
                        Err(e) => {
                            tracing::warn!(
                                script = %command,
                                error = %e,
                                "script stdout is not JSON — no receipt recorded"
                            );
                            None
                        }
                    }
                };

                // MoveBranch still reads the raw trimmed stdout as a new id.
                last_script_output = Some(stdout);
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
                let (nest_file, _) = crate::move_branch::find_nest_by_id(vault_root, &current_nest_id)?
                    .ok_or_else(|| {
                        anyhow::anyhow!("Store hop: no nest found with id '{current_nest_id}'")
                    })?;

                // Store into the folder the nest file is actually in.
                // manifest.effective_path() is a name built from id + name
                // ("52-H_books"); it only matches the real folder when the
                // nest sits at the top of the vault, so a nest at
                // 52_media/52-H_books would get a stray 52-H_books/ at the
                // vault root.
                let nest_dir = nest_file.parent().unwrap_or(vault_root);
                let rel = nest_dir
                    .strip_prefix(vault_root)
                    .map_err(|_| anyhow::anyhow!("nest {} is outside the vault", nest_dir.display()))?
                    .to_string_lossy()
                    .replace('\\', "/");

                // vault_root doubles as library_root here, so blobs for
                // route-stored files land in <vault>/by-hash/.
                crate::store::store(envelope, vault_root, &rel).await?;
            }
            Hop::Link => {
                let envelope = envelope.ok_or_else(|| {
                    anyhow::anyhow!("Link hop needs an envelope whose links to record, but this Route was run with envelope: None")
                })?;
                // Check every link before writing any, so one bad link
                // records nothing rather than part of the set.
                let from = envelope.node_id();
                let links = envelope
                    .links
                    .iter()
                    .map(|l| link_facts(&from, l))
                    .collect::<anyhow::Result<Vec<_>>>()?;
                let ledger = crate::ledger::for_vault(vault_root)?;
                for link in links {
                    let mut event = Event::new(
                        &current_nest_id, &current_nest_id, None, Some(envelope.id),
                        EventKind::Linked, None,
                    )
                    .about(envelope.content_hash.clone(), envelope.caused_by);
                    event.link = Some(link);
                    ledger.append(event)?;
                }
            }
            Hop::Emit => {
                let items = last_receipt
                    .as_ref()
                    .and_then(|v| v.as_array())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Emit hop needs a preceding RunScript whose stdout is a JSON array of items"
                        )
                    })?;

                // Build every envelope before keeping any, so one bad item
                // emits nothing rather than part of the batch.
                let built = items
                    .iter()
                    .map(|item| envelope_from_item(item, envelope))
                    .collect::<anyhow::Result<Vec<_>>>()?;
                emitted.extend(built);
            }

            Hop::Log { path } => {
                let full = vault_root.join(path);
                if let Some(parent) = full.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let line = serde_json::json!({
                    "at": chrono::Utc::now(),
                    "nest": current_nest_id,
                    "receipt": last_receipt,
                    "envelope": envelope.map(|e| serde_json::json!({
                        "id": e.id,
                        "data_type": e.data_type,
                        "source": e.source,
                        "jd_address": e.jd_address,
                        "created_at": e.created_at,
                        "received_at": e.received_at,
                        "tags": e.tags,
                        "meta": e.meta,
                        "caused_by": e.caused_by,
                    })),
                });
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&full)?;
                writeln!(file, "{line}")?;
            }
        }
    }

    Ok(emitted)
}

/// Build one envelope from an item an Emit hop was handed. The item must
/// name a data_type and carry a payload. jd_address, tags and meta are
/// optional. `key` becomes meta "item_key", so a re-run can replace the
/// same item rather than duplicate it.
fn envelope_from_item(item: &serde_json::Value, parent: Option<&Envelope>) -> anyhow::Result<Envelope> {
    let data_type = item
        .get("data_type")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("emitted item has no data_type: {item}"))?;

    let payload: Payload = serde_json::from_value(
        item.get("payload").cloned().unwrap_or(serde_json::Value::Null),
    )
    .map_err(|e| anyhow::anyhow!("emitted item '{data_type}' has an invalid payload: {e}"))?;

    let jd_address = item.get("jd_address").and_then(|v| v.as_str()).map(String::from);

    let mut envelope = Envelope::new(
        Source::Other("fan-out".to_string()),
        data_type,
        payload,
        jd_address,
    );

    if let Some(tags) = item.get("tags").and_then(|v| v.as_array()) {
        envelope.tags = tags.iter().filter_map(|t| t.as_str().map(String::from)).collect();
    }
    if let Some(meta) = item.get("meta").and_then(|v| v.as_object()) {
        for (k, v) in meta {
            if let Some(s) = v.as_str() {
                envelope.meta.insert(k.clone(), s.to_string());
            }
        }
    }
    if let Some(key) = item.get("key").and_then(|v| v.as_str()) {
        envelope.meta.insert("item_key".to_string(), key.to_string());
    }

    envelope.caused_by = parent.map(|p| p.id);
    Ok(envelope)
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

    #[tokio::test]
    async fn run_script_receives_the_envelope_and_its_receipt_is_logged() {
        let vault = tempfile::tempdir().unwrap();
        let envelope = Envelope::new(
            Source::Manual,
            "text/todo",
            Payload::Text("buy milk".to_string()),
            Some("22"),
        );

        // The script saves what it received on stdin, then prints a receipt.
        let stdin_file = vault.path().join("stdin.json");
        let script = format!(
            "cat > '{}'; echo '{{\"action\":\"test\",\"status\":\"ok\"}}'",
            stdin_file.display()
        );

        let mut route = Route::new(
            "test-dodo",
            vec![Stop {
                nest_id: "22".to_string(),
                hops: vec![
                    Hop::RunScript {
                        command: "sh".to_string(),
                        args: vec!["-c".to_string(), script],
                    },
                    Hop::Log { path: "logs/test.jsonl".into() },
                ],
            }],
        );

        route.run(vault.path(), Some(&envelope)).await.unwrap();

        assert_eq!(route.status, RouteStatus::Completed);

        let received = fs::read_to_string(&stdin_file).unwrap();
        assert!(
            received.contains(&envelope.id.to_string()),
            "script should receive the envelope on stdin, got: {received}"
        );

        let log = fs::read_to_string(vault.path().join("logs/test.jsonl")).unwrap();
        assert!(
            log.contains("\"receipt\":{\"action\":\"test\",\"status\":\"ok\"}"),
            "log should carry the receipt, got: {log}"
        );
    }

    #[tokio::test]
    async fn emit_hop_turns_a_scripts_array_into_one_envelope_per_item() {
        let vault = tempfile::tempdir().unwrap();
        let parent = Envelope::new(
            Source::Manual,
            "text/journal",
            Payload::Text("2026/10/22 lunch and a workout".to_string()),
            Some("34-B"),
        );

        let items = r#"[{"data_type":"data/food-intake","payload":{"kind":"text","data":"chicken burrito"},"key":"lunch-1"},{"data_type":"data/weight","payload":{"kind":"json","data":{"lb":180}},"tags":["morning"]}]"#;

        let mut route = Route::new(
            "test-dodo",
            vec![Stop {
                nest_id: "34-B".to_string(),
                hops: vec![
                    Hop::RunScript {
                        command: "sh".to_string(),
                        args: vec!["-c".to_string(), format!("echo '{items}'")],
                    },
                    Hop::Emit,
                ],
            }],
        );

        route.run(vault.path(), Some(&parent)).await.unwrap();

        assert_eq!(route.status, RouteStatus::Completed);
        assert_eq!(route.emitted.len(), 2);
        assert_eq!(route.emitted[0].data_type, "data/food-intake");
        assert_eq!(route.emitted[0].caused_by, Some(parent.id));
        assert_eq!(route.emitted[0].meta.get("item_key").map(String::as_str), Some("lunch-1"));
        assert_eq!(route.emitted[1].data_type, "data/weight");
        assert_eq!(route.emitted[1].tags, vec!["morning".to_string()]);
    }

    #[tokio::test]
    async fn one_malformed_item_emits_nothing_and_fails_the_route() {
        let vault = tempfile::tempdir().unwrap();

        // The second item has no data_type. Nothing from the batch should be
        // kept, not even the valid first item.
        let items = r#"[{"data_type":"data/weight","payload":{"kind":"json","data":{"lb":180}}},{"payload":{"kind":"text","data":"no type"}}]"#;

        let mut route = Route::new(
            "test-dodo",
            vec![Stop {
                nest_id: "34-B".to_string(),
                hops: vec![
                    Hop::RunScript {
                        command: "sh".to_string(),
                        args: vec!["-c".to_string(), format!("echo '{items}'")],
                    },
                    Hop::Emit,
                ],
            }],
        );

        assert!(route.run(vault.path(), None).await.is_err());
        assert!(matches!(route.status, RouteStatus::Failed { .. }));
        assert!(route.emitted.is_empty(), "a failed batch must emit nothing");
    }

    fn read_events(path: &Path) -> Vec<serde_json::Value> {
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn each_arrival_is_recorded_to_the_watcher_chained_by_cause() {
        let vault = tempfile::tempdir().unwrap();
        seed_nest(vault.path(), "22_upkeep", &test_manifest("22", "upkeep"));
        seed_nest(vault.path(), "52_media", &test_manifest("52", "media"));
        seed_nest(vault.path(), "91_Logging", &test_manifest("91", "Logging"));

        let mut route = Route::new(
            "test-dodo",
            vec![
                Stop { nest_id: "22".to_string(), hops: vec![] },
                Stop { nest_id: "52".to_string(), hops: vec![] },
            ],
        )
        .watched_by(vec!["91".to_string()]);

        route.run(vault.path(), None).await.unwrap();

        let events = read_events(&vault.path().join("91_Logging/logs/events.jsonl"));
        assert_eq!(events.len(), 2, "one arrival per stop");
        assert_eq!(events[0]["from"], "intake");
        assert_eq!(events[0]["to"], "22");
        assert_eq!(events[0]["kind"], "transition");
        assert_eq!(events[1]["from"], "22");
        assert_eq!(events[1]["to"], "52");
        assert_eq!(events[1]["caused_by"], events[0]["event_id"]);
    }

    #[tokio::test]
    async fn a_failing_stop_is_recorded_to_the_watchers_errors_channel() {
        let vault = tempfile::tempdir().unwrap();
        seed_nest(vault.path(), "91_Logging", &test_manifest("91", "Logging"));

        let mut route = Route::new(
            "test-dodo",
            vec![Stop {
                nest_id: "22".to_string(),
                hops: vec![Hop::RunScript { command: "false".to_string(), args: vec![] }],
            }],
        )
        .watched_by(vec!["91".to_string()]);

        assert!(route.run(vault.path(), None).await.is_err());

        let arrivals = read_events(&vault.path().join("91_Logging/logs/events.jsonl"));
        assert_eq!(arrivals.len(), 1, "the arrival is still logged");

        let errors = read_events(&vault.path().join("91_Logging/errors/events.jsonl"));
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["kind"], "error");
        assert_eq!(errors[0]["to"], "22");
        assert_eq!(errors[0]["caused_by"], arrivals[0]["event_id"]);
    }
}