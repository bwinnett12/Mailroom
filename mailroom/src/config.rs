use anyhow::Result;
use serde::Deserialize;

/// Top-level mailroom configuration.
/// Loaded from `mailroom.toml` (or env vars with MAILROOM_ prefix).
#[derive(Debug, Clone, Deserialize)]
pub struct MailroomConfig {
    /// Address to bind the HTTP server on
    #[serde(default = "default_bind")]
    pub bind_addr: String,

    /// Registered downstream agents
    pub agents: Vec<AgentConfig>,

    /// Johnny Decimal root path (filesystem dir containing JD index)
    #[serde(default = "default_jd_root")]
    pub jd_root: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    /// Human-readable name, e.g. "local-llama"
    pub name: String,
    /// Base URL of the agent's API, e.g. "http://localhost:11434"
    pub base_url: String,
    /// Which source types this agent accepts
    pub accepts: Vec<String>,
    /// Optional: Johnny Decimal area/category this agent is associated with
    pub jd_prefix: Option<String>,
}

impl MailroomConfig {
    pub fn load() -> Result<Self> {
        dotenvy::dotenv().ok();

        let cfg = config::Config::builder()
            .add_source(config::File::with_name("mailroom").required(false))
            .add_source(config::Environment::with_prefix("MAILROOM").separator("__"))
            // Defaults if no file found
            .set_default("bind_addr", "0.0.0.0:3000")?
            .set_default("jd_root", "./jd")?
            .set_default("agents", Vec::<config::Value>::new())?
            .build()?
            .try_deserialize()?;

        Ok(cfg)
    }
}

fn default_bind() -> String { "0.0.0.0:3000".into() }
fn default_jd_root() -> String { "./jd".into() }
