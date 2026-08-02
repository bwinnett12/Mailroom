use std::sync::Arc;
use askama::Template;

use crate::{
    envelope::{InboundEnvelope, Payload, Source},
    state::AppState,
    store,
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
    routes: Vec<(String, String)>,  // (id, name) — for the dropdown
}

pub async fn new_envelope_form(State(state): State<Arc<AppState>>) -> Response {
    let routes = state.registry.read().await.all()
        .into_iter()
        .map(|m| (m.id.clone(), m.name.clone()))
        .collect();
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

    let registry = state.registry.read().await;
    let manifest = match registry.get(&body.jd_address) {
        Some(m) => m,
        None => return render(SubmitResultTemplate {
            success: false,
            message: format!("Unknown route: {}", body.jd_address),
        }),
    };

    let result = match store::store(&envelope, &state.library_root, &manifest.effective_path()).await {
        Ok(r) => SubmitResultTemplate { success: true, message: format!("Saved to {}", r.content_path.display()) },
        Err(e) => SubmitResultTemplate { success: false, message: format!("Error: {e}") },
    };
    render(result)
}