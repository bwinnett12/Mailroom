mod routes;

use axum::{
    Router,
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Json,
    extract::State,
};
use reqwest::Client;
use routes::RouteTable;
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, sync::Arc};

// ── Request / response types ────────────────────────────────────────────────
// Kept identical to the original so existing callers don't break.

#[derive(Deserialize, Debug)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    #[serde(default = "default_temperature")]
    temperature: f32,
    /// Optional: JD address to influence routing.
    /// e.g. "35.2" routes to the health model, "11.1" to codellama.
    /// Callers can omit this and rely on the model alias instead.
    #[serde(default)]
    jd: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct Message {
    role: String,
    content: String,
}

fn default_temperature() -> f32 { 0.7 }

#[derive(Serialize, Deserialize, Debug)]
struct LocalAiResponse {
    choices: Vec<Choice>,
}

#[derive(Serialize, Deserialize, Debug)]
struct Choice {
    message: Message,
}

/// Outgoing payload — strips the `jd` field, rewrites `model`.
#[derive(Serialize, Debug)]
struct ForwardedRequest<'a> {
    model: &'a str,
    messages: &'a Vec<Message>,
    temperature: f32,
}

// ── App state ───────────────────────────────────────────────────────────────

struct AppState {
    client: Client,
    routes: RouteTable,
}

// ── Main ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mailroom=debug".into()),
        )
        .init();

    let state = Arc::new(AppState {
        client: Client::new(),
        routes: RouteTable::load(),
    });

    let app = Router::new()
        .route("/v1/chat/completions", post(handle_chat))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    tracing::info!("82_Mailroom listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

// ── Handler ─────────────────────────────────────────────────────────────────

async fn handle_chat(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<ChatRequest>,
) -> impl IntoResponse {

    // Resolve which backend + model to use
    let target = state.routes.resolve(&payload.model, payload.jd.as_deref());

    tracing::debug!(
        model_in  = %payload.model,
        jd        = ?payload.jd,
        routed_to = %target.label,
        backend   = %target.url,
        model_out = %target.model,
        "routing"
    );

    let forwarded = ForwardedRequest {
        model: &target.model,
        messages: &payload.messages,
        temperature: payload.temperature,
    };

    let response = state.client
        .post(&target.url)
        .json(&forwarded)
        .send()
        .await;

    match response {
        Ok(res) => {
            let status = res.status();
            match res.json::<LocalAiResponse>().await {
                Ok(api_json) => (StatusCode::OK, Json(api_json)).into_response(),
                Err(_) => {
                    tracing::error!(backend = %target.url, http_status = %status, "bad response from backend");
                    (StatusCode::BAD_GATEWAY, "Backend returned an unparseable response").into_response()
                }
            }
        }
        Err(err) => {
            tracing::error!(backend = %target.url, error = %err, "backend unreachable");
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Backend unreachable: {}", target.label)).into_response()
        }
    }
}
