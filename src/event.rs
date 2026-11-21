// src/event.rs
//
// An Event is one edge between stations: a Dodo arriving at a nest, or a
// failure on the way there. Watchers record events; each event names the
// event that caused it (caused_by), so any record traces back to the
// intake that started the chain.
//
// Each nest has two channels for what it hears about:
//   logs/    arrivals (transitions) — what happened
//   errors/  failures — what went wrong, and where
// Recording here is best-effort: a watcher that cannot write a record logs
// a warning rather than failing the route that produced the event. These
// per-nest files are views. The record of truth is the vault's ledger
// (ledger.rs), and a failed ledger append does fail the route.

use std::io::Write;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A Dodo arrived at a stop. Recorded before the stop's hops run.
    Transition,
    /// Every hop at the stop succeeded. caused_by is the arrival.
    Completed,
    /// A hop at the stop failed. caused_by is the arrival.
    Error,
    /// Found at startup: the arrival was recorded, but neither Completed
    /// nor Error ever followed. caused_by is the arrival.
    Interrupted,
    /// A new envelope entered a route. Carries `envelope`: the facts that
    /// make it a node in the graph. Written once, before the first stop.
    Received,
    /// A Link hop recorded one edge. Carries `link`.
    Linked,
}

/// What the graph needs to know about an envelope to make it a node.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnvelopeFacts {
    /// The envelope's node id (see Envelope::node_id).
    pub node: String,
    pub data_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
}

impl EnvelopeFacts {
    pub fn of(envelope: &crate::envelope::Envelope) -> Self {
        EnvelopeFacts {
            node: envelope.node_id(),
            data_type: envelope.data_type.clone(),
            title: envelope.meta.get("title").cloned(),
            tags: envelope.tags.clone(),
            created_at: envelope.created_at,
        }
    }
}

/// One edge, as recorded by a Link hop.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LinkFacts {
    pub from: String,
    pub label: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub event_id: Uuid,
    pub from: String,
    pub to: String,
    pub caused_by: Option<Uuid>,
    pub envelope_id: Option<Uuid>,
    pub kind: EventKind,
    pub at: DateTime<Utc>,
    pub detail: Option<String>,
    /// SHA-256 of the envelope's content (see store::compute_content_hash).
    /// The envelope id says which record; this says which bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// The envelope this event's envelope was emitted from (fan-out).
    /// caused_by links events within one route; this links routes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope_parent: Option<Uuid>,
    /// Set on Received events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope: Option<EnvelopeFacts>,
    /// Set on Linked events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkFacts>,
}

impl Event {
    pub fn new(
        from: &str,
        to: &str,
        caused_by: Option<Uuid>,
        envelope_id: Option<Uuid>,
        kind: EventKind,
        detail: Option<String>,
    ) -> Self {
        Event {
            event_id: Uuid::new_v4(),
            from: from.to_string(),
            to: to.to_string(),
            caused_by,
            envelope_id,
            kind,
            at: Utc::now(),
            detail,
            content_hash: None,
            envelope_parent: None,
            envelope: None,
            link: None,
        }
    }

    /// Attach what is known about the envelope this event concerns.
    pub fn about(mut self, content_hash: Option<String>, envelope_parent: Option<Uuid>) -> Self {
        self.content_hash = content_hash;
        self.envelope_parent = envelope_parent;
        self
    }
}

/// Which of a nest's channels an event is written to.
#[derive(Debug, Clone, Copy)]
pub enum Channel {
    Logs,
    Errors,
}

impl Channel {
    fn dir_name(self) -> &'static str {
        match self {
            Channel::Logs => "logs",
            Channel::Errors => "errors",
        }
    }
}

/// Append one event, as a JSON line, to a nest's channel.
pub fn record(nest_dir: &Path, channel: Channel, event: &Event) {
    let result = (|| -> anyhow::Result<()> {
        let dir = nest_dir.join(channel.dir_name());
        std::fs::create_dir_all(&dir)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("events.jsonl"))?;
        writeln!(file, "{}", serde_json::to_string(event)?)?;
        Ok(())
    })();

    if let Err(e) = result {
        tracing::warn!(nest = %nest_dir.display(), error = %e, "failed to record event");
    }
}