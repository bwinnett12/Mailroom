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
    extract::State,
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
    envelope::{Envelope, InboundEnvelope, Payload},
    inference::InferenceClient,
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
            match state.registry.get(address) {

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
                                match state.registry.get(&address) {
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
						

						// ── Write to disk ─────────────────────────────────────────────
						let library_root = &state.library_root;
						// For now we write relative to vault_root/Library.
						// When Island is online this becomes /storage/Library.
						// We'll make this path configurable via env var next.

						let jd_path = manifest.effective_path();
						// e.g. "34.2_Journal" or "34_My-story/34.2_Journal"
						// The manifest tells us where this node lives on disk.

						match store::store(&envelope, &library_root, &jd_path).await {
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

						let decision = RoutingDecision {
							envelope_id: envelope.id,
							routed_to:   address.clone(),
							node_name:   Some(manifest.name.clone()),
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