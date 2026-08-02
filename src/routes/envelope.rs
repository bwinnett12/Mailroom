// src/routes/envelope.rs
//
// Handles incoming envelopes — the core routing loop of the Mailroom.
//
// POST /envelope
//   receives an Envelope as JSON
//   looks up the JD address in the registry
//   if no address → queues for AI classification (stub for now)
//   returns a routing decision
//
// This is the handler that everything else in the system talks to.

use std::sync::Arc;

use axum::{
    extract::{Multipart, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
// Rust concept — selective imports:
// We only import what we use. `extract::State` is Axum's extractor
// for shared state. `Json` handles both request deserialization
// and response serialization. `IntoResponse` is the trait that lets
// us return different types from a handler.

use crate::{
    attendant,
    envelope::{Envelope, InboundEnvelope, Payload, Source},
    inference::InferenceClient,
    manifest::Manifest,
    state::AppState,
    store,
};

// ── Response types ────────────────────────────────────────────────────────────
// What the Mailroom sends back after receiving an envelope.
// Separate from Envelope itself — the response describes what
// the Mailroom *decided*, not what it *received*.

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct RoutingDecision {
    /// The envelope's ID — so the caller can track it.
    pub envelope_id: uuid::Uuid,

    /// The JD address this envelope was routed to.
    /// Either from the envelope itself, or assigned by the registry.
    pub routed_to: String,

    /// The name of the node at that address, from the registry.
    /// None if the address wasn't found in the registry.
    pub node_name: Option<String>,

    /// What the Mailroom will do with this envelope.
    pub action: RoutingAction,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingAction {
    /// Address found in registry, envelope accepted for storage.
    Accepted,

    /// No JD address provided — queued for AI classification.
    /// Will be re-routed once classified.
    PendingClassification,

    /// Address provided but not found in the registry.
    /// Routed to 82.2_Unclassified for review.
    Unclassified,

    /// The node exists but doesn't accept this data type.
    /// Also routed to 82.2_Unclassified.
    TypeMismatch,
}

// ── Handler ───────────────────────────────────────────────────────────────────

/// POST /envelope
///
/// The main entry point for all data entering the Mailroom.
/// Receives an Envelope, makes a routing decision, returns the decision.
///
/// Rust concept — handler signature:
///   Axum reads the argument types left to right and runs each extractor.
///   State(state) extracts Arc<AppState> from the router's state.
///   Json(envelope) deserializes the request body into an Envelope.
///   If deserialization fails, Axum returns 422 before calling this function.
pub async fn receive(
    State(state):   State<Arc<AppState>>,
    Json(inbound):  Json<InboundEnvelope>,  // ← InboundEnvelope, not Envelope
) -> impl IntoResponse {
	let envelope = inbound.into_envelope();
    tracing::info!(
        id          = %envelope.id,
        source      = ?envelope.source,
        data_type   = %envelope.data_type,
        jd_address  = ?envelope.jd_address,
        "envelope received"
    );
    // %  = Display format  (clean, human readable)
    // ?  = Debug format    (more verbose, shows enum variant names)
    // We use ? for source and jd_address because they're enums/Options
    // and Debug gives us more useful log output for those.

    // ── Routing logic ─────────────────────────────────────────────────────────

    match &envelope.jd_address {
        // Rust concept — matching on Option:
        // Option<T> has two variants: Some(T) and None.
        // match forces us to handle both — the compiler rejects
        // code that ignores either case.

        None => {
            // No address provided — needs AI classification.
            // For now we stub this: return PendingClassification.
            // Later: call inference.classify(&envelope) here.
            tracing::info!(
                id = %envelope.id,
                "no JD address — pending classification"
            );

            let decision = RoutingDecision {
                envelope_id: envelope.id,
                routed_to:   "82.2".to_string(),
                node_name:   Some("Unclassified".to_string()),
                action:      RoutingAction::PendingClassification,
            };

            (StatusCode::ACCEPTED, Json(decision))
            // 202 Accepted — received but not yet fully processed.
            // Axum turns this tuple into an HTTP response automatically:
            // (StatusCode, Json<T>) implements IntoResponse.
        }

        Some(address) => {
            // Address provided — look it up in the registry.
            // .cloned() so we're working with an owned Manifest from here
            // on — the read guard is a temporary, dropped at the end of
            // this line, rather than held across the store()/attendant
            // await calls further down.
            // Bound to a `let` deliberately, not chained directly into the
            // match scrutinee below — a temporary RwLockReadGuard created
            // *in* a match scrutinee stays alive for the whole match
            // expression (every arm body), not just the lookup itself.
            // That would hold this read lock through the mints_subnests
            // branch further down, which needs a write lock — same task,
            // waiting on itself, guaranteed deadlock. Binding it here means
            // the guard (a genuine temporary of this statement) drops at
            // the semicolon; `looked_up` itself is a fully owned Option<Manifest>
            // (thanks to .cloned()) with no borrow left to justify holding
            // the lock any longer.
            let looked_up = state.registry.read().await.get(address).cloned();
            match looked_up {

                None => {
                    // No JD address provided — ask LocalAI to classify this envelope.
                    tracing::info!(
                        id        = %envelope.id,
                        data_type = %envelope.data_type,
                        "no JD address — attempting AI classification"
                    );

                    let ai = InferenceClient::new(&state.inference, &state.http_client);
                    // Construct the inference client from AppState's fields.
                    // Borrows both — no cloning, no ownership transfer.

                    match ai.classify(&envelope).await {
                        Ok(address) => {
                            // LocalAI returned a JD address.
                            // Validate it looks plausible before trusting it —
                            // models occasionally return garbage or explanations
                            // instead of a bare address.
                            if is_valid_jd_address(&address) {
                                tracing::info!(
                                    id      = %envelope.id,
                                    address = %address,
                                    "AI classified envelope"
                                );

                                // Re-route with the classified address.
                                // We do this by updating the envelope and checking
                                // the registry — same logic as the Some(address) branch.
                                let looked_up = state.registry.read().await.get(&address).cloned();
                                match looked_up {
                                    Some(manifest) => {
                                        // Write to disk at the classified address.
                                        let mut classified = envelope;
                                        classified.jd_address = Some(address.clone());
                                        // `mut` lets us modify the local binding.
                                        // We update jd_address so the meta sidecar
                                        // records the classified address, not None.

                                        let jd_path = manifest.effective_path();
                                        match store::store(&classified, &state.library_root, &jd_path).await {
                                            Ok(result) => {
                                                tracing::info!(
                                                    content = %result.content_path.display(),
                                                    "classified envelope written to disk"
                                                );
                                            }
                                            Err(e) => {
                                                tracing::error!(error = %e, "failed to write classified envelope");
                                            }
                                        }

                                        let decision = RoutingDecision {
                                            envelope_id: classified.id,
                                            routed_to:   address,
                                            node_name:   Some(manifest.name.clone()),
                                            action:      RoutingAction::Accepted,
                                        };

                                        return (StatusCode::OK, Json(decision));
                                        // `return` exits the function early here —
                                        // we're inside a nested match so we can't
                                        // just fall through to the bottom.
                                    }

                                    None => {
                                        // AI returned an address the registry doesn't know.
                                        // Route to 82.2 for human review.
                                        tracing::warn!(
                                            id      = %envelope.id,
                                            address = %address,
                                            "AI returned unknown JD address — routing to 82.2"
                                        );
                                    }
                                }

                            } else {
                                // AI returned something that doesn't look like a JD address.
                                tracing::warn!(
                                    id       = %envelope.id,
                                    response = %address,
                                    "AI classification returned invalid address — routing to 82.2"
                                );
                            }

                            // Fall through — route to 82.2 if anything above went wrong.
                            let decision = RoutingDecision {
                                envelope_id: envelope.id,
                                routed_to:   "82.2".to_string(),
                                node_name:   Some("Unclassified".to_string()),
                                action:      RoutingAction::Unclassified,
                            };
                            (StatusCode::ACCEPTED, Json(decision))
                        }

                        Err(e) => {
                            // LocalAI unreachable — Island is probably offline.
                            // Route to 82.2, don't crash.
                            tracing::warn!(
                                id    = %envelope.id,
                                error = %e,
                                "AI classification failed (Island offline?) — routing to 82.2"
                            );

                            let decision = RoutingDecision {
                                envelope_id: envelope.id,
                                routed_to:   "82.2".to_string(),
                                node_name:   Some("Unclassified".to_string()),
                                action:      RoutingAction::PendingClassification,
                                // PendingClassification — not Unclassified —
                                // because we know it needs classification,
                                // we just couldn't do it right now.
                            };

                            (StatusCode::ACCEPTED, Json(decision))
                        }
                    }
                }




                Some(manifest) => {
                    // Address found — check if this node accepts the data type.
                    let accepted = manifest.accepts.is_empty()
                    // is_empty() — true if the accepts list has no entries.
                    // An empty accepts list means "accept anything" —
                    // domain nodes often work this way.

                        || manifest.accepts.iter().any(|a| {
                            // iter() borrows the Vec, giving &String references.
                            // any() returns true if the closure returns true
                            // for at least one element.
                            a == "any"
                            || a == &envelope.data_type
                            // &envelope.data_type — we need & to compare
                            // &String (from iter) with String (the field).
                            || envelope.data_type.starts_with(
                                a.trim_end_matches('*')
                                // "text/*" should match "text/journal".
                                // trim_end_matches('*') turns "text/*" → "text/"
                                // then starts_with checks the prefix.
                            )
                        });

                    if accepted {
						tracing::info!(
							id      = %envelope.id,
							address = %address,
							node    = %manifest.name,
							"routed successfully"
						);
						

						// ── Write to disk (or mint a subnest first) ───────────────────
						// For now we write relative to vault_root/Library.
						// When Island is online this becomes /storage/Library.
						// We'll make this path configurable via env var next.

						let mints_subnests = manifest.mailroom.as_ref()
							.is_some_and(|m| m.mints_subnests);

						// The Nest we actually end up storing at — the parent
						// Rookery itself for a normal node, or the freshly
						// minted (or matched, on a dedup hit) child if this
						// Nest is a Rookery. The routing decision below
						// reflects wherever the envelope actually landed,
						// not necessarily where it was originally addressed.
						let landed_at: Manifest = if mints_subnests {
							match attendant::intake(&envelope, &manifest, &state).await {
								Ok(child) => child,
								Err(e) => {
									tracing::error!(
										error = %e,
										id    = %envelope.id,
										address = %address,
										"attendant failed to mint subnest — falling back to parent Nest"
									);
									manifest.clone()
								}
							}
						} else {
							let jd_path = manifest.effective_path();
							// e.g. "34.2_Journal" or "34_My-story/34.2_Journal"
							// The manifest tells us where this node lives on disk.

							match store::store(&envelope, &state.library_root, &jd_path).await {
								Ok(result) => {
									tracing::info!(
										content = %result.content_path.display(),
										meta    = %result.meta_path.display(),
										"written to disk"
									);
								}
								Err(e) => {
									// Log the error but don't fail the request —
									// the routing decision was correct even if the write failed.
									// Later: queue the envelope for retry.
									tracing::error!(
										error = %e,
										id    = %envelope.id,
										"failed to write envelope to disk"
									);
								}
							}
							manifest.clone()
						};

						let decision = RoutingDecision {
							envelope_id: envelope.id,
							routed_to:   landed_at.id.clone(),
							node_name:   Some(landed_at.name.clone()),
							action:      RoutingAction::Accepted,
						};

						(StatusCode::OK, Json(decision))
					} else {
					// Node exists but rejects this data type.
					tracing::warn!(
						id        = %envelope.id,
						address   = %address,
						data_type = %envelope.data_type,
						accepts   = ?manifest.accepts,
						"data type rejected by node — routing to 82.2"
					);

					let decision = RoutingDecision {
						envelope_id: envelope.id,
						routed_to:   "82.2".to_string(),
						node_name:   Some("Unclassified".to_string()),
						action:      RoutingAction::TypeMismatch,
					};

					(StatusCode::OK, Json(decision))
				}
                }
            }
        }
    }
}

/// Returns true if the string looks like a valid JD address.
/// Valid examples: "34", "34.2", "34.2-A", "35.3"
/// Invalid: "I think this belongs in...", "", "maybe 34.2?"
fn is_valid_jd_address(s: &str) -> bool {
    let s = s.trim();
    // trim() removes leading/trailing whitespace.
    // Models often add a newline at the end of their response.

    if s.is_empty() {
        return false;
    }

    // A JD address starts with one or two digits,
    // optionally followed by a dot and more characters.
    // We check the first character is a digit as a quick sanity check.
    // This isn't exhaustive — it's a guard against obvious garbage.
    s.chars().next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    // .chars().next() — get the first character as Option<char>
    // .map(|c| c.is_ascii_digit()) — check if it's 0-9
    // .unwrap_or(false) — empty string returns false
}

/// POST /envelope/upload
///
/// Like POST /envelope, but for real file content sent from a remote
/// device — multipart/form-data instead of JSON.
///
/// Payload::FilePath (used by plain /envelope) is a *reference*: the
/// Mailroom process itself opens whatever path you give it on its own
/// local filesystem. It never transmits bytes over the network at all
/// — if the file only exists on some other device (Loom, say) and
/// Mailroom is running on Locomotive, a FilePath payload naming a
/// Loom-local path simply won't resolve there.
///
/// This route is the actual "a device somewhere else has a real file,
/// get it into the vault" mechanism: it reads the real bytes over the
/// wire and constructs Payload::Bytes, which store() writes as a
/// genuine copy into entries/ — not a pointer — landing it in the
/// store of truth the way you'd want a real intake pipeline to work.
///
/// Expected multipart fields:
///   file        — the actual file content (required)
///   data_type   — e.g. "media/book" (required)
///   jd_address  — e.g. "52-B" (required — no AI-classification
///                 fallback on this path yet, unlike /envelope)
///   title       — optional, becomes meta["title"]
pub async fn upload(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut data_type:  Option<String>  = None;
    let mut jd_address: Option<String>  = None;
    let mut title:      Option<String>  = None;
    let mut filename:   Option<String>  = None;

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                return (StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": format!("multipart error: {e}") }))
                ).into_response();
            }
        };

        match field.name().unwrap_or("").to_string().as_str() {
            "file" => {
                // file_name() reads from the field's Content-Disposition
                // header — grab it before .bytes() consumes the field.
                filename = field.file_name().map(|s| s.to_string());
                match field.bytes().await {
                    Ok(b) => file_bytes = Some(b.to_vec()),
                    Err(e) => return (StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({ "error": format!("failed to read file field: {e}") }))
                    ).into_response(),
                }
            }
            "data_type"  => data_type  = field.text().await.ok(),
            "jd_address" => jd_address = field.text().await.ok(),
            "title"      => title      = field.text().await.ok(),
            _ => { /* ignore unknown fields rather than erroring */ }
        }
    }

    let (Some(bytes), Some(data_type), Some(jd_address)) = (file_bytes, data_type, jd_address) else {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({
            "error": "upload requires 'file', 'data_type', and 'jd_address' fields"
        }))).into_response();
    };

    let mut envelope = Envelope::new(
        Source::Manual,
        data_type,
        Payload::Bytes(bytes),
        Some(jd_address.clone()),
    );
    if let Some(t) = title {
        envelope = envelope.with_meta("title", t);
    }
    if let Some(f) = filename {
        // Same meta key attendant.rs's child_slug() already reads as a
        // title fallback — and now also what store.rs's
        // content_extension() reads to pick the real extension instead
        // of defaulting every Bytes payload to .bin.
        envelope = envelope.with_meta("filename", f);
    }

    // Deliberately duplicated from receive()'s dispatch logic rather
    // than factored into a shared function — that logic has already
    // been proven working end to end this session, and refactoring it
    // to share code with a brand-new handler risks the same drift/patch
    // problems already hit twice today. Worth factoring out once this
    // path is proven too.
    let looked_up = state.registry.read().await.get(&jd_address).cloned();
    let manifest = match looked_up {
        Some(m) => m,
        None => return (StatusCode::BAD_REQUEST, Json(serde_json::json!({
            "error": format!("unknown jd_address: {jd_address}")
        }))).into_response(),
    };

    let mints_subnests = manifest.mailroom.as_ref().is_some_and(|m| m.mints_subnests);

    let landed_at = if mints_subnests {
        match attendant::intake(&envelope, &manifest, &state).await {
            Ok(child) => child,
            Err(e) => {
                tracing::error!(error = %e, id = %envelope.id, "attendant failed to mint subnest — falling back to parent Nest");
                manifest.clone()
            }
        }
    } else {
        let jd_path = manifest.effective_path();
        match store::store(&envelope, &state.library_root, &jd_path).await {
            Ok(result) => {
                tracing::info!(content = %result.content_path.display(), meta = %result.meta_path.display(), "uploaded file written to disk");
            }
            Err(e) => {
                tracing::error!(error = %e, id = %envelope.id, "failed to write uploaded envelope to disk");
            }
        }
        manifest.clone()
    };

    (StatusCode::CREATED, Json(serde_json::json!({
        "envelope_id": envelope.id.to_string(),
        "routed_to":   landed_at.id,
        "node_name":   landed_at.name,
        "action":      "accepted",
    }))).into_response()
}