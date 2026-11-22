// src/routes/graph.rs
//
// Read-only HTTP view of the graph. Requests read the live graph the
// ledger keeps (updated on every append, under the ledger's lock), so an
// answer always matches the ledger exactly and costs only the query.
//
// Node ids contain slashes ("topic/partial-derivatives"), so they go in
// query parameters, not the path.
//
//   GET /graph/stats                                 node/edge counts by label
//   GET /graph/node?id=course/calc-3                 one node and its edges
//   GET /graph/related?from=<id>&via=ABOUT&label=note
//                                                    the Calc 3 query
//   GET /graph/walk?from=<id>&depth=3&dir=in&labels=PART_OF,TAKEN_AT
//   GET /graph/path?from=<a>&to=<b>&dir=both&labels=ABOUT,TAKEN_AT,PART_OF
//   GET /graph/degrees                               M1 degree table
//   GET /graph/centrality?top=10&labels=ABOUT,CITES  M2 eigenvector centrality,
//                                                    per connected component
//   GET  /graph/nodes?label=recipe                   every node with a label (index)
//   GET  /graph/check                                full integrity check
//   POST /graph/unlink  {"from","label","to","reason"?}   withdraw one edge
//   POST /graph/retract {"id","reason"?}                  withdraw a node + its edges
//
// Corrections (unlink, retract) are ledger events like everything else:
// the history keeps the mistake and its correction. Content still arrives
// through /dispatch and routes; these two only take things back.
//   GET /graph/edges.csv                             edge list for NetworkX
//
// Wire it in by adding `.merge(graph::routes())` to the router in
// routes/mod.rs (before `.with_state(state)`), and `pub mod graph;`.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

use crate::graph::{Direction, Graph};
use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/graph/stats", get(stats))
        .route("/graph/node", get(node))
        .route("/graph/related", get(related))
        .route("/graph/walk", get(walk))
        .route("/graph/path", get(path))
        .route("/graph/degrees", get(degrees))
        .route("/graph/centrality", get(centrality))
        .route("/graph/nodes", get(nodes))
        .route("/graph/check", get(check))
        .route("/graph/unlink", post(unlink))
        .route("/graph/retract", post(retract))
        .route("/graph/edges.csv", get(edges_csv))
}

type Reply = Result<Response, (StatusCode, Json<serde_json::Value>)>;

/// Run `f` against the vault's live graph (kept up to date by the ledger
/// on every append; see ledger.rs).
fn with_graph(state: &AppState, f: impl FnOnce(&Graph) -> Reply) -> Reply {
    let ledger = crate::ledger::for_vault(&state.vault_root).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;
    ledger.with_graph(f)
}

fn bad(msg: impl Into<String>) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": msg.into() })),
    )
}

fn not_found(id: &str) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": format!("no node {id:?}") })),
    )
}

fn direction(
    s: Option<&str>,
    default: Direction,
) -> Result<Direction, (StatusCode, Json<serde_json::Value>)> {
    match s {
        None => Ok(default),
        Some(s) => Direction::parse(s)
            .ok_or_else(|| bad(format!("dir must be in, out or both, not {s:?}"))),
    }
}

/// "ABOUT, taken_at" → ["ABOUT", "TAKEN_AT"]. Empty means every label.
fn label_list(s: Option<&str>) -> Vec<String> {
    s.unwrap_or("")
        .split(',')
        .map(|l| l.trim().to_uppercase().replace([' ', '-'], "_"))
        .filter(|l| !l.is_empty())
        .collect()
}

async fn stats(State(state): State<Arc<AppState>>) -> Reply {
    with_graph(&state, |g| {
        let mut node_labels: BTreeMap<String, usize> = BTreeMap::new();
        for n in g.nodes() {
            if n.labels.is_empty() {
                *node_labels.entry("(unlabelled)".into()).or_default() += 1;
            }
            for l in &n.labels {
                *node_labels.entry(l.clone()).or_default() += 1;
            }
        }
        let mut edge_labels: BTreeMap<String, usize> = BTreeMap::new();
        for e in g.edges() {
            *edge_labels.entry(e.label.clone()).or_default() += 1;
        }
        Ok(Json(json!({
            "nodes": g.node_count(),
            "edges": g.edge_count(),
            "node_labels": node_labels,
            "edge_labels": edge_labels,
        }))
        .into_response())
    })
}

#[derive(Deserialize)]
struct NodeQuery {
    id: String,
}

