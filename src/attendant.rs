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
    let (child_manifest, _dir) = {
        let mut registry = state.registry.write().await;
        mint_child(
            &state.vault_root,
            &mut registry,
            gate,
            &child_id,
            &name,
            &envelope.data_type,
            envelope.meta.get("call_number").cloned(),
        )?
    };

    // Through a route, so the delivery is in the ledger and the graph.
    crate::route::store_ledgered(&state.vault_root, envelope, &child_id).await?;

    Ok(child_manifest)
}

/// Mint the child nest `child_id` inside `gate`'s real folder, or return it
/// if the registry already has it. The child is a leaf that accepts
/// `data_type`; its folder is `<gate folder>/<child_id>_<name>`. Registers
/// it (with its folder) so the next lookup finds it without a restart.
/// Shared by the attendant (intake over HTTP) and adopt (files dropped
/// into a Rookery). Returns the manifest and its folder relative to the
/// vault.
pub fn mint_child(
    vault_root: &std::path::Path,
    registry: &mut crate::registry::Registry,
    gate: &Manifest,
    child_id: &str,
    name: &str,
    data_type: &str,
    call_number: Option<String>,
) -> anyhow::Result<(Manifest, std::path::PathBuf)> {
    if let (Some(existing), Some(dir)) = (registry.get(child_id).cloned(), registry.dir_of(child_id)) {
        return Ok((existing, dir));
    }

    // The child goes inside the gate's real folder (a book under
    // 52_media/52-B_books, not a stray 52-B_books/ at the vault root).
    // `path` records the full path relative to the vault, the convention
    // documented on the field itself, so effective_path() is right too.
    let gate_dir = registry
        .dir_of(&gate.id)
        .unwrap_or_else(|| std::path::PathBuf::from(gate.effective_path()));
    let child_rel = gate_dir.join(format!("{child_id}_{name}"));
    let full_path = child_rel.to_string_lossy().replace('\\', "/");

    let mut child_manifest = Manifest::stub(child_id, name, NodeKind::Leaf);
    child_manifest.path = Some(full_path);
    child_manifest.accepts = vec![data_type.to_string()];
    child_manifest.store = Some(StoreKind::Overwrite);
    child_manifest.mailroom = Some(MailroomConfig {
        active: true,
        requires_auth: false,
        notify_on_write: false,
        ai_classify: false,
        mints_subnests: false, // a book doesn't itself mint further subnests
        child_addressing: None,
    });
    child_manifest.call_number = call_number;

    // create_station() is what generate_fresh_tree() uses at bootstrap; a
    // one-node slice reuses it for a single runtime mint. Its parent is the
    // vault root, because `path` is already the full relative path.
    let node = Nest {
        manifest: child_manifest.clone(),
        children: Vec::new(),
    };
    create_station(&node, vault_root)?;

    tracing::info!(
        child_id = %child_id,
        path     = %vault_root.join(&child_rel).display(),
        "minted new subnest"
    );

    registry.insert_at(child_id.to_string(), child_manifest.clone(), child_rel.clone());
    Ok((child_manifest, child_rel))
}

/// content_hash addressing: `{gate_id}-{first 16 hex chars of the sha256}`.
/// 16 hex chars = 64 bits. With 8 (32 bits), the chance of two items in
/// one gate colliding passes 1% around 9,300 items and 50% around 77,000;
/// with 16 it stays below one in a million past 6 million items. Ids
/// already issued with 8 chars stay valid: an id is read from its nest
/// file, never re-derived.
pub fn child_id_for_hash(gate: &Manifest, hash: &str) -> String {
    format!("{}-{}", gate.id, &hash[..16])
}

async fn content_hash_child_id(gate: &Manifest, envelope: &Envelope) -> anyhow::Result<String> {
    let hash = store::compute_content_hash(&envelope.payload)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "content_hash addressing needs hashable content — this envelope's \
                 payload (e.g. a bare Url) has nothing local to hash"
            )
        })?;

    Ok(child_id_for_hash(gate, &hash))
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
