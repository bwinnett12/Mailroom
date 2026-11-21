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
// The live graph (graph.rs) is kept here too, inside the same lock as the
// file. Every successful append is applied to it before the lock is
// released, so the graph is always exactly the ledger replayed: no reader
// can see a record in one and not the other. Every MAILROOM_SNAPSHOT_EVERY
// appends (default 1000, 0 = never) the graph is written to
//   <vault>/.mailroom/graph/snapshot.bin
// and on open the graph is loaded from that snapshot and only the records
// after it are replayed. See graph/snapshot.rs.
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
use crate::graph::{snapshot, Graph};

/// Default snapshot cadence, in appended records.
pub const DEFAULT_SNAPSHOT_EVERY: u64 = 1000;

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
    snapshot_path: PathBuf,
    snapshot_every: u64,
    startup: Startup,
    /// Dangling references found at open. Only a full read can find them;
    /// see `check` and the note in open_with.
    dangling_causes: Vec<(u64, Uuid)>,
    dangling_parents: Vec<(u64, Uuid)>,
    inner: Mutex<Inner>,
    // Held, never read: dropping the Ledger releases the lock.
    _lock: File,
}

struct Inner {
    file: File,
    next_seq: u64,
    /// Bytes known to be good. A failed append truncates back to this.
    len: u64,
    /// The ledger replayed. Updated by every append, under this lock.
    graph: Graph,
    /// Appends since the last snapshot was written.
    since_snapshot: u64,
    /// seq and event_id of the newest record, which a snapshot is "as of".
    last: Option<(u64, Uuid)>,
    /// Arrivals not yet closed by Completed / Error / Interrupted, by
    /// event_id. At startup these are the stops a crash cut off; while
    /// running they are the stops in flight.
    open: HashMap<Uuid, Record>,
}

/// How the graph was built when the ledger was opened.
#[derive(Debug, Clone, PartialEq)]
pub struct Startup {
    /// The snapshot's seq, if one was loaded and matched this ledger.
    pub from_snapshot: Option<u64>,
    /// Records replayed on top of the snapshot (or all of them, without one).
    pub replayed: usize,
    /// Why a snapshot on disk was not used, if there was one.
    pub snapshot_rejected: Option<String>,
    /// Bytes of ledger read and parsed at open.
    pub bytes_read: u64,
    pub millis: u128,
}

impl Ledger {
    /// Open (creating if needed) the ledger for a vault, repairing a torn
    /// final line. Fails if another process holds the ledger, or if a
    /// complete line is corrupt.
    pub fn open(vault_root: &Path) -> anyhow::Result<Ledger> {
        let every = std::env::var("MAILROOM_SNAPSHOT_EVERY")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(DEFAULT_SNAPSHOT_EVERY);
        Ledger::open_with(vault_root, every)
    }