async fn node(State(state): State<Arc<AppState>>, Query(q): Query<NodeQuery>) -> Reply {
    with_graph(&state, |g| {
        let n = g.node(&q.id).ok_or_else(|| not_found(&q.id))?;
        Ok(Json(json!({
            "node": n,
            "out": g.edges_of(&q.id, Direction::Out, None),
            "in": g.edges_of(&q.id, Direction::In, None),
        }))
        .into_response())
    })
}

#[derive(Deserialize)]
struct RelatedQuery {
    from: String,
    via: Option<String>,
    label: Option<String>,
}

async fn related(State(state): State<Arc<AppState>>, Query(q): Query<RelatedQuery>) -> Reply {
    with_graph(&state, |g| {
        if g.node(&q.from).is_none() {
            return Err(not_found(&q.from));
        }
        let via = label_list(Some(q.via.as_deref().unwrap_or("ABOUT")));
        let [via] = via.as_slice() else {
            return Err(bad("via takes exactly one edge label"));
        };
        let label = q.label.as_deref().unwrap_or("note");
        let found: Vec<_> = g
            .related(&q.from, via, label)
            .into_iter()
            .filter_map(|id| g.node(&id).cloned())
            .collect();
        Ok(
            Json(json!({ "from": q.from, "via": via, "label": label, "related": found }))
                .into_response(),
        )
    })
}

#[derive(Deserialize)]
struct WalkQuery {
    from: String,
    depth: Option<usize>,
    dir: Option<String>,
    labels: Option<String>,
}

async fn walk(State(state): State<Arc<AppState>>, Query(q): Query<WalkQuery>) -> Reply {
    with_graph(&state, |g| {
        if g.node(&q.from).is_none() {
            return Err(not_found(&q.from));
        }
        let dir = direction(q.dir.as_deref(), Direction::Both)?;
        let labels = label_list(q.labels.as_deref());
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let depth = q.depth.unwrap_or(2).min(10);
        let reached: Vec<_> = g
            .walk(&q.from, depth, dir, &labels)
            .into_iter()
            .map(|(id, d)| json!({ "depth": d, "node": g.node(&id) }))
            .collect();
        Ok(Json(json!({ "from": q.from, "depth": depth, "reached": reached })).into_response())
    })
}

#[derive(Deserialize)]
struct PathQuery {
    from: String,
    to: String,
    dir: Option<String>,
    labels: Option<String>,
}

async fn path(State(state): State<Arc<AppState>>, Query(q): Query<PathQuery>) -> Reply {
    with_graph(&state, |g| {
        for id in [&q.from, &q.to] {
            if g.node(id).is_none() {
                return Err(not_found(id));
            }
        }
        let dir = direction(q.dir.as_deref(), Direction::Both)?;
        let labels = label_list(q.labels.as_deref());
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let found = g.path(&q.from, &q.to, dir, &labels);
        Ok(Json(json!({
            "from": q.from,
            "to": q.to,
            "length": found.as_ref().map(|p| p.len() - 1),
            "path": found,
        }))
        .into_response())
    })
}

async fn degrees(State(state): State<Arc<AppState>>) -> Reply {
    with_graph(&state, |g| {
        Ok(Json(json!({ "edges": g.edge_count(), "degrees": g.degrees() })).into_response())
    })
}

async fn edges_csv(State(state): State<Arc<AppState>>) -> Reply {
    with_graph(&state, |g| {
        Ok((
            [(header::CONTENT_TYPE, "text/csv; charset=utf-8")],
            g.edges_csv(),
        )
            .into_response())
    })
}

#[derive(Deserialize)]
struct CentralityQuery {
    labels: Option<String>,
    top: Option<usize>,
    /// How many components to report, biggest first.
    components: Option<usize>,
}

async fn centrality(State(state): State<Arc<AppState>>, Query(q): Query<CentralityQuery>) -> Reply {
    let labels = label_list(q.labels.as_deref());
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let top = q.top.unwrap_or(10);
    let shown = q.components.unwrap_or(5);
    with_graph(&state, |g| {
        let comps = crate::graph::centrality::eigenvector(g, &labels).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
        })?;
        let total = comps.len();
        let listed: Vec<_> = comps
            .into_iter()
            .take(shown)
            .map(|c| {
                json!({
                    "size": c.nodes.len(),
                    "lambda": c.lambda,
                    "iterations": c.iterations,
                    "top": c.nodes.into_iter().take(top).collect::<Vec<_>>(),
                })
            })
            .collect();
        Ok(Json(json!({
            "labels": if labels.is_empty() { json!("all except system edges") } else { json!(labels) },
            "note": "scores are unit length per component; compare within a component, not across",
            "components_total": total,
            "components": listed,
        }))
        .into_response())
    })
}

#[derive(Deserialize)]
struct NodesQuery {
    label: String,
}

