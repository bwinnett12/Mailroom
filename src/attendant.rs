// src/attendant.rs
//
// The attendant handles envelopes addressed to a Rookery — a Nest whose
// `nest` file sets `mailroom.mints_subnests = true` (e.g. 52-B_books).
// Rather than storing directly, it mints a brand new child Nest beneath
// the Rookery — its own directory, its own `nest` file, its own JD
// address — and stores the envelope there instead.
//
// This is the USPS-attendant metaphor made literal: the Dodo arrives at
// the gate (the Rookery) with an envelope; the attendant either points
// it at an existing child (a dedup hit — the same content already has a
// Nest) or mints a fresh one and sends it there.
//
// Deliberately reuses two things that already existed rather than
// building new mechanism:
//   - orchard::create_orchard(), previously only called at bootstrap
//     time from schema::generate_fresh_tree(), materializes the child's
//     directory + `nest` file exactly the same way here, at request time.
//   - store::compute_content_hash(), previously only used for dedup
//     metadata in EntryMeta, doubles as the addressing scheme itself —
//     no separate classification algorithm needed.

use crate::{
    envelope::Envelope,
    manifest::{Manifest, MailroomConfig, NodeKind, StoreKind},
    nest::Nest,
    orchard::create_station,
    state::AppState,
    store,
};

/// Handle an envelope addressed to a Rookery.
///
/// Returns the Manifest of wherever the envelope actually ended up —
/// either a freshly minted child, or an existing one on a dedup hit —
/// so the caller's routing decision reflects the real destination.
pub async fn intake(
    envelope: &Envelope,
    gate: &Manifest,
    state: &AppState,
) -> anyhow::Result<Manifest> {
    let strategy = gate
        .mailroom
        .as_ref()
        .and_then(|m| m.child_addressing.clone())
        .unwrap_or_else(|| "content_hash".to_string());

    let child_id = match strategy.as_str() {
        "content_hash" => content_hash_child_id(gate, envelope).await?,
        other => {
            anyhow::bail!(
                "Rookery {} requests unknown child_addressing strategy '{other}' \
                 — only 'content_hash' exists today",
                gate.id
            );
        }
    };

    // ── Dedup check — has this content already got a Nest? ─────────────
    // A read lock is enough here; we only escalate to a write lock if
    // we're actually about to mint something new.
    if let Some(existing) = state.registry.read().await.get(&child_id).cloned() {
        // A dedup hit means child_id — derived from this content's own
        // hash — already exists, which by construction means the bytes
        // are byte-for-byte identical to what's already on disk. There's
        // no scenario where this branch runs on genuinely different
        // content. Writing another full copy into entries/ every time
        // the same file gets resent is pure waste with zero new
        // information — an early real test surfaced exactly this: three
        // 11MB copies of the same PDF from three identical uploads.
        tracing::info!(
            id       = %envelope.id,
            child_id = %child_id,
            "dedup hit — identical content already stored, skipping redundant write"
        );
        return Ok(existing);
    }

    // ── Mint a fresh child Nest ──────────────────────────────────────────
    let name = child_slug(envelope, &state.title_cleanup);

    // effective_path()'s fallback ("{id}_{name}") assumes the node lives
    // directly under library_root — true for top-level schema-generated
    // nodes, but not for a book living under 52-B_books. Set `path`
    // explicitly to the full relative path, same convention documented
    // on the field itself ("34_My-story/34.2_Journal"-style), rather
    // than relying on the derived fallback here.
    let folder_name = format!("{child_id}_{name}");
    let full_path = format!("{}/{}", gate.effective_path(), folder_name);

    let child_manifest = Manifest {
        id:   child_id.clone(),
        name: name.clone(),
        path: Some(full_path),
        kind: NodeKind::Leaf,
        accepts: vec![envelope.data_type.clone()],
        store: Some(StoreKind::Overwrite),
        routing: None,
        library: None,
        mailroom: Some(MailroomConfig {
            active: true,
            requires_auth: false,
            notify_on_write: false,
            ai_classify: false,
            mints_subnests: false, // a book doesn't itself mint further subnests
            child_addressing: None,
        }),
        about: None,
        known_tags: Vec::new(),
        call_number: envelope.meta.get("call_number").cloned(),
    };

    // create_orchard() already exists and already does exactly this —
    // previously only ever called at bootstrap from generate_fresh_tree().
    // A one-node slice reuses it unchanged for a single runtime mint.
    // Parent is library_root itself, not the gate's own directory —
    // child_manifest.path is already the full relative path (including
    // the gate's own folder name), so create_station's own
    // parent_path.join(effective_path()) resolves correctly without
    // double-joining the gate's prefix.
    let node = Nest {
        manifest: child_manifest.clone(),
        children: Vec::new(),
    };
    create_station(&node, &state.library_root)?;

    tracing::info!(
        id       = %envelope.id,
        child_id = %child_id,
        path     = %state.library_root.join(child_manifest.effective_path()).display(),
        "minted new subnest"
    );

    // Register it so the *next* lookup (including the store() call right
    // below, which goes through the same registry-driven path lookup
    // logic other Nests use) can find it without a server restart.
    state.registry.write().await.insert(child_id.clone(), child_manifest.clone());

    store::store(envelope, &state.library_root, &child_manifest.effective_path()).await?;

    Ok(child_manifest)
}