    /// `open`, with an explicit snapshot cadence (0 = never snapshot).
    pub fn open_with(vault_root: &Path, snapshot_every: u64) -> anyhow::Result<Ledger> {
        let started = std::time::Instant::now();
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
        let snapshot_path = vault_root
            .join(".mailroom")
            .join("graph")
            .join("snapshot.bin");

        // ── Where to start reading ──────────────────────────────────────────
        //
        // With a snapshot that matches this ledger, the graph and the open
        // arrivals come from the snapshot, and only the ledger bytes after
        // its checkpoint are read. Without one, the whole ledger is read.
        //
        // The trade: records before the checkpoint are not re-parsed at
        // every start. They were parsed when they were appended and again
        // when the snapshot was taken; the tail, where a crash can do
        // damage, is always checked. Dangling-reference warnings need every
        // event id, so they only run on a full read.
        let mut snapshot_rejected = None;
        let loaded = match snapshot::read(&snapshot_path) {
            Ok(None) => None,
            Err(e) => {
                snapshot_rejected = Some(format!("damaged: {e}"));
                None
            }
            Ok(Some(l)) => match verify_checkpoint(&path, &l.checkpoint) {
                Ok(()) => Some(l),
                Err(why) => {
                    snapshot_rejected = Some(why);
                    None
                }
            },
        };
        if let Some(why) = &snapshot_rejected {
            tracing::warn!(snapshot = %snapshot_path.display(), %why, "graph snapshot not used — rebuilding from the whole ledger");
        }

        let (mut graph, mut open, start, expect_seq, base) = match loaded {
            Some(l) => {
                let cp = l.checkpoint;
                let open = cp
                    .open_arrivals
                    .iter()
                    .map(|r| (r.event.event_id, r.clone()))
                    .collect();
                let base = Some((cp.last_seq, cp.last_event_id));
                (l.graph, open, cp.ledger_offset, Some(cp.last_seq + 1), base)
            }
            None => (Graph::default(), HashMap::new(), 0, None, None),
        };
        let from_snapshot = base.map(|(seq, _)| seq);

        let (tail, good_len, torn_bytes) = scan_from(&path, start, expect_seq)?;

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

        for r in &tail {
            graph.apply(r);
            track(&mut open, r);
        }
        let (dangling_causes, dangling_parents) = if from_snapshot.is_none() {
            let h = check(&tail);
            (h.dangling_causes, h.dangling_parents)
        } else {
            (Vec::new(), Vec::new())
        };

        let last = tail.last().map(|r| (r.seq, r.event.event_id)).or(base);
        let next_seq = last.map(|(seq, _)| seq + 1).unwrap_or(1);
        let mut inner = Inner {
            file,
            next_seq,
            len: good_len,
            graph,
            since_snapshot: tail.len() as u64,
            last,
            open,
        };

        // Write a fresh snapshot if the one on disk was unusable, or the
        // tail has grown past the cadence. Failure is only a warning: the
        // ledger is intact, and the next start just reads more of it.
        let stale = snapshot_rejected.is_some()
            || (snapshot_every > 0 && inner.since_snapshot >= snapshot_every);
        if stale && last.is_some() {
            if let Err(e) = write_snapshot(&snapshot_path, &mut inner) {
                tracing::warn!(error = %e, "could not write graph snapshot");
            }
        }

        let startup = Startup {
            from_snapshot,
            replayed: tail.len(),
            snapshot_rejected,
            bytes_read: good_len + torn_bytes - start,
            millis: started.elapsed().as_millis(),
        };
        tracing::info!(
            records = next_seq - 1,
            from_snapshot = ?startup.from_snapshot,
            replayed = startup.replayed,
            bytes_read = startup.bytes_read,
            nodes = inner.graph.node_count(),
            edges = inner.graph.edge_count(),
            ms = startup.millis,
            "graph loaded"
        );

        Ok(Ledger {
            path,
            device: device_name(),
            snapshot_path,
            snapshot_every,
            startup,
            dangling_causes,
            dangling_parents,
            inner: Mutex::new(inner),
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

        // The record is durable; now the graph. Same lock, so no reader
        // sees the ledger and the graph disagree.
        inner.graph.apply(&record);
        track(&mut inner.open, &record);
        inner.last = Some((record.seq, record.event.event_id));
        inner.since_snapshot += 1;
        if self.snapshot_every > 0 && inner.since_snapshot >= self.snapshot_every {
            // Synchronous, under the lock: appends wait while it's written.
            // Fine at this size; a background writer is a later change.
            if let Err(e) = write_snapshot(&self.snapshot_path, &mut inner) {
                tracing::warn!(error = %e, "could not write graph snapshot");
            }
        }
        Ok(record)
    }

    /// Run `f` against the live graph. The ledger is locked while `f` runs,
    /// so keep it to reading.
    pub fn with_graph<R>(&self, f: impl FnOnce(&Graph) -> R) -> R {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        f(&inner.graph)
    }

    /// Write a snapshot now, whatever the cadence.
    #[allow(dead_code)]
    pub fn snapshot_now(&self) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if inner.last.is_none() {
            return Ok(());
        }
        write_snapshot(&self.snapshot_path, &mut inner)
    }

    /// How the graph was built at open.
    #[allow(dead_code)]
    pub fn startup(&self) -> &Startup {
        &self.startup
    }

    /// Every record, in order. Reads the whole file: for tests and
    /// one-off checks, not the request path.
    #[allow(dead_code)]
    pub fn records(&self) -> anyhow::Result<Vec<Record>> {
        // Holding the lock keeps a concurrent append from being read half
        // written.
        let _inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        Ok(scan_from(&self.path, 0, None)?.0)
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
        // The open arrivals are kept up to date on every append (and carried
        // in the snapshot), so nothing needs re-reading here.
        let (mut interrupted, records) = {
            let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let open: Vec<Record> = inner.open.values().cloned().collect();
            (open, inner.next_seq as usize - 1)
        };
        interrupted.sort_by_key(|r| r.seq);
        let health = Health {
            records,
            interrupted,
            dangling_causes: self.dangling_causes.clone(),
            dangling_parents: self.dangling_parents.clone(),
        };
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

fn write_snapshot(path: &Path, inner: &mut Inner) -> anyhow::Result<()> {
    let (last_seq, last_event_id) = inner.last.expect("callers check there is a record");
    let mut open_arrivals: Vec<Record> = inner.open.values().cloned().collect();
    open_arrivals.sort_by_key(|r| r.seq);
    let checkpoint = snapshot::Checkpoint {
        last_seq,
        last_event_id,
        ledger_offset: inner.len,
        open_arrivals,
    };
    snapshot::write(path, &inner.graph, &checkpoint)?;
    inner.since_snapshot = 0;
    tracing::debug!(seq = last_seq, "graph snapshot written");
    Ok(())
}

/// Keep the set of open arrivals in step with one record.
fn track(open: &mut HashMap<Uuid, Record>, r: &Record) {
    match r.event.kind {
        EventKind::Transition => {
            open.insert(r.event.event_id, r.clone());
        }
        EventKind::Completed | EventKind::Error | EventKind::Interrupted => {
            if let Some(cause) = r.event.caused_by {
                open.remove(&cause);
            }
        }
        EventKind::Received | EventKind::Linked => {}
    }
}

/// A snapshot belongs to this ledger if the line just before its
/// ledger_offset is the record it says it was taken at. Reads only that
/// one line. Catches a snapshot from another vault, one kept after the
/// ledger was reset, and one ahead of a ledger that lost records.
fn verify_checkpoint(path: &Path, cp: &snapshot::Checkpoint) -> Result<(), String> {
    use std::io::{Read, Seek, SeekFrom};
    let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if cp.last_seq == 0 || cp.ledger_offset == 0 {
        return Err("snapshot is of an empty ledger".into());
    }
    if len < cp.ledger_offset {
        return Err(format!(
            "snapshot covers {} bytes of ledger, the ledger is {len} bytes",
            cp.ledger_offset
        ));
    }
    // Read back far enough to hold the last line before the offset.
    let window = cp.ledger_offset.min(1 << 20);
    let mut buf = vec![0u8; window as usize];
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    f.seek(SeekFrom::Start(cp.ledger_offset - window))
        .map_err(|e| e.to_string())?;
    f.read_exact(&mut buf).map_err(|e| e.to_string())?;
    if buf.last() != Some(&b'\n') {
        return Err("snapshot's offset is not at the end of a ledger line".into());
    }
    let body = &buf[..buf.len() - 1];
    let line = match body.iter().rposition(|&b| b == b'\n') {
        Some(p) => &body[p + 1..],
        None if window == cp.ledger_offset => body,
        None => return Err("record before the snapshot's offset is too long to check".into()),
    };
    let r: Record = serde_json::from_slice(line)
        .map_err(|_| "the line before the snapshot's offset is not a record".to_string())?;
    if r.seq != cp.last_seq || r.event.event_id != cp.last_event_id {
        return Err(format!(
            "record {} at the snapshot's offset is not the one it was taken at (another vault, or a reset ledger)",
            r.seq
        ));
    }
    Ok(())
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

/// Read a ledger file from byte `start` (0 for the whole file). Returns
/// the records, the byte length of the good prefix (counted from the start
/// of the file), and how many bytes of unterminated tail follow it.
/// `expect_seq` is the seq the first record must have, when reading from a
/// checkpoint.
fn scan_from(
    path: &Path,
    start: u64,
    expect_seq: Option<u64>,
) -> anyhow::Result<(Vec<Record>, u64, u64)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut bytes = Vec::new();
    match File::open(path) {
        Ok(mut f) => {
            f.seek(SeekFrom::Start(start))?;
            f.read_to_end(&mut bytes)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), start, 0)),
        Err(e) => return Err(e.into()),
    }
    let place = |line_no: usize| {
        if start == 0 {
            format!("line {line_no}")
        } else {
            format!("line {line_no} after byte {start}")
        }
    };

    let mut records: Vec<Record> = Vec::new();
    let mut offset = 0usize;
    let mut line_no = 0usize;

    while offset < bytes.len() {
        let Some(end) = bytes[offset..].iter().position(|&b| b == b'\n') else {
            // No newline: the write that made this line never finished.
            let torn = (bytes.len() - offset) as u64;
            return Ok((records, start + offset as u64, torn));
        };
        line_no += 1;
        let line = &bytes[offset..offset + end];
        let record: Record = serde_json::from_slice(line).map_err(|e| {
            anyhow::anyhow!(
                "ledger {} {} is complete but unreadable ({e}). \
                 That is corruption, not an interrupted write, so Mailroom \
                 will not guess. Inspect the line, then fix or remove it by hand",
                path.display(),
                place(line_no)
            )
        })?;
        let expected = records.last().map(|p| p.seq + 1).or(expect_seq);
        if let Some(want) = expected {
            if record.seq != want {
                anyhow::bail!(
                    "ledger {} {}: seq {} follows {} — records are \
                     missing or out of order",
                    path.display(),
                    place(line_no),
                    record.seq,
                    want - 1
                );
            }
        }
        records.push(record);
        offset += end + 1;
    }
    Ok((records, start + offset as u64, 0))
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

    // ── The live graph and its snapshot ──────────────────────────────────

    fn link(i: u64) -> Event {
        let mut e = ev(EventKind::Linked, None, None);
        e.link = Some(crate::event::LinkFacts {
            from: format!("n{i}"),
            label: "NEXT".into(),
            to: format!("n{}", i + 1),
        });
        e
    }

    fn same_as_replay(l: &Ledger) {
        let replayed = Graph::from_records(&l.records().unwrap());
        l.with_graph(|g| {
            assert_eq!(g.edges(), replayed.edges());
            assert_eq!(g.degrees(), replayed.degrees());
        });
    }

    #[test]
    fn the_live_graph_is_always_the_ledger_replayed() {
        let vault = tempfile::tempdir().unwrap();
        let l = Ledger::open_with(vault.path(), 0).unwrap();
        for i in 0..5 {
            l.append(link(i)).unwrap();
            same_as_replay(&l);
        }
        assert_eq!(l.with_graph(|g| g.edge_count()), 5);
    }

    // Snapshot every 2 appends; 5 appends → snapshots at seq 2 and 4.
    // Reopen: load the seq-4 snapshot, replay 1 record (seq 5).
    #[test]
    fn reopening_loads_the_snapshot_and_replays_only_the_tail() {
        let vault = tempfile::tempdir().unwrap();
        {
            let l = Ledger::open_with(vault.path(), 2).unwrap();
            for i in 0..5 {
                l.append(link(i)).unwrap();
            }
        }
        let l = Ledger::open_with(vault.path(), 2).unwrap();
        assert_eq!(l.startup().from_snapshot, Some(4));
        assert_eq!(l.startup().replayed, 1);
        same_as_replay(&l);
    }

    #[test]
    fn a_snapshot_from_another_vault_is_ignored() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        for v in [&a, &b] {
            let l = Ledger::open_with(v.path(), 0).unwrap();
            for i in 0..3 {
                l.append(link(i)).unwrap();
            }
            l.snapshot_now().unwrap();
        }
        let snap = |v: &tempfile::TempDir| v.path().join(".mailroom/graph/snapshot.bin");
        std::fs::copy(snap(&a), snap(&b)).unwrap();

        let l = Ledger::open_with(b.path(), 0).unwrap();
        assert_eq!(l.startup().from_snapshot, None);
        assert!(l
            .startup()
            .snapshot_rejected
            .as_deref()
            .unwrap()
            .contains("another vault"));
        same_as_replay(&l);
    }

