// src/routes/entries.rs
//
// GET /envelopes
//
// Lists entry metadata files from /storage/Library.
// Reads .meta.json sidecars written by store::store().
// No database needed — the sidecars are the index.
//
// Query parameters:
//   jd=34.2   filter by exact JD address
//   jd=34     filter by JD area prefix (all nodes under 34)
//   limit=20  cap results (default 50)
//
// Sort order:
//   1. Pinned entries first
//   2. Starred entries second
//   3. Everything else newest first

use std::{path::Path, sync::Arc};

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use tokio::fs;
use walkdir::WalkDir;

use crate::{state::AppState, store::EntryMeta};

// ── Query params ──────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ListParams {
    pub jd:    Option<String>,
    // Filter by JD address or prefix.
    // "34.2" → exact match
    // "34"   → all entries whose jd_address starts with "34"

    pub limit: Option<usize>,
    // Maximum number of entries to return.
    // Defaults to 50 if absent.
}

// ── Response ──────────────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
pub struct EntriesResponse {
    pub total:   usize,
    // Total number of entries returned (after filtering and limiting).

    pub entries: Vec<EntryMeta>,
    // The entries themselves, sorted by priority then recency.
}

// ── Handler ───────────────────────────────────────────────────────────────────

/// GET /envelopes
///
/// Walks the Library directory, reads all .meta.json sidecars,
/// filters by JD address if requested, sorts, and returns.
pub async fn list(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListParams>,
    // Query<T> extracts ?key=value parameters from the URL.
    // Axum deserializes them into ListParams automatically.
    // If a required field is missing it returns 400 — but since
    // all our fields are Option, missing params just become None.
) -> impl IntoResponse {
    let library_root = &state.library_root;

    if !library_root.exists() {
        // Library doesn't exist yet — return empty list, not an error.
        // This is normal during initial setup.
        return (StatusCode::OK, Json(EntriesResponse {
            total:   0,
            entries: vec![],
        }));
    }

    // ── Collect all .meta.json files ──────────────────────────────────────────
    let mut entries: Vec<EntryMeta> = Vec::new();

    for dir_entry in WalkDir::new(&library_root)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = dir_entry.path();

        // We only want .meta.json files — the sidecars store() wrote.
        if !is_meta_file(path) {
            continue;
            // `continue` skips the rest of this loop iteration
            // and moves to the next entry. Like `next` in Ruby.
        }

        // Read and deserialize the sidecar.
        match read_meta(path).await {
            Ok(meta) => {
                // Apply JD filter if one was provided.
                if matches_filter(&meta, &params.jd) {
                    entries.push(meta);
                }
            }
            Err(e) => {
                // Log bad sidecars but keep going —
                // one corrupt file shouldn't break the list.
                tracing::warn!(
                    path  = %path.display(),
                    error = %e,
                    "failed to read meta file — skipping"
                );
            }
        }
    }

    // ── Sort ──────────────────────────────────────────────────────────────────
    entries.sort_by(|a, b| {
        // sort_by takes a closure that compares two elements.
        // We return std::cmp::Ordering: Less, Equal, or Greater.
        // The sort is stable — equal elements keep their relative order.

        // First: pinned entries before everything else.
        match (a.pinned, b.pinned) {
            (true, false) => return std::cmp::Ordering::Less,
            // a is pinned, b is not → a comes first (Less = earlier)
            (false, true) => return std::cmp::Ordering::Greater,
            _ => {}
            // Both pinned or both not pinned → fall through to next sort
        }

        // Second: starred entries before normal entries.
        match (a.starred, b.starred) {
            (true, false) => return std::cmp::Ordering::Less,
            (false, true) => return std::cmp::Ordering::Greater,
            _ => {}
        }

        // Third: newest first by created_at.
        b.created_at.cmp(&a.created_at)
        // Note: b.cmp(a) not a.cmp(b) — reversed for newest-first.
        // DateTime implements Ord so .cmp() works directly.
    });

    // ── Apply limit ───────────────────────────────────────────────────────────
    let limit = params.limit.unwrap_or(50);
    // unwrap_or: if limit param was absent, default to 50.

    entries.truncate(limit);
    // truncate(n) keeps only the first n elements, discarding the rest.
    // If entries.len() < limit, nothing changes.

    let total = entries.len();

    (StatusCode::OK, Json(EntriesResponse { total, entries }))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Returns true if this path is a .meta.json sidecar file.
fn is_meta_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.ends_with(".meta.json"))
        // .map applies a function to the inner value of Option.
        // If file_name() returned None, map returns None.
        .unwrap_or(false)
        // unwrap_or: if any step returned None, return false.
}

/// Read and deserialize a .meta.json sidecar file.
async fn read_meta(path: &Path) -> anyhow::Result<EntryMeta> {
    let contents = fs::read_to_string(path).await?;
    // Read the file as a UTF-8 string. ? propagates any IO error.

    let meta: EntryMeta = serde_json::from_str(&contents)?;
    // Deserialize JSON into EntryMeta. ? propagates parse errors.

    Ok(meta)
}

/// Returns true if this entry matches the JD filter.
/// None filter = match everything.
fn matches_filter(meta: &EntryMeta, filter: &Option<String>) -> bool {
    match filter {
        None => true,
        // No filter provided — everything matches.

        Some(prefix) => {
            meta.jd_address
                .as_deref()
                // as_deref: Option<String> → Option<&str>
                // Lets us compare without cloning.
                .map(|addr| addr.starts_with(prefix.as_str()))
                // "34.2".starts_with("34") → true
                // "34.2".starts_with("34.2") → true
                // "35.1".starts_with("34") → false
                .unwrap_or(false)
                // Entry has no jd_address → doesn't match any filter
        }
    }
}