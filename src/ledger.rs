// src/ledger.rs
//
// The ledger: one append-only, ordered, durable record of every event in a
// vault. It is Mailroom's write-ahead log and the source of truth that the
// graph (and later, replication) is built from.
//
//   <vault>/.mailroom/ledger/events.jsonl   one Record per line
//   <vault>/.mailroom/ledger/ledger.lock    held for as long as a process
//                                           has the ledger open
//
// Guarantees, and how each one is kept:
//
//   Ordered     every record gets the next `seq`. One writer per vault:
//               inside a process, `for_vault` hands out a single shared
//               Ledger; across processes, the lock file refuses a second
//               Mailroom on the same vault.
//
//   Durable     `append` returns only after fsync (sync_data). A record
//               the caller has seen succeed survives a crash or power cut.
//
//   Complete    a crash mid-append can leave a partial last line. `open`
//               finds that unterminated fragment and cuts it off; the
//               caller never saw that append succeed, so nothing is lost.
//               A *terminated* line that does not parse is not a torn
//               write — it is corruption — and `open` refuses to continue
//               rather than silently dropping history.
//
//   Versioned   every record carries `v`. Readers tolerate unknown fields,
//               so new fields can be added without breaking old records.
//
// The per-nest logs/ and errors/ files written by event::record still
// exist. They are views for humans browsing a nest; the ledger is the
// record. A failed ledger append fails the route. A failed view write
// is only a warning.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::event::{Event, EventKind};

/// Bump when the meaning of an existing field changes. Adding a field
/// with a serde default does not need a bump.
pub const SCHEMA_VERSION: u32 = 1;

/// One line of the ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    /// Schema version this record was written under.
    pub v: u32,
    /// Position in the ledger: 1, 2, 3, … with no gaps.
    pub seq: u64,
    /// Which machine wrote it. Every device keeps its own ledger, so
    /// this matters once ledgers are compared during replication.
    pub device: String,
    #[serde(flatten)]
    pub event: Event,
}

pub struct Ledger {
    path: PathBuf,
    device: String,
    inner: Mutex<Inner>,
    // Held, never read: dropping the Ledger releases the lock.
    _lock: File,
}

struct Inner {
    file: File,
    next_seq: u64,
    /// Bytes known to be good. A failed append truncates back to this.
    len: u64,
}

impl Ledger {
    /// Open (creating if needed) the ledger for a vault, repairing a torn
    /// final line. Fails if another process holds the ledger, or if a
    /// complete line is corrupt.
    pub fn open(vault_root: &Path) -> anyhow::Result<Ledger> {
        let dir = vault_root.join(".mailroom").join("ledger");
        std::fs::create_dir_all(&dir)?;

        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("ledger.lock"))?;
        if let Err(e) = lock.try_lock() {
            anyhow::bail!(
                "the ledger at {} is in use by another Mailroom process ({e}). \
                 Two processes writing one vault would interleave history; \
                 stop the other one, or point this one at a different vault",
                dir.display()
            );
        }

        let path = dir.join("events.jsonl");
        let existed = path.exists();
        let (records, good_len, torn_bytes) = scan(&path)?;

        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        if torn_bytes > 0 {
            tracing::warn!(
                ledger = %path.display(),
                bytes = torn_bytes,
                "ledger ended in a partial line from an interrupted write — removing it"
            );
            file.set_len(good_len)?;
            file.sync_all()?;
        }
        if !existed {
            // Make the new file's directory entry durable too, not just its
            // contents.
            File::open(&dir)?.sync_all()?;
        }

        let next_seq = records.last().map(|r| r.seq + 1).unwrap_or(1);

