/// 82_Mailroom — Route Table
///
/// Maps model names and JD area prefixes to backend endpoints + model IDs.
/// This is the human-AI middle ground: you edit this file to control
/// which model handles which part of your JD system.
///
/// JD area guide (your index):
///   00-09  Philosophy / Directives / Identity
///   10-19  Ecosystem (NixOS, machines, services, networking)
///   20-29  Estate (treasury, projects, rooms, orchard)
///   30-39  User (skills, quests, health, journal)
///   50-59  Library (resources, information sources, models)
///   60-69  Output (research, creative projects)
///   80-89  Data management (mailroom, scratchpad, logs)
///   90-99  Archives

use std::collections::HashMap;

/// A resolved backend target: where to send the request and with which model.
#[derive(Debug, Clone)]
pub struct RouteTarget {
    /// Full URL of the backend, e.g. "http://localhost:8080/v1/chat/completions"
    pub url: String,
    /// Model ID to use on that backend
    pub model: String,
    /// Human label for logs
    pub label: String,
}

/// The routing table loaded once at startup.
pub struct RouteTable {
    /// Explicit model alias → target  (checked first)
    /// e.g. "fast", "coder", "journal", "health"
    aliases: HashMap<String, RouteTarget>,

    /// JD area prefix → target  (checked second, by longest prefix match)
    /// e.g. "10" matches 10-19, "35" matches 35_Health specifically
    jd_areas: Vec<(String, RouteTarget)>,

    /// Fallback when nothing matches
    fallback: RouteTarget,
}

impl RouteTable {
    pub fn load() -> Self {
        // ── Backends ──────────────────────────────────────────────────────────
        // Adjust base URLs to match your actual LocalAI / Ollama setup.

        let local_ai   = "http://localhost:8080/v1/chat/completions";
        let ollama     = "http://localhost:11434/v1/chat/completions";

        // ── Aliases ───────────────────────────────────────────────────────────
        // These are the model names callers can send in the `model` field.
        // Add your own as you pull more models.
        let aliases = HashMap::from([
            // Generic aliases
            ("fast".into(),    target(local_ai, "phi-3-mini",       "Fast / phi-3-mini")),
            ("smart".into(),   target(local_ai, "mistral-7b",       "Smart / Mistral-7B")),
            ("coder".into(),   target(ollama,   "codellama",        "Coder / CodeLlama")),

            // JD-flavoured aliases (callers can use these directly)
            ("journal".into(), target(local_ai, "mistral-7b",       "34_My-story")),
            ("health".into(),  target(local_ai, "meditron",         "35_Health")),
            ("finance".into(), target(local_ai, "mistral-7b",       "21_Treasury")),
            ("quest".into(),   target(local_ai, "mistral-7b",       "33_Quests")),
            ("nixos".into(),   target(ollama,   "codellama",        "11_NixOS")),
            ("library".into(), target(local_ai, "mistral-7b",       "52_Library")),
        ]);

        // ── JD Area routing ───────────────────────────────────────────────────
        // Longest prefix wins. Order doesn't matter — we sort by length below.
        // Add sub-area overrides freely: "35.7" (mental health) can differ from "35".
        let mut jd_areas = vec![
            // 00-09 Philosophy & Directives → careful, deliberate model
            ("00".into(), target(local_ai, "mistral-7b",   "00_Philosophy")),
            ("01".into(), target(local_ai, "mistral-7b",   "01_Directives")),

            // 10-19 Ecosystem / tech → coder model
            ("10".into(), target(ollama,   "codellama",    "10_Ecosystem")),
            ("11".into(), target(ollama,   "codellama",    "11_NixOS")),
            ("12".into(), target(ollama,   "codellama",    "12_Machines")),
            ("14".into(), target(ollama,   "codellama",    "14_Services")),
            ("17".into(), target(ollama,   "codellama",    "17_Networking")),

            // 20-29 Estate → general assistant
            ("20".into(), target(local_ai, "mistral-7b",   "20_Estate")),
            ("21".into(), target(local_ai, "mistral-7b",   "21_Treasury")),
            ("22".into(), target(local_ai, "mistral-7b",   "22_Projects")),
            ("24".into(), target(local_ai, "phi-3-mini",   "24_Rooms")),

            // 30-39 User
            ("30".into(), target(local_ai, "mistral-7b",   "30_User")),
            ("33".into(), target(local_ai, "mistral-7b",   "33_Quests")),
            ("34".into(), target(local_ai, "mistral-7b",   "34_My-story")),
            ("35".into(), target(local_ai, "meditron",     "35_Health")),

            // 50-59 Library → research model
            ("50".into(), target(local_ai, "mistral-7b",   "50_Library")),
            ("51".into(), target(local_ai, "mistral-7b",   "51_Sources")),

            // 60-69 Output / creative
            ("61".into(), target(local_ai, "mistral-7b",   "61_Research")),
            ("62".into(), target(local_ai, "mistral-7b",   "62_Projects")),

            // 80-89 Data / Mailroom itself → fast model
            ("80".into(), target(local_ai, "phi-3-mini",   "80_Data")),
            ("82".into(), target(local_ai, "phi-3-mini",   "82_Mailroom")),
            ("83".into(), target(local_ai, "phi-3-mini",   "83_Scratchpad")),
        ];

        // Sort longest prefix first so "35.7" beats "35"
        jd_areas.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

        // ── Fallback ──────────────────────────────────────────────────────────
        let fallback = target(local_ai, "mistral-7b", "fallback");

        Self { aliases, jd_areas, fallback }
    }

    /// Resolve a (model_name, optional jd_address) pair to a backend target.
    ///
    /// Priority:
    ///   1. Exact alias match on model name
    ///   2. JD area prefix match (longest wins)
    ///   3. Passthrough — use model name as-is on the fallback backend
    ///   4. Fallback target
    pub fn resolve(&self, model: &str, jd: Option<&str>) -> RouteTarget {
        // 1. Alias
        if let Some(t) = self.aliases.get(model) {
            return t.clone();
        }

        // 2. JD area prefix
        if let Some(addr) = jd {
            for (prefix, target) in &self.jd_areas {
                if addr.starts_with(prefix.as_str()) {
                    return target.clone();
                }
            }
        }

        // 3. Pass through the raw model name to the fallback URL
        //    (lets callers use exact model IDs like "llama-3.1-8b" directly)
        if !model.is_empty() && model != "default" {
            return RouteTarget {
                url: self.fallback.url.clone(),
                model: model.to_string(),
                label: format!("passthrough:{}", model),
            };
        }

        // 4. Fallback
        self.fallback.clone()
    }
}

fn target(url: &str, model: &str, label: &str) -> RouteTarget {
    RouteTarget {
        url: url.to_string(),
        model: model.to_string(),
        label: label.to_string(),
    }
}
