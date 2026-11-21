// src/graph/snapshot.rs
//
// A snapshot is the graph written to disk at one point in the ledger, so
// startup can load it and replay only the records after that point.
//
// It is disposable. The ledger is the truth. A snapshot that is missing,
// damaged, from a different ledger, or ahead of the ledger is thrown away
// and the graph is rebuilt by full replay. So everything here is about
// *detecting* damage, never repairing it.
//
// ── File layout ─────────────────────────────────────────────────────────────
//
//   offset  bytes
//   0       4     magic  "MRGS"  (Mailroom Graph Snapshot)
//   4       4     format version, u32 little-endian
//   8       …     frames, back to back:
//
//                   ┌──────────┬──────────┬──────────────────┐
//                   │ len  u32 │ crc  u32 │ payload (len B)  │
//                   └──────────┴──────────┴──────────────────┘
//
//                 len and crc are little-endian. crc is CRC-32 (IEEE) of
//                 the payload. The payload is one Frame as JSON.
//
//   Frames, in order:  Header, then Node / Edge / EnvKey in any order,
//                      then End.
//
// Why frames and a per-frame CRC, not one JSON document:
//   - A torn write (crash mid-write) cuts the file short. The last frame's
//     length runs past the end of the file, or End is missing: detected.
//   - A flipped bit inside a frame fails that frame's CRC: detected, and
//     the error says which frame.
//   - End carries the counts of what came before it, so a frame silently
//     dropped (a bug, not a crash) is detected too.
//   - It is the same shape as the record formats in Petrov ch. 3: a
//     length prefix so you can skip, a checksum so you can trust.
//
// The payload is JSON for readability while this is young. Swapping it for
// a binary encoding later changes only encode/decode, not the framing.
//
// Writing is atomic: temp file in the same directory, fsync, rename over
// the old snapshot, fsync the directory. A crash leaves the old snapshot
// or the new one, never half of one under the real name.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Edge, Graph, Node, NodeId};

const MAGIC: &[u8; 4] = b"MRGS";
const FORMAT_VERSION: u32 = 1;
/// No single frame should come near this. A larger length means the
/// length field itself is damaged, so stop before allocating it.
const MAX_FRAME: u32 = 64 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Frame {
    /// Which point in which ledger this snapshot is of. See Checkpoint.
    Header(Checkpoint),
    Node(Node),
    Edge(Edge),
    /// An envelope received with a key: its uuid → its node. Needed so
    /// later events that name the envelope by uuid land on the key node.
    EnvKey {
        envelope: Uuid,
        node: NodeId,
    },
    End {
        nodes: u64,
        edges: u64,
        env_keys: u64,
    },
}

/// The point in the ledger a snapshot was taken at, and what startup
/// needs to carry on from there without reading anything before it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    /// The last record the snapshot includes.
    pub last_seq: u64,
    /// That record's event_id. What ties the snapshot to *this* ledger: one
    /// copied from another vault, or kept after the ledger was reset, will
    /// not find this id at last_seq.
    pub last_event_id: Uuid,
    /// Length in bytes of the ledger up to and including last_seq. Startup
    /// starts reading the ledger here.
    pub ledger_offset: u64,
    /// Arrivals that had not closed (Completed / Error / Interrupted) by
    /// last_seq. Startup recovery needs them, and they would otherwise
    /// only be found by reading the whole ledger.
    pub open_arrivals: Vec<crate::ledger::Record>,
}

/// A snapshot read back from disk.
#[derive(Debug)]
pub struct Loaded {
    pub graph: Graph,
    pub checkpoint: Checkpoint,
}

