// src/routes/journal.rs
//
// Convenience routes for the journal (34.2_Journal).
//
// POST /journal
//   Accepts plain text — wraps it in an envelope addressed to 34.2
//   automatically. No need to know the JD system to write a journal entry.
//
// GET /journal/summary
//   Reads all entries written today, sends them to LocalAI, returns
//   a daily summary. The summary itself is also saved as an entry
//   so it becomes part of the permanent record.

use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::{
    envelope::{InboundEnvelope, Payload, Source},
    inference::InferenceClient,
    state::AppState,
    store,
};

// ── POST /journal ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct JournalEntry {
    /// The text content of the journal entry.
    pub content: String,

    /// Optional timestamp — if absent, uses now.
    /// Useful for backdating entries written offline.
    pub created_at: Option<chrono::DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct JournalResponse {
    pub envelope_id: String,
    pub file:        String,
    pub jd_address:  String,
}

/// POST /journal
///
/// Convenience endpoint — wraps text in an envelope and routes it
/// to 34.2_Journal automatically. The caller just sends text.
pub async fn write(
    State(state): State<Arc<AppState>>,
    Json(body):   Json<JournalEntry>,
) -> impl IntoResponse {
    // Build an InboundEnvelope addressed to the journal.
    // This is exactly what the POST /envelope endpoint does,
    // but with the JD address pre-filled.
    let inbound = InboundEnvelope {
        source:     Source::Manual,
        data_type:  "text/journal".to_string(),
        payload:    Payload::Text(body.content),
        jd_address: Some("34.2".to_string()),
        meta:       std::collections::HashMap::new(),
        created_at: body.created_at,
    };

    let envelope = inbound.into_envelope();

    // Look up the journal node in the registry.
    let manifest = match state.registry.get("34.2") {
        Some(m) => m,
        None => {
            tracing::error!("34.2_Journal not found in registry");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "journal node not found in registry",
                    "hint": "ensure 34.2_Journal has a .mailroom file in your vault"
                })),
            ).into_response();
        }
    };

    let jd_path = manifest.effective_path();

    match store::store(&envelope, &state.library_root, &jd_path).await {
        Ok(result) => {
            tracing::info!(
                file = %result.content_path.display(),
                "journal entry written"
            );

            (StatusCode::CREATED, Json(serde_json::json!({
                "envelope_id": envelope.id.to_string(),
                "file":        result.content_path.display().to_string(),
                "jd_address":  "34.2",
            }))).into_response()
        }

        Err(e) => {
            tracing::error!(error = %e, "failed to write journal entry");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            ).into_response()
        }
    }
}

// ── GET /journal/summary ──────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SummaryResponse {
    pub date:        String,
    pub entry_count: usize,
    pub summary:     String,
    pub saved_to:    Option<String>,
    // Path where the summary was saved, if successful.
}

