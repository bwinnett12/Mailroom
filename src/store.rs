// src/store.rs
//
// Writes envelopes to disk under /storage/Library.
//
// Every accepted envelope produces two files:
//   {timestamp}_{origin}_{jd}_{short-id}.{ext}       ← the content
//   {timestamp}_{origin}_{jd}_{short-id}.meta.json   ← the index entry
//
// The content file holds the raw payload.
// The meta file holds everything else — id, source, data_type, tags.
// Together they are a self-describing, database-free archive entry.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::fs;
// tokio::fs is the async version of std::fs.
// We use it here because store() is called from async handlers —
// blocking on disk I/O inside an async context would stall the runtime.
// tokio::fs hands the I/O to the OS and suspends until it's done.

use crate::envelope::{Envelope, Payload, Source};

// ── Origin code ───────────────────────────────────────────────────────────────

/// Short uppercase code representing the envelope's source.
/// Embedded in the filename so the origin is readable without
/// opening the file or querying a database.
fn origin_code(source: &Source) -> &'static str {
    // &'static str — a string literal baked into the binary.
    // Lives for the entire program lifetime.
    // Perfect for short fixed codes like these.
    match source {
        Source::Manual          => "MAN",
        Source::Device(_)       => "DEV",
        Source::WebHook(_)      => "WEB",
        Source::Internal(_)     => "INT",
        Source::Forwarded(_)    => "FWD",
        Source::Other(_)        => "OTH",
        // The _ inside each variant means "I know there's data here,
        // but I don't need it for this match." The compiler would warn
        // if we wrote Device without the _.
    }
}

// ── File extension ────────────────────────────────────────────────────────────

/// File extension for the content file, derived from the payload type.
/// The file is what it is — no wrapping, no indirection.
fn content_extension(payload: &Payload) -> &'static str {
    match payload {
        Payload::Text(_)     => "md",
        Payload::Json(_)     => "json",
        Payload::Bytes(_)    => "bin",
        Payload::FilePath(_) => "ref",
        // .ref = a reference file — contains the path, not the bytes.
        // The actual file lives wherever FilePath points.
        // We don't copy large files into entries/ — we record where they are.
        Payload::Url(_)      => "url",
        // .url = a URL reference file — contains the URL as plain text.
    }
}

// ── Filename builder ──────────────────────────────────────────────────────────

/// Build the base filename (without extension) for an envelope.
///
/// Format: {timestamp}_{origin}_{jd}_{short-id}
/// Example: 20250616T143200Z_MAN_34.2_72f489ec
pub fn build_filename(envelope: &Envelope) -> String {
    let timestamp = format_timestamp(envelope.created_at);
    let origin    = origin_code(&envelope.source);
    let jd        = envelope.jd_address
        .as_deref()
        // as_deref() converts Option<String> → Option<&str>.
        // Lets us use the string by reference without cloning.
        .unwrap_or("XX");
        // XX = no address — shouldn't happen for accepted envelopes
        // but we handle it gracefully rather than panicking.
    let short_id  = &envelope.id.to_string()[..8];
    // to_string() converts Uuid to its hyphenated string form.
    // [..8] takes the first 8 characters — enough to be unique
    // in any realistic personal archive.
    // e.g. "72f489ec" from "72f489ec-def5-4bca-8e7d-8c25bc60c637"

    format!("{timestamp}_{origin}_{jd}_{short_id}")
}

/// Format a DateTime<Utc> as a compact ISO 8601 string safe for filenames.
/// Colons aren't allowed in filenames on some systems — we omit them.
/// Example: 2025-06-16T14:32:00Z → 20250616T143200Z
fn format_timestamp(dt: DateTime<Utc>) -> String {
    dt.format("%Y%m%dT%H%M%SZ").to_string()
    // chrono's format() uses strftime-style codes:
    // %Y = 4-digit year, %m = month, %d = day
    // %H = hour, %M = minute, %S = second
    // T and Z are literal characters
}

// ── Meta sidecar ──────────────────────────────────────────────────────────────

/// The sidecar metadata file written alongside every content file.
/// This is what PostgreSQL will eventually read to build its index.
/// Until then, it makes the archive self-describing.
#[derive(Debug, Serialize, Deserialize)]
// Add Deserialize — we now read these back from disk in the list handler
pub struct EntryMeta {
    pub envelope_id:  String,
    pub jd_address:   Option<String>,
    pub data_type:    String,
    pub source:       String,
    pub created_at:   DateTime<Utc>,
    pub received_at:  DateTime<Utc>,
    pub meta:         std::collections::HashMap<String, String>,
    pub content_file: String,

    // ── New fields ────────────────────────────────────────────────
    #[serde(default)]
    pub pinned: bool,
    // Pinned entries always sort to the top.
    // Set by the Mailroom for overview/index entries.
    // Example: a node's overview.md is always pinned.

