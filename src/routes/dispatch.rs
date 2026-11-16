// src/routes/dispatch.rs
//
// POST /dispatch: the table-driven entry point.
//
// Looks the envelope's data_type up in the routing table (routes.toml) and
// runs the matching route through Dodo::run_route, so authorization is
// checked before any hop runs.
//
// Fan-out: a route with an Emit hop hands back envelopes it produced. Each
// one is then dispatched through its own route, breadth-first, with
// caused_by already set to the envelope that produced it. Depth is capped
// so a route cannot emit forever.
//
// Kept separate from POST /envelope, which still owns the classify-and-route
// flow for envelopes that arrive with no JD address.

use std::collections::VecDeque;
use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde_json::{json, Value};

use crate::{envelope::{Envelope, InboundEnvelope}, state::AppState};

/// How many levels of Emit may sit below the envelope POSTed in. Children
/// of the POSTed envelope are depth 1; their children depth 2; and so on.
const MAX_FANOUT_DEPTH: u8 = 3;

pub async fn dispatch(
    State(state): State<Arc<AppState>>,
    Json(inbound): Json<InboundEnvelope>,
) -> impl IntoResponse {
    let envelope = inbound.into_envelope();
    let envelope_id = envelope.id;

    let Some((dodo, resolved)) = state.router.resolve(&envelope.data_type) else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": "no route for data_type",
                "data_type": envelope.data_type,
            })),
        )
            .into_response();
    };

    let mut route = resolved.to_route();

    if let Err(e) = dodo.run_route(&mut route, &state.vault_root, Some(&envelope)).await {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": e.to_string(),
                "status": format!("{:?}", route.status),
            })),
        )
            .into_response();
    }

    let mut dispatched = Vec::new();
    let mut queue: VecDeque<(Envelope, u8)> =
        route.emitted.drain(..).map(|e| (e, 1)).collect();

    while let Some((child, depth)) = queue.pop_front() {
        dispatched.push(dispatch_child(&state, child, depth, &mut queue).await);
    }

    (
        StatusCode::CREATED,
        Json(json!({
            "envelope_id": envelope_id,
            "dodo": dodo.id,
            "nest": resolved.nest_id,
            "status": format!("{:?}", route.status),
            "dispatched": dispatched,
        })),
    )
        .into_response()
}

/// Run one emitted envelope through its own route. Its own emitted children
/// are pushed onto `queue` at depth + 1, unless they would exceed the cap.
async fn dispatch_child(
    state: &AppState,
    child: Envelope,
    depth: u8,
    queue: &mut VecDeque<(Envelope, u8)>,
) -> Value {
    let child_id = child.id;
    let child_type = child.data_type.clone();

    if depth > MAX_FANOUT_DEPTH {
        return json!({
            "envelope_id": child_id,
            "data_type": child_type,
            "error": format!("fan-out deeper than {MAX_FANOUT_DEPTH} levels; not dispatched"),
        });
    }

    let Some((dodo, resolved)) = state.router.resolve(&child_type) else {
        return json!({
            "envelope_id": child_id,
            "data_type": child_type,
            "error": "no route for data_type",
        });
    };

    let mut route = resolved.to_route();

    match dodo.run_route(&mut route, &state.vault_root, Some(&child)).await {
        Ok(()) => {
            queue.extend(route.emitted.drain(..).map(|e| (e, depth + 1)));
            json!({
                "envelope_id": child_id,
                "data_type": child_type,
                "dodo": dodo.id,
                "nest": resolved.nest_id,
                "status": format!("{:?}", route.status),
            })
        }
        Err(e) => json!({
            "envelope_id": child_id,
            "data_type": child_type,
            "error": e.to_string(),
            "status": format!("{:?}", route.status),
        }),
    }
}