async fn nodes(State(state): State<Arc<AppState>>, Query(q): Query<NodesQuery>) -> Reply {
    with_graph(&state, |g| {
        let found = g.nodes_with_label(&q.label);
        Ok(Json(json!({ "label": q.label, "count": found.len(), "nodes": found })).into_response())
    })
}

async fn check(State(state): State<Arc<AppState>>) -> Reply {
    let ledger = crate::ledger::for_vault(&state.vault_root).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;
    // The ledger part runs on a blocking thread: it re-reads the whole file.
    let l = ledger.clone();
    let mut report = tokio::task::spawn_blocking(move || l.verify())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
        })?;

    // Blobs: re-hash every one this machine's coffer holds. A blob the
    // graph knows about but this coffer doesn't hold (text payloads, or
    // content kept only on another device) is not a problem.
    let contents = crate::coffer::for_vault(&state.vault_root)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
        })?
        .contents
        .clone();
    let (mut verified, mut absent) = (0usize, 0usize);
    let mut mismatched = Vec::new();
    for hash in &report.blob_hashes {
        if !contents.join(crate::blobs::rel_path(hash)).exists() {
            absent += 1;
            continue;
        }
        match crate::blobs::verify(&contents, hash).await {
            Ok(true) => verified += 1,
            Ok(false) => mismatched.push(hash.clone()),
            Err(e) => report.problems.push(format!("blob {hash}: {e}")),
        }
    }
    for h in &mismatched {
        report
            .problems
            .push(format!("blob {h}: bytes no longer match their hash"));
    }

    let status = if report.problems.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::CONFLICT
    };
    Ok((
        status,
        Json(json!({
            "ok": report.problems.is_empty(),
            "records": report.records,
            "problems": report.problems,
            "open_stops": report.open_stops,
            "dangling_causes": report.dangling_causes,
            "dangling_parents": report.dangling_parents,
            "snapshot": report.snapshot,
            "blobs": { "verified": verified, "mismatched": mismatched.len(), "not_stored_here": absent },
        })),
    )
        .into_response())
}

const SYSTEM_NODE_PREFIXES: [&str; 2] = ["nest:", "blob:"];

#[derive(Deserialize)]
struct UnlinkBody {
    from: String,
    label: String,
    to: String,
    reason: Option<String>,
}

async fn unlink(State(state): State<Arc<AppState>>, Json(b): Json<UnlinkBody>) -> Reply {
    let label = b.label.trim().to_uppercase().replace([' ', '-'], "_");
    if crate::graph::centrality::SYSTEM_LABELS.contains(&label.as_str()) {
        return Err(bad(format!(
            "{label} edges record what happened and can't be unlinked; retract the node instead"
        )));
    }
    let ledger = crate::ledger::for_vault(&state.vault_root).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;
    if !ledger.with_graph(|g| g.has_edge(&b.from, &label, &b.to)) {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("no edge {} -{label}-> {}", b.from, b.to) })),
        ));
    }
    let mut event = crate::event::Event::new(
        "graph",
        "graph",
        None,
        None,
        crate::event::EventKind::Unlinked,
        b.reason,
    );
    event.link = Some(crate::event::LinkFacts {
        from: b.from.clone(),
        label: label.clone(),
        to: b.to.clone(),
    });
    let record = ledger.append(event).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;
    Ok(Json(
        json!({ "unlinked": { "from": b.from, "label": label, "to": b.to }, "seq": record.seq }),
    )
    .into_response())
}

#[derive(Deserialize)]
struct RetractBody {
    id: String,
    reason: Option<String>,
}

async fn retract(State(state): State<Arc<AppState>>, Json(b): Json<RetractBody>) -> Reply {
    if SYSTEM_NODE_PREFIXES.iter().any(|p| b.id.starts_with(p)) {
        return Err(bad(format!(
            "{} is a system node (nest or blob) and can't be retracted",
            b.id
        )));
    }
    let ledger = crate::ledger::for_vault(&state.vault_root).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;
    let Some(edges) = ledger.with_graph(|g| {
        g.node(&b.id)
            .map(|_| g.edges_of(&b.id, Direction::Both, None).len())
    }) else {
        return Err(not_found(&b.id));
    };
    let mut event = crate::event::Event::new(
        "graph",
        "graph",
        None,
        None,
        crate::event::EventKind::Retracted,
        b.reason,
    );
    event.node = Some(b.id.clone());
    let record = ledger.append(event).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;
    Ok(
        Json(json!({ "retracted": b.id, "edges_removed": edges, "seq": record.seq }))
            .into_response(),
    )
}
