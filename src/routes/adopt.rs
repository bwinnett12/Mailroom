// src/routes/adopt.rs
//
// Taking in files dropped into intake nests (see src/adopt.rs).
//
//   GET  /adopt          what's waiting (JSON); changes nothing
//   POST /adopt          adopt everything that isn't blocked (JSON report)
//   GET  /adopt/panel    the same list as an HTML fragment with a button
//   POST /adopt/panel    the button: adopt, then show what happened
//
// The panel is loaded into the home page by htmx.

use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};

use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/adopt", get(list).post(adopt))
        .route("/adopt/panel", get(panel).post(panel_adopt))
}

async fn outcome(state: &AppState, dry_run: bool) -> anyhow::Result<crate::adopt::Outcome> {
    // Write lock: a Rookery mints nests. Adoption is a manual batch, so
    // holding it for the duration is fine.
    let mut registry = state.registry.write().await;
    crate::adopt::run(&state.vault_root, &mut registry, &state.router, dry_run, crate::adopt::SETTLE).await
}

fn error(e: anyhow::Error) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": format!("{e:#}") }))).into_response()
}

async fn list(State(state): State<Arc<AppState>>) -> Response {
    match outcome(&state, true).await {
        Ok(o) => Json(o).into_response(),
        Err(e) => error(e),
    }
}

async fn adopt(State(state): State<Arc<AppState>>) -> Response {
    match outcome(&state, false).await {
        Ok(o) => Json(o).into_response(),
        Err(e) => error(e),
    }
}

async fn panel(State(state): State<Arc<AppState>>) -> Response {
    match outcome(&state, true).await {
        Ok(o) => Html(render(&o, None)).into_response(),
        Err(e) => Html(format!("<p>Could not list waiting files: {}</p>", esc(&format!("{e:#}")))).into_response(),
    }
}

async fn panel_adopt(State(state): State<Arc<AppState>>) -> Response {
    let done = match outcome(&state, false).await {
        Ok(o) => o,
        Err(e) => return Html(format!("<p>Adopt failed: {}</p>", esc(&format!("{e:#}")))).into_response(),
    };
    let after = outcome(&state, true).await.ok();
    Html(render(after.as_ref().unwrap_or(&done), Some(&done))).into_response()
}

fn render(list: &crate::adopt::Outcome, done: Option<&crate::adopt::Outcome>) -> String {
    let mut html = String::new();
    if let Some(d) = done {
        html.push_str(&format!("<p>Took in {} file(s).</p><ul>", d.adopted.len()));
        for a in &d.adopted {
            html.push_str(&format!(
                "<li>{} → {}{}{}</li>",
                esc(&name(&a.from)),
                esc(&a.nest),
                if a.minted { " (new nest)" } else { "" },
                if a.duplicate { " (already in the coffer)" } else { "" },
            ));
        }
        for f in &d.failed {
            html.push_str(&format!("<li>{}: failed, left in place: {}</li>", esc(&name(&f.path)), esc(&f.error)));
        }
        html.push_str("</ul>");
    }
    let ready = list.waiting.iter().filter(|w| w.blocked.is_none()).count();
    if list.waiting.is_empty() {
        html.push_str("<p>Nothing waiting in the intake nests.</p>");
        return html;
    }
    html.push_str("<ul>");
    for w in &list.waiting {
        html.push_str(&format!(
            "<li>{} in {} ({}, {}){}</li>",
            esc(&name(&w.path)),
            esc(&w.nest),
            esc(&w.data_type),
            size(w.bytes),
            w.blocked.as_ref().map(|b| format!(": <em>{}</em>", esc(b))).unwrap_or_default(),
        ));
    }
    html.push_str("</ul>");
    if ready > 0 {
        html.push_str(&format!(
            "<button hx-post=\"/adopt/panel\" hx-target=\"#adopt\" hx-swap=\"innerHTML\">Take in {ready} file(s)</button>"
        ));
    }
    html
}

fn name(p: &std::path::Path) -> String {
    p.file_name().unwrap_or_default().to_string_lossy().to_string()
}

fn size(bytes: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", units[i]) }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
