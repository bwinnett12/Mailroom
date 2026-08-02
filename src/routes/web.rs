use std::sync::Arc;
use askama::Template;

use crate::{
    dodo::Dodo,
    envelope::{InboundEnvelope, Payload, Source},
    nest::Nest,
    route::{Hop, Route, Stop},
    state::AppState,
};
use axum::{
    extract::{State, Form},
    response::{Html, IntoResponse, Response},
    http::StatusCode,
};

fn render<T: Template>(tmpl: T) -> Response {
    match tmpl.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("template error: {e}")).into_response(),
    }
}

#[derive(Template)]
#[template(path = "new_envelope.html")]
struct NewEnvelopeTemplate {
    routes: Vec<(String, String, bool)>,  // (id, name, is_rookery) — for the dropdown
}

pub async fn new_envelope_form(State(state): State<Arc<AppState>>) -> Response {
    let mut routes: Vec<(String, String, bool)> = state.registry.read().await.all()
        .into_iter()
        .map(|m| {
            let is_rookery = m.mailroom.as_ref().is_some_and(|c| c.mints_subnests);
            (m.id.clone(), m.name.clone(), is_rookery)
        })
        .collect();

    // Pin the inbox to the top of the list rather than leaving it
    // buried alphabetically — 95% of submissions go there, and
    // scrolling through 100+ JD addresses every time defeats the point
    // of a fast-entry form. Same idea as a country picker floating
    // your own country above an otherwise fully alphabetized list.
    if let Some(inbox_index) = routes.iter().position(|(id, _, _)| id == "30-C") {
        let inbox = routes.remove(inbox_index);
        routes.insert(0, inbox);
    }

    render(NewEnvelopeTemplate { routes })
}

#[derive(Debug, serde::Deserialize)]
pub struct SubmitForm {
    pub content: String,
    pub jd_address: String,
}

#[derive(Template)]
#[template(path = "submit_result.html")]
struct SubmitResultTemplate {
    success: bool,
    message: String,
    envelope_id: String,
    dodo_id: String,
}

/// The Dodo standing in for "a person submitting the web form." Its
/// available_nests are built fresh from the live registry on every
/// request — not cached — so it's automatically as current as the
/// registry itself, including any subnest minted moments earlier.
///
/// home_nest is a placeholder today (cloned from whatever the target
/// nest turns out to be) — it doesn't mean much yet. Real meaning
/// arrives with the Operator-per-Nest design (see the backlog's Dodo
/// roles note); nothing currently reads it besides get_home_nest(),
/// which nothing calls either.
async fn delivery_dodo_for(state: &AppState, target_id: &str) -> anyhow::Result<Dodo> {
    let registry = state.registry.read().await;
    let available_nests: Vec<Nest> = registry
        .all()
        .into_iter()
        .map(|m| Nest { manifest: m.clone(), children: Vec::new() })
        .collect();

    let home_nest = available_nests
        .iter()
        .find(|n| n.manifest.id == target_id)
        .cloned()
        .or_else(|| available_nests.first().cloned())
        .ok_or_else(|| anyhow::anyhow!("registry is empty — nothing to deliver to"))?;

    Ok(Dodo {
        id: "delivery-dodo".to_string(),
        home_nest,
        available_nests,
    })
}

pub async fn submit(
    State(state): State<Arc<AppState>>,
    Form(body): Form<SubmitForm>,
) -> Response {
    let inbound = InboundEnvelope {
        source: Source::Manual,
        data_type: "text/journal".to_string(),
        payload: Payload::Text(body.content),
        jd_address: Some(body.jd_address.clone()),
        meta: std::collections::HashMap::new(),
        tags: Vec::new(),
        created_at: None,
    };
    let envelope = inbound.into_envelope();
    let envelope_id = envelope.id.to_string();

    let dodo = match delivery_dodo_for(&state, &body.jd_address).await {
        Ok(d) => d,
        Err(e) => return render(SubmitResultTemplate {
            success: false,
            message: format!("Error: {e}"),
            envelope_id: envelope_id.clone(),
            dodo_id: "none".to_string(),
        }),
    };

    let mut route = Route::new(
        dodo.id.clone(),
        vec![Stop {
            nest_id: body.jd_address.clone(),
            hops: vec![Hop::Store],
        }],
    );

    // authorize() rejects an unknown jd_address on its own — it can
    // only ever be in available_nests if the registry actually has it
    // — so there's no separate "unknown route" pre-check needed here
    // anymore; a bad address surfaces as an authorization failure below.
    let result = match dodo.run_route(&mut route, &state.vault_root, Some(&envelope)).await {
        Ok(()) => SubmitResultTemplate {
            success: true,
            message: format!("Delivered to {}", body.jd_address),
            envelope_id: envelope_id.clone(),
            dodo_id: dodo.id.clone(),
        },
        Err(e) => SubmitResultTemplate {
            success: false,
            message: format!("Error: {e}"),
            envelope_id: envelope_id.clone(),
            dodo_id: dodo.id.clone(),
        },
    };
    render(result)
}