        Ok(Ledger {
            path,
            device: device_name(),
            inner: Mutex::new(Inner {
                file,
                next_seq,
                len: good_len,
            }),
            _lock: lock,
        })
    }

    /// Append one event. Returns once the record is on disk.
    pub fn append(&self, event: Event) -> anyhow::Result<Record> {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let record = Record {
            v: SCHEMA_VERSION,
            seq: inner.next_seq,
            device: self.device.clone(),
            event,
        };
        let mut line = serde_json::to_string(&record)?;
        line.push('\n');

        let written = inner
            .file
            .write_all(line.as_bytes())
            .and_then(|_| inner.file.sync_data());
        if let Err(e) = written {
            // Cut off whatever part of the line made it to disk, so the
            // next append does not land after a fragment.
            let len = inner.len;
            let _ = inner.file.set_len(len);
            return Err(anyhow::anyhow!(e).context("ledger append failed"));
        }

        inner.len += line.len() as u64;
        inner.next_seq += 1;
        Ok(record)
    }

    /// Every record, in order.
    pub fn records(&self) -> anyhow::Result<Vec<Record>> {
        // Holding the lock keeps a concurrent append from being read half
        // written.
        let _inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        Ok(scan(&self.path)?.0)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Startup recovery. Run before serving, while nothing is in flight.
    ///
    /// A stop whose arrival was recorded but which never recorded a
    /// completion or an error was cut off mid-run. Its hops are NOT re-run:
    /// scripts and moves are not guaranteed to be safe to repeat, and a
    /// failed route already waits for a human (see Route::run). Instead an
    /// Interrupted event is appended, so the gap is recorded once and the
    /// next startup does not report it again.
    pub fn recover(&self) -> anyhow::Result<Health> {
        let health = check(&self.records()?);
        for arrival in &health.interrupted {
            let mut event = Event::new(
                &arrival.event.to,
                &arrival.event.to,
                Some(arrival.event.event_id),
                arrival.event.envelope_id,
                EventKind::Interrupted,
                Some("found at startup: stop began but never finished".to_string()),
            );
            event.content_hash = arrival.event.content_hash.clone();
            event.envelope_parent = arrival.event.envelope_parent;
            self.append(event)?;
            tracing::warn!(
                seq = arrival.seq,
                nest = %arrival.event.to,
                envelope = ?arrival.event.envelope_id,
                "interrupted stop recorded — its hops were not re-run"
            );
        }
        for (seq, missing) in &health.dangling_causes {
            tracing::warn!(seq, caused_by = %missing, "event names a cause that is not earlier in the ledger");
        }
        for (seq, missing) in &health.dangling_parents {
            tracing::warn!(seq, envelope_parent = %missing, "envelope names a parent the ledger has never seen");
        }
        Ok(health)
    }
}

/// What a pass over the ledger found.
#[derive(Debug, Default)]
pub struct Health {
    pub records: usize,
    /// Arrivals with no Completed, Error or Interrupted event caused by them.
    pub interrupted: Vec<Record>,
    /// (seq, id): caused_by points at no earlier event.
    pub dangling_causes: Vec<(u64, Uuid)>,
    /// (seq, id): envelope_parent points at no earlier envelope.
    pub dangling_parents: Vec<(u64, Uuid)>,
}

/// Pure check over a list of records. No I/O, so it is easy to test by hand.
pub fn check(records: &[Record]) -> Health {
    let mut seen_events: HashSet<Uuid> = HashSet::new();
    let mut seen_envelopes: HashSet<Uuid> = HashSet::new();
    let mut closed: HashSet<Uuid> = HashSet::new();
    let mut health = Health {
        records: records.len(),
        ..Health::default()
    };

    for r in records {
        let e = &r.event;
        if let Some(cause) = e.caused_by {
            if !seen_events.contains(&cause) {
                health.dangling_causes.push((r.seq, cause));
            }
            if matches!(
                e.kind,
                EventKind::Completed | EventKind::Error | EventKind::Interrupted
            ) {
                closed.insert(cause);
            }
        }
        if let Some(parent) = e.envelope_parent {
            if !seen_envelopes.contains(&parent) {
                health.dangling_parents.push((r.seq, parent));
            }
        }
        seen_events.insert(e.event_id);
        if let Some(id) = e.envelope_id {
            seen_envelopes.insert(id);
        }
    }

    health.interrupted = records
        .iter()
        .filter(|r| r.event.kind == EventKind::Transition && !closed.contains(&r.event.event_id))
        .cloned()
        .collect();

    // An envelope's parent is only reported once, however many events it has.
    let mut reported = HashSet::new();
    health.dangling_parents.retain(|(_, p)| reported.insert(*p));
    health
}