/// Write `graph` as the snapshot at `path`, as of `checkpoint`.
pub fn write(path: &Path, graph: &Graph, checkpoint: &Checkpoint) -> anyhow::Result<()> {
    let dir = path.parent().expect("snapshot path has a parent");
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".snapshot.{}.tmp", Uuid::new_v4()));

    let result = (|| -> anyhow::Result<()> {
        let mut out = std::io::BufWriter::new(File::create(&tmp)?);
        out.write_all(MAGIC)?;
        out.write_all(&FORMAT_VERSION.to_le_bytes())?;

        put(&mut out, &Frame::Header(checkpoint.clone()))?;
        let (mut nodes, mut edges, mut env_keys) = (0u64, 0u64, 0u64);
        for n in graph.nodes() {
            put(&mut out, &Frame::Node(n.clone()))?;
            nodes += 1;
        }
        for e in graph.edges() {
            put(&mut out, &Frame::Edge(e.clone()))?;
            edges += 1;
        }
        for (envelope, node) in graph.envelope_keys() {
            put(
                &mut out,
                &Frame::EnvKey {
                    envelope,
                    node: node.clone(),
                },
            )?;
            env_keys += 1;
        }
        put(
            &mut out,
            &Frame::End {
                nodes,
                edges,
                env_keys,
            },
        )?;

        let file = out.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)?;
        File::open(dir)?.sync_all()?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Read the snapshot at `path`.
///   Ok(None)   no snapshot there
///   Ok(Some)   a complete, undamaged snapshot
///   Err        anything else; the message says what was wrong
pub fn read(path: &Path) -> anyhow::Result<Option<Loaded>> {
    let mut bytes = Vec::new();
    match File::open(path) {
        Ok(mut f) => f.read_to_end(&mut bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    if bytes.len() < 8 || &bytes[..4] != MAGIC {
        anyhow::bail!("not a Mailroom graph snapshot (bad magic)");
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    if version != FORMAT_VERSION {
        anyhow::bail!("snapshot format {version}, this build reads {FORMAT_VERSION}");
    }

    let mut pos = 8usize;
    let mut index = 0usize;
    let mut header: Option<Checkpoint> = None;
    let (mut nodes, mut edges, mut env_keys) = (Vec::new(), Vec::new(), Vec::new());

    loop {
        let frame = take(&bytes, &mut pos, index)?;
        index += 1;
        match frame {
            Frame::Header(checkpoint) => {
                if index != 1 {
                    anyhow::bail!("frame {}: Header must be first", index - 1);
                }
                header = Some(checkpoint);
            }
            _ if header.is_none() => anyhow::bail!("frame 0 is not a Header"),
            Frame::Node(n) => nodes.push(n),
            Frame::Edge(e) => edges.push(e),
            Frame::EnvKey { envelope, node } => env_keys.push((envelope, node)),
            Frame::End {
                nodes: n,
                edges: e,
                env_keys: k,
            } => {
                let counted = (
                    nodes.len() as u64,
                    edges.len() as u64,
                    env_keys.len() as u64,
                );
                if counted != (n, e, k) {
                    anyhow::bail!(
                        "End says {n} nodes, {e} edges, {k} keys; file holds {}, {}, {}",
                        counted.0,
                        counted.1,
                        counted.2
                    );
                }
                if pos != bytes.len() {
                    anyhow::bail!("{} bytes after End", bytes.len() - pos);
                }
                break;
            }
        }
    }

    let checkpoint = header.expect("checked above");
    let graph = Graph::from_parts(nodes, edges, env_keys)?;
    Ok(Some(Loaded { graph, checkpoint }))
}

fn put(out: &mut impl Write, frame: &Frame) -> anyhow::Result<()> {
    let payload = serde_json::to_vec(frame)?;
    let len = u32::try_from(payload.len())
        .ok()
        .filter(|&l| l <= MAX_FRAME)
        .ok_or_else(|| anyhow::anyhow!("frame of {} bytes is too large", payload.len()))?;
    out.write_all(&len.to_le_bytes())?;
    out.write_all(&crc32(&payload).to_le_bytes())?;
    out.write_all(&payload)?;
    Ok(())
}

fn take(bytes: &[u8], pos: &mut usize, index: usize) -> anyhow::Result<Frame> {
    let rest = bytes.len() - *pos;
    if rest == 0 {
        anyhow::bail!(
            "file ends after frame {} with no End frame (torn write?)",
            index.saturating_sub(1)
        );
    }
    if rest < 8 {
        anyhow::bail!("frame {index}: header cut short ({rest} bytes left)");
    }
    let len = u32::from_le_bytes(bytes[*pos..*pos + 4].try_into().unwrap());
    let crc = u32::from_le_bytes(bytes[*pos + 4..*pos + 8].try_into().unwrap());
    if len > MAX_FRAME {
        anyhow::bail!("frame {index}: length {len} is implausible (damaged length field)");
    }
    let start = *pos + 8;
    let end = start + len as usize;
    if end > bytes.len() {
        anyhow::bail!(
            "frame {index}: needs {len} bytes, only {} left (torn write?)",
            bytes.len() - start
        );
    }
    let payload = &bytes[start..end];
    let actual = crc32(payload);
    if actual != crc {
        anyhow::bail!("frame {index}: checksum {actual:08x}, expected {crc:08x} (damaged)");
    }
    *pos = end;
    serde_json::from_slice(payload)
        .map_err(|e| anyhow::anyhow!("frame {index}: checksum ok but unreadable: {e}"))
}

// ── CRC-32 ──────────────────────────────────────────────────────────────────
//
// The IEEE 802.3 / zlib / PNG CRC-32: reflected, polynomial 0xEDB88320,
// initial value and final XOR 0xFFFFFFFF. Written out rather than pulled
// from a crate because it is twenty lines and worth knowing.
//
// A CRC treats the bytes as one long binary polynomial and keeps the
// remainder of dividing it by a fixed polynomial. Any burst of flipped
// bits shorter than 32 is always caught, and a random corruption slips
// through about once in 4 billion. It detects accidents, not tampering:
// anyone can recompute it. (Blobs use SHA-256 because their name has to be
// unforgeable; a snapshot only has to notice that the disk lied.)
//
// The table holds the remainder for each possible byte, so the inner loop
// handles 8 bits per step instead of 1.

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

static CRC_TABLE: [u32; 256] = crc_table();

pub fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = CRC_TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, EventKind, LinkFacts};
    use crate::ledger::Record;

    // The standard check values every CRC-32 implementation is tested against.
    #[test]
    fn crc32_matches_the_published_check_values() {
        assert_eq!(crc32(b""), 0x0000_0000);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
    }

    fn sample() -> Graph {
        let link = |seq, from: &str, label: &str, to: &str| {
            let mut event = Event::new("n", "n", None, None, EventKind::Linked, None);
            event.link = Some(LinkFacts {
                from: from.into(),
                label: label.into(),
                to: to.into(),
            });
            Record {
                v: 1,
                seq,
                device: "t".into(),
                event,
            }
        };
        Graph::from_records(&[
            link(1, "note", "ABOUT", "topic/pd"),
            link(2, "paper", "ABOUT", "topic/pd"),
            link(3, "note", "TAKEN_AT", "session/4"),
        ])
    }

    fn checkpoint() -> Checkpoint {
        Checkpoint {
            last_seq: 3,
            last_event_id: Uuid::new_v4(),
            ledger_offset: 900,
            open_arrivals: vec![],
        }
    }

    fn written() -> (tempfile::TempDir, std::path::PathBuf, Checkpoint) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.bin");
        let cp = checkpoint();
        write(&path, &sample(), &cp).unwrap();
        (dir, path, cp)
    }

    #[test]
    fn a_snapshot_reads_back_as_the_same_graph() {
        let (_d, path, cp) = written();
        let loaded = read(&path).unwrap().unwrap();
        let got = &loaded.checkpoint;
        assert_eq!(
            (got.last_seq, got.last_event_id, got.ledger_offset),
            (cp.last_seq, cp.last_event_id, cp.ledger_offset)
        );
        let g = sample();
        assert_eq!(loaded.graph.node_count(), g.node_count());
        assert_eq!(loaded.graph.edges(), g.edges());
        assert_eq!(
            loaded.graph.degrees(),
            g.degrees(),
            "adjacency rebuilt, not just the edge list"
        );
        assert_eq!(
            loaded.graph.hop(
                &["paper".into()],
                Some("ABOUT"),
                crate::graph::Direction::Out
            ),
            vec!["topic/pd".to_string()]
        );
    }

    #[test]
    fn no_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(&dir.path().join("snapshot.bin")).unwrap().is_none());
    }

    #[test]
    fn a_flipped_bit_is_caught_by_the_checksum() {
        let (_d, path, _) = written();
        let mut bytes = fs::read(&path).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0x01;
        fs::write(&path, &bytes).unwrap();
        let err = read(&path).unwrap_err().to_string();
        assert!(
            err.contains("checksum") || err.contains("implausible") || err.contains("needs"),
            "{err}"
        );
    }

    #[test]
    fn a_torn_write_is_caught_at_every_cut_point() {
        let (_d, path, _) = written();
        let full = fs::read(&path).unwrap();
        for cut in 0..full.len() {
            fs::write(&path, &full[..cut]).unwrap();
            assert!(
                read(&path).is_err(),
                "a file cut at byte {cut} of {} was accepted",
                full.len()
            );
        }
        fs::write(&path, &full).unwrap();
        assert!(read(&path).unwrap().is_some());
    }

    #[test]
    fn a_dropped_frame_is_caught_by_the_end_counts() {
        let (_d, path, _) = written();
        let bytes = fs::read(&path).unwrap();
        // Walk the frames and cut out the second one (the first Node).
        let len_at = |p: usize| u32::from_le_bytes(bytes[p..p + 4].try_into().unwrap()) as usize;
        let first = 8;
        let second = first + 8 + len_at(first);
        let third = second + 8 + len_at(second);
        let mut cut = bytes[..second].to_vec();
        cut.extend_from_slice(&bytes[third..]);
        fs::write(&path, &cut).unwrap();
        assert!(read(&path).unwrap_err().to_string().contains("End says"));
    }

    #[test]
    fn trailing_bytes_and_wrong_magic_are_rejected() {
        let (_d, path, _) = written();
        let mut bytes = fs::read(&path).unwrap();
        bytes.push(0);
        fs::write(&path, &bytes).unwrap();
        assert!(read(&path).unwrap_err().to_string().contains("after End"));
        fs::write(&path, b"JUNKJUNKJUNK").unwrap();
        assert!(read(&path).unwrap_err().to_string().contains("magic"));
    }
}