    #[serde(default)]
    pub starred: bool,
    // Starred entries are user-marked as important.
    // Set by the user via the front-end or a future PATCH endpoint.
    // Sorts above normal entries but below pinned.
}
impl EntryMeta {
    fn from_envelope(envelope: &Envelope, content_filename: &str) -> Self {
        Self {
            envelope_id:  envelope.id.to_string(),
            jd_address:   envelope.jd_address.clone(),
            data_type:    envelope.data_type.clone(),
            source:       format!("{:?}", envelope.source),
            created_at:   envelope.created_at,
            received_at:  envelope.received_at,
            meta:         envelope.meta.clone(),
            content_file: content_filename.to_string(),
            pinned:       false,
            starred:      false,
        }
    }
}

// ── WriteResult ───────────────────────────────────────────────────────────────

/// What store() returns on success — the paths of the files written.
#[derive(Debug)]
pub struct WriteResult {
    pub content_path: PathBuf,
    pub meta_path:    PathBuf,
}

// ── store() ───────────────────────────────────────────────────────────────────

/// Write an envelope to disk under library_root/{jd_path}/entries/.
///
/// `library_root` is /storage/Library (or local equivalent during dev).
/// `jd_path` is the relative path to the JD node directory,
///   e.g. "34_My-story/34.2_Journal"
///
/// Returns the paths of the two files written.
pub async fn store(
    envelope:     &Envelope,
    library_root: &Path,
    jd_path:      &str,
    // The relative path within the library to this node's directory.
    // We get this from the registry manifest — not derived from the
    // JD address alone, because the folder might have a custom name.
) -> anyhow::Result<WriteResult> {

    // ── Build the entries directory path ──────────────────────────────────────
    let entries_dir = library_root
        .join(jd_path)
        // .join() appends a path segment — like Path::new(a).push(b)
        // but returns a new PathBuf rather than mutating.
        .join("entries");

    // ── Create the directory if it doesn't exist ──────────────────────────────
    fs::create_dir_all(&entries_dir).await?;
    // create_dir_all creates the full path including any missing parents.
    // Like `mkdir -p` in bash.
    // The & borrows entries_dir — create_dir_all needs a reference, not ownership.

    // ── Build filenames ───────────────────────────────────────────────────────
    let base      = build_filename(envelope);
    let ext       = content_extension(&envelope.payload);
    let content_filename = format!("{base}.{ext}");
    let meta_filename    = format!("{base}.meta.json");

    let content_path = entries_dir.join(&content_filename);
    let meta_path    = entries_dir.join(&meta_filename);

    // ── Write content file ────────────────────────────────────────────────────
    write_payload(&envelope.payload, &content_path).await?;

    // ── Write meta sidecar ────────────────────────────────────────────────────
    let meta    = EntryMeta::from_envelope(envelope, &content_filename);
    let meta_json = serde_json::to_string_pretty(&meta)?;
    // to_string_pretty formats JSON with indentation — readable on disk.
    // to_string (without pretty) is more compact — we prefer readable here.

    fs::write(&meta_path, meta_json).await?;
    // fs::write creates or overwrites the file with the given bytes.
    // String implements AsRef<[u8]> so it converts automatically.

    tracing::info!(
        content = %content_path.display(),
        meta    = %meta_path.display(),
        "envelope written to disk"
    );

    Ok(WriteResult { content_path, meta_path })
}

// ── Payload writer ────────────────────────────────────────────────────────────

/// Write the payload content to the given path.
/// Each payload variant writes its content differently.
async fn write_payload(payload: &Payload, path: &Path) -> anyhow::Result<()> {
    match payload {
        Payload::Text(text) => {
            fs::write(path, text).await?;
            // Write the string directly — it's already the content.
        }

        Payload::Json(value) => {
            let json = serde_json::to_string_pretty(value)?;
            fs::write(path, json).await?;
        }

        Payload::Bytes(bytes) => {
            fs::write(path, bytes).await?;
            // Vec<u8> writes directly as raw bytes.
        }

        Payload::FilePath(src_path) => {
            // For file references we write the path as text — we don't
            // copy the file. Large files (audio, video) stay where they are.
            // The .ref file is a pointer, not a copy.
            let content = format!(
                "ref: {}\n",
                src_path.display()
                // .display() formats a PathBuf for human-readable output.
                // Handles platform path separators correctly.
            );
            fs::write(path, content).await?;
        }

        Payload::Url(url) => {
            // Write the URL as plain text — simple and unambiguous.
            fs::write(path, url).await?;
        }
    }

    Ok(())
    // Ok(()) — success with no meaningful return value.
    // The () is "unit" — Rust's way of saying "nothing to return."
}