/// Read a ledger file. Returns the records, the byte length of the good
/// prefix, and how many bytes of unterminated tail follow it.
fn scan(path: &Path) -> anyhow::Result<(Vec<Record>, u64, u64)> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 0, 0)),
        Err(e) => return Err(e.into()),
    };

    let mut records: Vec<Record> = Vec::new();
    let mut offset = 0usize;
    let mut line_no = 0usize;

    while offset < bytes.len() {
        let Some(end) = bytes[offset..].iter().position(|&b| b == b'\n') else {
            // No newline: the write that made this line never finished.
            let torn = (bytes.len() - offset) as u64;
            return Ok((records, offset as u64, torn));
        };
        line_no += 1;
        let line = &bytes[offset..offset + end];
        let record: Record = serde_json::from_slice(line).map_err(|e| {
            anyhow::anyhow!(
                "ledger {} line {line_no} is complete but unreadable ({e}). \
                 That is corruption, not an interrupted write, so Mailroom \
                 will not guess. Inspect the line, then fix or remove it by hand",
                path.display()
            )
        })?;
        if let Some(prev) = records.last() {
            if record.seq != prev.seq + 1 {
                anyhow::bail!(
                    "ledger {} line {line_no}: seq {} follows {} — records are \
                     missing or out of order",
                    path.display(),
                    record.seq,
                    prev.seq
                );
            }
        }
        records.push(record);
        offset += end + 1;
    }
    Ok((records, offset as u64, 0))
}

/// The one ledger for this vault in this process. Every route in the
/// process shares it, which is what makes appends single-writer.
pub fn for_vault(vault_root: &Path) -> anyhow::Result<Arc<Ledger>> {
    static OPEN: OnceLock<Mutex<HashMap<PathBuf, Arc<Ledger>>>> = OnceLock::new();
    let key = std::fs::canonicalize(vault_root)?;
    let mut open = OPEN
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if let Some(ledger) = open.get(&key) {
        return Ok(ledger.clone());
    }
    let ledger = Arc::new(Ledger::open(&key)?);
    open.insert(key, ledger.clone());
    Ok(ledger)
}

