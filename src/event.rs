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
// Recording is best-effort: a watcher that cannot write a record logs a
// warning rather than failing the route that produced the event.

use std::io::Write;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Transition,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub event_id: Uuid,
    pub from: String,
    pub to: String,
    pub caused_by: Option<Uuid>,
    pub envelope_id: Option<Uuid>,
    pub kind: EventKind,
    pub at: DateTime<Utc>,
    pub detail: Option<String>,
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
        }
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