/// content_hash addressing: `{gate_id}-{first 8 hex chars of the sha256}`.
/// Same truncation precedent as store.rs's own filename short-ids —
/// not a new convention, just applied to addressing instead of filenames.
async fn content_hash_child_id(gate: &Manifest, envelope: &Envelope) -> anyhow::Result<String> {
    let hash = store::compute_content_hash(&envelope.payload)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "content_hash addressing needs hashable content — this envelope's \
                 payload (e.g. a bare Url) has nothing local to hash"
            )
        })?;

    Ok(format!("{}-{}", gate.id, &hash[..8]))
}

/// A short human-readable slug for the child's folder name — best-effort
/// only. Falls back to the envelope id if nothing better is available;
/// nothing downstream depends on this being meaningful, just readable.
fn child_slug(envelope: &Envelope, cleanup_phrases: &[String]) -> String {
    let raw = envelope
        .meta
        .get("title")
        .or_else(|| envelope.meta.get("filename"))
        .cloned()
        .unwrap_or_else(|| envelope.id.to_string());
    let cleaned = crate::cleanup::clean(&raw, cleanup_phrases);
    slugify(&cleaned)
}

/// Filesystem-safe slug: whitespace (and commas) collapse to a single
/// '-', so "Test Book One" becomes "Test-Book-One" — matching the
/// hyphen-within/underscore-between convention already used elsewhere
/// in the JD tree (e.g. "21-D-A_Recurring-expenses"). Characters that
/// are outright invalid on at least one common filesystem (NTFS/exFAT)
/// are dropped rather than substituted, so a title like "Frankenstein:
/// Or, The Modern Prometheus" becomes "Frankenstein-Or-The-Modern-Prometheus"
/// rather than keeping a bare colon that'd break on Windows/exFAT and
/// look wrong on macOS.
///
/// Also relied on by move_branch.rs, to keep a renamed/reclassified
/// node's folder name using the same convention as a freshly minted one.
pub(crate) fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_sep = false;
    for c in s.trim().chars() {
        if c.is_whitespace() || c == ',' {
            if !last_was_sep && !out.is_empty() {
                out.push('-');
                last_was_sep = true;
            }
        } else if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            continue; // drop outright rather than substitute
        } else {
            out.push(c);
            last_was_sep = false;
        }
    }
    out.trim_end_matches('-').to_string()
}



// ── slugify tests — append to the end of src/attendant.rs ─────────────────
#[cfg(test)]
mod slugify_tests {
    use super::*;
 
    #[test]
    fn spaces_become_hyphens() {
        assert_eq!(slugify("Test Book One"), "Test-Book-One");
    }
 
    #[test]
    fn colons_and_commas_are_stripped_not_kept() {
        // The exact case that motivated this function in the first
        // place — a real title with punctuation that's invalid on
        // NTFS/exFAT (colon) and should collapse cleanly (comma).
        assert_eq!(
            slugify("Frankenstein: Or, The Modern Prometheus"),
            "Frankenstein-Or-The-Modern-Prometheus"
        );
    }
 
    #[test]
    fn repeated_whitespace_collapses_to_one_hyphen() {
        assert_eq!(slugify("Too   Many    Spaces"), "Too-Many-Spaces");
    }
 
    #[test]
    fn trailing_punctuation_does_not_leave_a_dangling_hyphen() {
        assert_eq!(slugify("Trailing Comma,"), "Trailing-Comma");
    }
 
    #[test]
    fn empty_input_does_not_panic() {
        assert_eq!(slugify(""), "");
    }
}
