# Mailroom

A central routing hub (built with Axum / Rust) that accepts data from any source —
camera frames, microphone audio, video, text, files, webhooks — wraps each in a
typed **Envelope**, and dispatches it to one or more downstream AI agents.

Johnny Decimal addresses act as the human-AI middle ground: every envelope can carry
a JD address that determines how it's filed and which agent handles it.

---

## Architecture

```
                    ┌─────────────────────────────────┐
  Camera  ──POST──▶ │                                 │
  Mic     ──POST──▶ │   /ingest/*   →  Envelope       │──▶  Ollama (llama3 / llava)
  Video   ──POST──▶ │                                 │──▶  n8n / custom agent
  Text    ──POST──▶ │   Johnny Decimal index           │──▶  …
  File    ──POST──▶ │   /jd/:address  ← lookup        │
  Webhook ──POST──▶ │                                 │
                    └─────────────────────────────────┘
```

### Envelope

Every payload becomes an `Envelope`:

```json
{
  "id": "uuid-v4",
  "timestamp": "2026-06-12T00:00:00Z",
  "source": "camera",
  "jd_address": "11.01",
  "destination": "local-llava",
  "payload": {
    "kind": "binary",
    "mime_type": "image/jpeg",
    "data": "<base64>"
  },
  "meta": { "device_id": "cam-0", "fps": 30 }
}
```

---

## Quick start

```bash
# 1. Install Rust (if needed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 2. Run Ollama locally (optional, for local AI)
ollama serve
ollama pull llama3
ollama pull llava

# 3. Configure
cp mailroom.toml.example mailroom.toml
# edit agents / JD root as needed

# 4. Run
cargo run

# Server listens on http://0.0.0.0:3000
```

---

## Endpoints

| Method | Path | Description |
|--------|------|-------------|
| POST | `/ingest/camera` | Camera frame (multipart `frame` field) |
| POST | `/ingest/microphone` | Audio chunk (multipart `audio` field) |
| POST | `/ingest/video` | Video clip (multipart `video` field) |
| POST | `/ingest/text` | Text / prompt (JSON body `{"content":"…"}`) |
| POST | `/ingest/file` | Any file (multipart `file` field) |
| POST | `/ingest/webhook` | Arbitrary JSON webhook |
| GET  | `/agents` | List registered agents |
| GET  | `/jd` | Full Johnny Decimal index |
| GET  | `/jd/:address` | Lookup by JD address / prefix |

### Query params (all ingest endpoints)

| Param | Description |
|-------|-------------|
| `agent` | Force a specific agent by name |
| `jd` | Tag the envelope with a JD address |

---

## Johnny Decimal integration

Edit `jd/index.json` to map JD addresses to agents:

```json
{
  "address": "11.01",
  "title": "Camera Frames",
  "agent": "local-llava",
  "tags": ["camera", "image"]
}
```

When you POST to `/ingest/camera?jd=11.01`, the mailroom:
1. Wraps the frame in an Envelope tagged `11.01`
2. Looks up `11.01` in the JD index → agent `local-llava`
3. Dispatches to that agent

---

## Adding a new agent

In `mailroom.toml`:

```toml
[[agents]]
name     = "my-agent"
base_url = "http://localhost:8080"
accepts  = ["text", "file"]
jd_prefix = "20"
```

For non-Ollama agents, the mailroom POSTs the full Envelope JSON to
`{base_url}/receive`. Implement that endpoint on the agent side.

---

## Extending

- **New source type**: add a file under `src/ingest/`, register the route in `src/ingest/mod.rs`
- **New agent adapter**: add a file under `src/agents/`, update `send_to_agent()` in `registry.rs`
- **Streaming**: the WebSocket upgrade path is available via `axum::extract::ws`; plug in at a new `/ingest/stream/*` route