    #[test]
    fn a_damaged_snapshot_is_rebuilt_and_replaced() {
        let vault = tempfile::tempdir().unwrap();
        {
            let l = Ledger::open_with(vault.path(), 0).unwrap();
            for i in 0..3 {
                l.append(link(i)).unwrap();
            }
            l.snapshot_now().unwrap();
        }
        let snap = vault.path().join(".mailroom/graph/snapshot.bin");
        let mut bytes = std::fs::read(&snap).unwrap();
        let n = bytes.len();
        bytes[n - 3] ^= 0xFF;
        std::fs::write(&snap, bytes).unwrap();

        let l = Ledger::open_with(vault.path(), 0).unwrap();
        assert!(l
            .startup()
            .snapshot_rejected
            .as_deref()
            .unwrap()
            .starts_with("damaged"));
        same_as_replay(&l);
        drop(l);
        let l = Ledger::open_with(vault.path(), 0).unwrap();
        assert_eq!(
            l.startup().from_snapshot,
            Some(3),
            "a good snapshot was written in its place"
        );
    }

    #[test]
    fn a_snapshot_ahead_of_the_ledger_is_ignored() {
        let vault = tempfile::tempdir().unwrap();
        {
            let l = Ledger::open_with(vault.path(), 0).unwrap();
            for i in 0..4 {
                l.append(link(i)).unwrap();
            }
            l.snapshot_now().unwrap();
        }
        // The ledger loses its last two records (restored from an older copy).
        let file = ledger_file(vault.path());
        let text = std::fs::read_to_string(&file).unwrap();
        let kept: String = text.lines().take(2).map(|l| format!("{l}\n")).collect();
        std::fs::write(&file, kept).unwrap();

        let l = Ledger::open_with(vault.path(), 0).unwrap();
        assert!(l
            .startup()
            .snapshot_rejected
            .as_deref()
            .unwrap()
            .contains("the ledger is"));
        assert_eq!(l.with_graph(|g| g.edge_count()), 2);
    }

