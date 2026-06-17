use anyhow::Result;
use std::sync::Arc;

use crate::{agents::AgentRegistry, config::MailroomConfig, johnny::JohnnyDecimalIndex};

/// Shared state injected into every Axum handler via `State<Arc<AppState>>`.
pub struct AppState {
    pub config: MailroomConfig,
    pub agents: AgentRegistry,
    pub jd: JohnnyDecimalIndex,
}

impl AppState {
    pub async fn new(config: MailroomConfig) -> Result<Self> {
        let agents = AgentRegistry::from_config(&config.agents);
        let jd = JohnnyDecimalIndex::load(&config.jd_root).await?;
        Ok(Self { config, agents, jd })
    }
}