/// GET /journal/summary
///
/// Reads all journal entries written today, sends them to LocalAI,
/// and returns a summary. The summary is also saved back to 34.2_Journal
/// as a pinned entry so it becomes part of the permanent record.
pub async fn summary(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
	let today = Utc::now().format("%Y%m%d").to_string();
    // Local::now() uses the server's local timezone — correct for a
    // personal journal. Utc::now() would give the wrong "day" if you're
    // in a timezone behind UTC.

    let journal_dir = state.library_root
        .join("34_My-story")
        .join("34.2_Journal")
        .join("entries");

    if !journal_dir.exists() {
        return (StatusCode::OK, Json(SummaryResponse {
            date:        today,
            entry_count: 0,
            summary:     "No journal entries found yet.".to_string(),
            saved_to:    None,
        })).into_response();
    }

    // ── Collect today's entries ───────────────────────────────────────────
    let mut entries: Vec<String> = Vec::new();

    let mut dir = match fs::read_dir(&journal_dir).await {
        Ok(d)  => d,
        Err(e) => {
            tracing::error!(error = %e, "failed to read journal directory");
            return (StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() }))
            ).into_response();
        }
    };

    // Walk the entries directory, collect files from today.
    while let Ok(Some(entry)) = dir.next_entry().await {
        // next_entry() is async — it suspends while the OS reads the dir.
        let name = entry.file_name();
        let name = name.to_string_lossy();

        // Skip meta files — we only want content files.
        // Skip summary files from previous runs.
        if name.ends_with(".meta.json") || name.contains("_SUMMARY_") {
            continue;
        }

        // Our filename format: 20260625T020721Z_MAN_34.2_eff58236.md
        // The first 8 chars are the date: 20260625
        if name.starts_with(&today) {
            match fs::read_to_string(entry.path()).await {
                Ok(content) => {
                    if !content.trim().is_empty() {
                        entries.push(content);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        file  = %name,
                        error = %e,
                        "failed to read entry — skipping"
                    );
                }
            }
        }
    }

    let entry_count = entries.len();

    if entries.is_empty() {
		return (StatusCode::OK, Json(SummaryResponse {
			date:        today.clone(),
			entry_count: 0,
			summary:     format!("No journal entries for {} yet.", today),
            saved_to:    None,
        })).into_response();
    }

    tracing::info!(
        date  = %today,
        count = entry_count,
        "generating daily summary"
    );

    // ── Build context for LocalAI ─────────────────────────────────────────
    let entries_text = entries
        .iter()
        .enumerate()
        .map(|(i, e)| format!("Entry {}:\n{}", i + 1, e.trim()))
        // enumerate() gives (index, value) pairs.
        // We number each entry so the model can reference them.
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    // join with a markdown divider between entries.

    let context = format!(
        "These are journal entries written on {}. \
         This is a personal journal in a system organised by Johnny Decimal. \
         The journal (34.2) is part of 34_My-story which captures daily life, \
         reflections, and experiences.",
        today
    );

    // ── Call LocalAI ──────────────────────────────────────────────────────
    let ai = InferenceClient::new(&state.inference, &state.http_client);

    let prompt = format!(
        "Please write a concise daily summary of these journal entries. \
         Capture the key themes, events, and any notable reflections. \
         Write in second person (\"You...\") as if reflecting back to the author. \
         Keep it to 2-3 paragraphs.\n\n{}",
        entries_text
    );

    let summary_text = match ai.summarise(&prompt, &context).await {
        Ok(s)  => s,
        Err(e) => {
            tracing::error!(error = %e, "LocalAI summarisation failed");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": "summarisation failed — is Island's LocalAI running?",
                    "detail": e.to_string()
                }))
            ).into_response();
        }
    };

    // ── Save summary back to journal ──────────────────────────────────────
    // The summary is itself a journal entry — pinned so it sorts to top.
    // Filename includes _SUMMARY_ so we can skip it when reading entries.
    let summary_filename = format!(
        "{}T000000Z_INT_34.2_SUMMARY_.md",
        today
        // INT = Internal source (generated by Mailroom)
        // T000000Z = midnight, so it sorts before other entries
    );

    let summary_path = journal_dir.join(&summary_filename);

    let summary_content = format!(
        "# Daily Summary — {}\n\n{}\n\n---\n*Generated by Mailroom at {}*\n",
        today,
        summary_text,
        Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
    );

    let saved_to = match fs::write(&summary_path, &summary_content).await {
        Ok(_) => {
            tracing::info!(
                file = %summary_path.display(),
                "daily summary saved"
            );
            Some(summary_path.display().to_string())
        }
        Err(e) => {
            // Don't fail the response if saving fails —
            // the summary text is still returned to the caller.
            tracing::warn!(error = %e, "failed to save summary to disk");
            None
        }
    };

    (StatusCode::OK, Json(SummaryResponse {
        date: today,
        entry_count,
        summary: summary_text,
        saved_to,
    })).into_response()
}