    // A crash after a snapshot: the stop that was open when the snapshot
    // was taken must still be found by recovery, without a full read.
    #[test]
    fn an_open_stop_carried_in_the_snapshot_is_still_recovered() {
        let vault = tempfile::tempdir().unwrap();
        let arrival = {
            let l = Ledger::open_with(vault.path(), 0).unwrap();
            l.append(link(0)).unwrap();
            let a = l.append(ev(EventKind::Transition, None, None)).unwrap();
            l.snapshot_now().unwrap();
            l.append(link(1)).unwrap();
            a
        }; // "crash": the arrival never closed
        let l = Ledger::open_with(vault.path(), 0).unwrap();
        assert_eq!(l.startup().from_snapshot, Some(2));
        assert_eq!(
            l.startup().replayed,
            1,
            "only the record after the checkpoint was read"
        );
        let h = l.recover().unwrap();
        assert_eq!(h.interrupted.len(), 1);
        assert_eq!(h.interrupted[0].event.event_id, arrival.event.event_id);
        assert!(l.recover().unwrap().interrupted.is_empty());
    }

    #[test]
    fn a_torn_tail_after_the_checkpoint_is_still_repaired() {
        let vault = tempfile::tempdir().unwrap();
        {
            let l = Ledger::open_with(vault.path(), 0).unwrap();
            l.append(link(0)).unwrap();
            l.snapshot_now().unwrap();
            l.append(link(1)).unwrap();
        }
        let mut f = OpenOptions::new()
            .append(true)
            .open(ledger_file(vault.path()))
            .unwrap();
        f.write_all(br#"{"v":1,"seq":3,"dev"#).unwrap();
        drop(f);
        let l = Ledger::open_with(vault.path(), 0).unwrap();
        assert_eq!(l.startup().from_snapshot, Some(1));
        assert_eq!(l.append(link(2)).unwrap().seq, 3);
        same_as_replay(&l);
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
