use axum::{extract::State, Json};
use reqwest::Client;
use serde::Serialize;
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::config::AgentConfig;
use crate::ingest::envelope::{Envelope, Payload, SourceKind};
use crate::router::AppState;

/// Holds all configured downstream agents and dispatches envelopes to them.
pub struct AgentRegistry {
    pub agents: Vec<AgentConfig>,
    client: Client,
}

impl AgentRegistry {
    pub fn from_config(configs: &[AgentConfig]) -> Self {
        Self {
            agents: configs.to_vec(),
            client: Client::new(),
        }
    }

    /// Dispatch an envelope to relevant agents.
    /// Returns the names of agents that received it.
    pub async fn dispatch(&self, envelope: &Envelope) -> Vec<String> {
        let source_str = envelope.source.as_str();
        let mut dispatched = Vec::new();

        // Collect matching agents
        let targets: Vec<&AgentConfig> = if let Some(dest) = &envelope.destination {
            // Explicit destination
            self.agents.iter().filter(|a| &a.name == dest).collect()
        } else {
            // Fan-out: all agents that accept this source type
            self.agents
                .iter()
                .filter(|a| a.accepts.iter().any(|t| t == source_str || t == "*"))
                .collect()
        };

        if targets.is_empty() {
            warn!(source = source_str, "No agents matched for envelope");
            return dispatched;
        }

        for agent in targets {
            match self.send_to_agent(agent, envelope).await {
                Ok(_) => {
                    info!(agent = %agent.name, envelope = %envelope.id, "Dispatched");
                    dispatched.push(agent.name.clone());
                }
                Err(e) => {
                    error!(agent = %agent.name, error = %e, "Dispatch failed");
                }
            }
        }

        dispatched
    }

    async fn send_to_agent(&self, agent: &AgentConfig, envelope: &Envelope) -> anyhow::Result<()> {
        // Route to appropriate agent adapter based on base_url conventions
        if agent.base_url.contains("11434") {
            // Ollama-style local inference
            super::ollama::send(self.client.clone(), agent, envelope).await
        } else {
            // Generic: POST the envelope JSON to agent's /receive endpoint
            let url = format!("{}/receive", agent.base_url.trim_end_matches('/'));
            self.client.post(&url).json(envelope).send().await?;
            Ok(())
        }
    }
}

// --- HTTP handler ---

#[derive(Debug, Serialize)]
pub struct AgentInfo {
    pub name: String,
    pub base_url: String,
    pub accepts: Vec<String>,
    pub jd_prefix: Option<String>,
}

pub async fn list_agents(State(state): State<Arc<AppState>>) -> Json<Vec<AgentInfo>> {
    let info = state.agents.agents.iter().map(|a| AgentInfo {
        name: a.name.clone(),
        base_url: a.base_url.clone(),
        accepts: a.accepts.clone(),
        jd_prefix: a.jd_prefix.clone(),
    }).collect();
    Json(info)
}