/// This machine's name, as written into each record. MAILROOM_DEVICE
/// overrides it; otherwise the hostname.
pub fn device_name() -> String {
    if let Ok(name) = std::env::var("MAILROOM_DEVICE") {
        if !name.trim().is_empty() {
            return name.trim().to_lowercase();
        }
    }
    ["/etc/hostname", "/proc/sys/kernel/hostname"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: EventKind, caused_by: Option<Uuid>, envelope: Option<Uuid>) -> Event {
        Event::new("a", "b", caused_by, envelope, kind, None)
    }

    fn ledger_file(vault: &Path) -> PathBuf {
        vault.join(".mailroom/ledger/events.jsonl")
    }

    #[test]
    fn appends_are_numbered_from_one_and_survive_reopening() {
        let vault = tempfile::tempdir().unwrap();
        {
            let l = Ledger::open(vault.path()).unwrap();
            let seqs: Vec<u64> = (0..3)
                .map(|_| l.append(ev(EventKind::Transition, None, None)).unwrap().seq)
                .collect();
            assert_eq!(seqs, vec![1, 2, 3]);
        }
        let l = Ledger::open(vault.path()).unwrap();
        assert_eq!(
            l.append(ev(EventKind::Transition, None, None)).unwrap().seq,
            4
        );
        let records = l.records().unwrap();
        assert_eq!(records.len(), 4);
        assert!(records.iter().all(|r| r.v == SCHEMA_VERSION));
    }

    #[test]
    fn a_torn_final_line_is_removed_and_numbering_continues() {
        let vault = tempfile::tempdir().unwrap();
        let len_after_two = {
            let l = Ledger::open(vault.path()).unwrap();
            l.append(ev(EventKind::Transition, None, None)).unwrap();
            l.append(ev(EventKind::Transition, None, None)).unwrap();
            std::fs::metadata(ledger_file(vault.path())).unwrap().len()
        };
        // Simulate a crash partway through writing record 3.
        let mut f = OpenOptions::new()
            .append(true)
            .open(ledger_file(vault.path()))
            .unwrap();
        f.write_all(br#"{"v":1,"seq":3,"device":"x","event_"#)
            .unwrap();
        drop(f);

        let l = Ledger::open(vault.path()).unwrap();
        assert_eq!(
            std::fs::metadata(ledger_file(vault.path())).unwrap().len(),
            len_after_two
        );
        assert_eq!(
            l.append(ev(EventKind::Transition, None, None)).unwrap().seq,
            3
        );
        assert_eq!(l.records().unwrap().len(), 3);
    }

    #[test]
    fn a_complete_but_unreadable_line_refuses_to_open() {
        let vault = tempfile::tempdir().unwrap();
        {
            let l = Ledger::open(vault.path()).unwrap();
            l.append(ev(EventKind::Transition, None, None)).unwrap();
        }
        let mut f = OpenOptions::new()
            .append(true)
            .open(ledger_file(vault.path()))
            .unwrap();
        f.write_all(b"not json at all\n").unwrap();
        drop(f);

        let err = Ledger::open(vault.path())
            .err()
            .expect("corruption must not be skipped");
        assert!(err.to_string().contains("line 2"), "{err}");
    }

    #[test]
    fn a_gap_in_seq_refuses_to_open() {
        let vault = tempfile::tempdir().unwrap();
        {
            let l = Ledger::open(vault.path()).unwrap();
            l.append(ev(EventKind::Transition, None, None)).unwrap();
            l.append(ev(EventKind::Transition, None, None)).unwrap();
        }
        // Repeat the last line, as a bad copy or merge might: seq 2, seq 2.
        let text = std::fs::read_to_string(ledger_file(vault.path())).unwrap();
        let second = text.lines().nth(1).unwrap().to_string();
        std::fs::write(ledger_file(vault.path()), format!("{text}{second}\n")).unwrap();

        let err = Ledger::open(vault.path())
            .err()
            .expect("out-of-order seq must be refused");
        assert!(err.to_string().contains("seq 2 follows 2"), "{err}");
    }

    #[test]
    fn a_second_open_on_the_same_vault_is_refused_while_the_first_is_held() {
        let vault = tempfile::tempdir().unwrap();
        let first = Ledger::open(vault.path()).unwrap();
        let err = Ledger::open(vault.path())
            .err()
            .expect("second writer must be refused");
        assert!(
            err.to_string().contains("in use by another Mailroom"),
            "{err}"
        );
        drop(first);
        assert!(
            Ledger::open(vault.path()).is_ok(),
            "lock is released on drop"
        );
    }

    // Hand-worked example for check():
    //   seq 1  arrival A1 (envelope E)              → closed by seq 2
    //   seq 2  completed, caused_by A1
    //   seq 3  arrival A2 (envelope E), caused_by A1 → never closed
    //   seq 4  arrival A3 (envelope F, parent E)     → closed by seq 5
    //   seq 5  error, caused_by A3
    //   seq 6  arrival A4, caused_by X (not in ledger), parent G (never seen)
    // Expected: interrupted = {A2, A4}; dangling causes = [(6, X)];
    //           dangling parents = [(6, G)].
    #[test]
    fn check_finds_interrupted_stops_and_dangling_references() {
        let (e, f, g, x) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let a1 = ev(EventKind::Transition, None, Some(e));
        let done = ev(EventKind::Completed, Some(a1.event_id), Some(e));
        let a2 = ev(EventKind::Transition, Some(a1.event_id), Some(e));
        let mut a3 = ev(EventKind::Transition, None, Some(f));
        a3.envelope_parent = Some(e);
        let err = ev(EventKind::Error, Some(a3.event_id), Some(f));
        let mut a4 = ev(EventKind::Transition, Some(x), None);
        a4.envelope_parent = Some(g);

        let records: Vec<Record> = [a1, done, a2.clone(), a3, err, a4.clone()]
            .into_iter()
            .enumerate()
            .map(|(i, event)| Record {
                v: 1,
                seq: i as u64 + 1,
                device: "t".into(),
                event,
            })
            .collect();

        let h = check(&records);
        let interrupted: Vec<Uuid> = h.interrupted.iter().map(|r| r.event.event_id).collect();
        assert_eq!(interrupted, vec![a2.event_id, a4.event_id]);
        assert_eq!(h.dangling_causes, vec![(6, x)]);
        assert_eq!(h.dangling_parents, vec![(6, g)]);
    }

    #[test]
    fn recover_records_each_interruption_once() {
        let vault = tempfile::tempdir().unwrap();
        let l = Ledger::open(vault.path()).unwrap();
        let arrival = l.append(ev(EventKind::Transition, None, None)).unwrap();

        let first = l.recover().unwrap();
        assert_eq!(first.interrupted.len(), 1);
        let last = l.records().unwrap().pop().unwrap();
        assert_eq!(last.event.kind, EventKind::Interrupted);
        assert_eq!(last.event.caused_by, Some(arrival.event.event_id));

        let second = l.recover().unwrap();
        assert!(
            second.interrupted.is_empty(),
            "already recorded; not reported twice"
        );
    }
}
