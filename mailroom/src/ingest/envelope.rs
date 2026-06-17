use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Every piece of data that flows through the mailroom is wrapped in an Envelope.
/// This is the canonical inter-process format — both human-readable metadata
/// and the raw payload are carried together.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    /// Unique message ID
    pub id: Uuid,
    /// When this envelope was created
    pub timestamp: DateTime<Utc>,
    /// Source that produced this data
    pub source: SourceKind,
    /// Optional Johnny Decimal address for routing/filing
    pub jd_address: Option<String>,
    /// Who/what should receive this (agent name, broadcast = None)
    pub destination: Option<String>,
    /// The actual payload
    pub payload: Payload,
    /// Arbitrary key-value metadata (e.g. device_id, sample_rate, resolution)
    pub meta: serde_json::Map<String, serde_json::Value>,
}

impl Envelope {
    pub fn new(source: SourceKind, payload: Payload) -> Self {
        Self {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            source,
            jd_address: None,
            destination: None,
            payload,
            meta: serde_json::Map::new(),
        }
    }

    pub fn with_jd(mut self, addr: impl Into<String>) -> Self {
        self.jd_address = Some(addr.into());
        self
    }

    pub fn for_agent(mut self, agent: impl Into<String>) -> Self {
        self.destination = Some(agent.into());
        self
    }
}

/// Where did this data come from?
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Camera,
    Microphone,
    Video,
    Text,
    File,
    Webhook,
    /// Escape hatch for future source types
    Custom(String),
}

impl SourceKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Camera    => "camera",
            Self::Microphone => "microphone",
            Self::Video     => "video",
            Self::Text      => "text",
            Self::File      => "file",
            Self::Webhook   => "webhook",
            Self::Custom(s) => s.as_str(),
        }
    }
}

/// The actual data, held as bytes with a MIME type, or pre-parsed JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Payload {
    /// Raw binary (image frame, audio chunk, video clip, arbitrary file)
    Binary {
        mime_type: String,
        /// Base64-encoded when serialised to JSON
        #[serde(with = "base64_bytes")]
        data: Vec<u8>,
    },
    /// UTF-8 text (transcription, prompt, log line, etc.)
    Text {
        content: String,
        mime_type: String,
    },
    /// Already-structured JSON (webhook bodies, agent responses)
    Json(serde_json::Value),
}

// ---------- base64 serde helper ----------
mod base64_bytes {
    use serde::{Deserialize, Deserializer, Serializer};
    use base64::{engine::general_purpose::STANDARD, Engine};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        STANDARD.decode(&s).map_err(serde::de::Error::custom)
    }
}
