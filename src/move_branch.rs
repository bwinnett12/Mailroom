// src/move_branch.rs
//
// Safe JD renumbering: rename a Nest's id, move its physical directory
// to match, rewrite any other nest file's [routing] rules that pointed
// at the old id, and flag — never silently touch — any address that's
// hardcoded as a literal fallback in Rust source rather than config.
//
// This is the general mechanism the classification workflow depends on:
// a book mints under 52-B-{hash} immediately and unconditionally
// (attendant.rs), then once something assigns it a real call number,
// move_branch re-addresses it to that call number without breaking
// anything else that might reference the old hash-based id.
//
// Deliberately an offline, direct-filesystem operation today — same
// class as schema-refresh and the add-nest CLI tool, not a live HTTP
// endpoint. Changes made here need a Mailroom restart to be picked up
// by the running Registry, same tradeoff already accepted elsewhere.

use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::manifest::Manifest;
use crate::attendant::slugify;

/// What to do if the destination id already has its own Nest — this is
/// a genuinely different situation from a plain move, not an error
/// case to reject outright.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MergeStrategy {
    /// Destination's own nest file wins entirely. Source's manifest
    /// directives (accepts, routing, known_tags, etc.) are discarded —
    /// only its physical content (entries/, any further children) gets
    /// moved in underneath the destination's existing manifest.
    KeepDestination,
    /// Source's nest file replaces destination's entirely (id/path
    /// updated to the destination's actual location either way).
    TakeSource,
    /// Don't resolve anything automatically. Report every field that
    /// differs between the two manifests and stop — nothing is moved
    /// or written until you rerun with an explicit KeepDestination/
    /// TakeSource choice, or hand-edit one of the nest files yourself.
    /// This is the safe default when a destination already exists.
    Compare,
}

/// Addresses baked into Rust source as literal string fallbacks — see
/// routes/journal.rs ("39.2-3C") and routes/envelope.rs ("82.2"). These
/// can't be rewritten by this tool; renaming one of these ids also
/// requires editing the relevant .rs file and rebuilding. Kept as a
/// small, manually-maintained list rather than trying to scan Rust
/// source for string literals — add to this list whenever a new
/// hardcoded fallback shows up elsewhere in the codebase.
const HARDCODED_IN_SOURCE: &[(&str, &str)] = &[
    ("39.2-3C", "routes/journal.rs — default text/journal fallback address"),
    ("82.2",    "routes/envelope.rs — Unclassified fallback address"),
];

#[derive(Debug)]
pub struct MoveBranchReport {
    pub from_id: String,
    pub to_id: String,
    pub old_path: PathBuf,
    pub new_path: PathBuf,
    /// (nest file path, data_type key) for every [routing] rule rewired.
    pub rewired_routing_rules: Vec<(String, String)>,
    pub hardcoded_warnings: Vec<String>,
    /// Populated only when the destination already existed and
    /// MergeStrategy::Compare was used (or implicitly applied).
    pub manifest_diffs: Vec<String>,
    pub merged_into_existing: bool,
    pub dry_run: bool,
}

/// Rename `from_id` to `to_id` across the whole vault.
///
/// Steps:
///   1. Find from_id's own nest file and directory.
///   2. Compute the new directory path (same parent, new id + slugified name).
///   3. If dry_run, stop here and report what *would* happen — nothing
///      is touched on disk.
///   4. Physically move the directory (fs::rename — same filesystem,
///      so this is a fast metadata-only move, not a copy).
///   5. Rewrite the moved nest file's own id and path fields.
///   6. Walk the whole vault again, rewriting any other nest file's
///      [routing] rules that pointed at from_id.
///   7. Check from_id against HARDCODED_IN_SOURCE and warn loudly if
///      it matches — this tool cannot fix Rust source for you.
pub fn move_branch(
    vault_root: &Path,
    from_id: &str,
    to_id: &str,
    merge_strategy: MergeStrategy,
    dry_run: bool,
) -> anyhow::Result<MoveBranchReport> {
    // ── 1. Locate from_id ────────────────────────────────────────────
    let (old_nest_path, mut manifest) = find_nest_by_id(vault_root, from_id)?
        .ok_or_else(|| anyhow::anyhow!(
            "no nest file found with id '{from_id}' under {}", vault_root.display()
        ))?;

    let old_dir = old_nest_path.parent()
        .ok_or_else(|| anyhow::anyhow!("nest file has no parent directory: {}", old_nest_path.display()))?
        .to_path_buf();

    let parent_dir = old_dir.parent()
        .ok_or_else(|| anyhow::anyhow!("cannot determine parent of {}", old_dir.display()))?
        .to_path_buf();

    // ── 2. Determine the destination ─────────────────────────────────
    // Actually search for a Nest whose id is `to_id`, rather than
    // guessing a path built from the *source's* name — a real
    // pre-existing destination almost always has a different name than
    // whatever's being merged into it, so a guessed path would silently
    // miss it and fall through to a plain move instead of a merge.
    let existing = find_nest_by_id(vault_root, to_id)?;

    let (new_dir, destination_exists, existing_manifest) = match existing {
        Some((existing_nest_path, existing_manifest)) => {
            let dir = existing_nest_path.parent()
                .ok_or_else(|| anyhow::anyhow!(
                    "nest file has no parent directory: {}", existing_nest_path.display()
                ))?
                .to_path_buf();
            (dir, true, Some(existing_manifest))
        }
        None => {
            // Nothing with this id exists yet — compute a fresh path,
            // same slug rules a freshly minted subnest already gets
            // (attendant::slugify), so a call-number id with spaces or
            // punctuation still produces a sane folder name.
            let new_folder_name = format!("{}_{}", to_id, slugify(&manifest.name));
            (parent_dir.join(&new_folder_name), false, None)
        }
    };

    let mut report = MoveBranchReport {
        from_id: from_id.to_string(),
        to_id: to_id.to_string(),
        old_path: old_dir.clone(),
        new_path: new_dir.clone(),
        rewired_routing_rules: Vec::new(),
        hardcoded_warnings: Vec::new(),
        manifest_diffs: Vec::new(),
        merged_into_existing: false,
        dry_run,
    };

    // ── 7. Hardcoded-address check — a warning, not an action, so this
    // runs regardless of dry_run. ─────────────────────────────────────
    for (addr, location) in HARDCODED_IN_SOURCE {
        if *addr == from_id {
            report.hardcoded_warnings.push(format!(
                "'{from_id}' is hardcoded as a literal fallback in {location} — \
                 renaming it here does NOT update that Rust source. You must \
                 edit it and rebuild, or that one code path will keep working \
                 as if this rename never happened."
            ));
        }
    }

    if destination_exists {
        let parsed = existing_manifest.as_ref()
            .expect("destination_exists implies existing_manifest is Some");
        report.manifest_diffs = diff_manifests(&manifest, parsed);
        report.merged_into_existing = true;

        if merge_strategy == MergeStrategy::Compare {
            tracing::info!(
                from_id, to_id,
                diffs = report.manifest_diffs.len(),
                "move_branch: destination already exists — Compare mode, \
                 nothing written, rerun with an explicit strategy"
            );
            return Ok(report); // diagnostic only — no writes, regardless of dry_run
        }
    }

    if dry_run {
        tracing::info!(
            from_id, to_id,
            old_dir = %old_dir.display(),
            new_dir = %new_dir.display(),
            destination_exists,
            "move_branch dry run — no changes made"
        );
        return Ok(report);
    }

    if destination_exists {
        // ── Merge path ────────────────────────────────────────────────
        merge_directory_contents(&old_dir, &new_dir)?;

        if merge_strategy == MergeStrategy::TakeSource {
            manifest.id = to_id.to_string();
            manifest.path = relative_manifest_path(&new_dir, vault_root)?;
            let toml_text = toml::to_string_pretty(&manifest)?;
            std::fs::write(new_dir.join("nest"), toml_text)?;
        }
        // KeepDestination: destination's nest file is left exactly as
        // it was — only the physical content got merged in above.
        let _ = existing_manifest; // read for the diff above; not written back here

        let _ = std::fs::remove_file(&old_nest_path);
        std::fs::remove_dir(&old_dir).ok(); // best-effort; non-empty = leave it, don't lose data

        tracing::info!(from_id, to_id, strategy = ?merge_strategy, "move_branch merged into existing destination");
        return Ok(report);
    }

    // ── Plain move path — no destination conflict ─────────────────────
    // ── 4. Physical move ──────────────────────────────────────────────
    std::fs::rename(&old_dir, &new_dir)?;
    tracing::info!(from = %old_dir.display(), to = %new_dir.display(), "moved directory");

    // ── 5. Rewrite the moved nest file's own id/path ──────────────────
    manifest.id = to_id.to_string();
    manifest.path = relative_manifest_path(&new_dir, vault_root)?;

    let moved_nest_path = new_dir.join("nest");
    let toml_text = toml::to_string_pretty(&manifest)?;
    std::fs::write(&moved_nest_path, toml_text)?;

    // ── 6. Rewire [routing] rules elsewhere in the vault ──────────────
    for entry in WalkDir::new(vault_root).follow_links(true).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.file_name().and_then(|n| n.to_str()) != Some("nest") {
            continue;
        }
        if path == moved_nest_path {
            continue; // already handled above
        }

        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let mut other: Manifest = match toml::from_str(&text) {
            Ok(m) => m,
            Err(_) => continue, // same tolerance as Registry::load — skip, don't abort
        };

        let mut changed = false;
        if let Some(routing) = other.routing.as_mut() {
            for (data_type, address) in routing.rules.iter_mut() {
                if address == from_id {
                    *address = to_id.to_string();
                    changed = true;
                    report.rewired_routing_rules.push((path.display().to_string(), data_type.clone()));
                }
            }
        }

        if changed {
            let updated_toml = toml::to_string_pretty(&other)?;
            std::fs::write(path, updated_toml)?;
            tracing::info!(nest = %path.display(), "rewired [routing] rule(s) pointing at old id");
        }
    }

    tracing::info!(
        from_id, to_id,
        rewired = report.rewired_routing_rules.len(),
        "move_branch complete"
    );

    Ok(report)
}

/// Move everything inside `from_dir` into `to_dir`, except `from_dir`'s
/// own `nest` file (the caller decides what happens to the destination's
/// manifest separately, per MergeStrategy). For a top-level entry that
/// doesn't already exist under `to_dir`, this is a plain rename (cheap,
/// same-filesystem). For one that already exists — e.g. both nests
/// independently have an `entries/` folder — we recurse and merge
/// file-by-file instead of overwriting, since a straight rename onto an
/// existing path would just fail.
fn merge_directory_contents(from_dir: &Path, to_dir: &Path) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(from_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "nest" {
            continue;
        }

        let from_path = entry.path();
        let to_path = to_dir.join(&name);

        if to_path.exists() {
            if from_path.is_dir() && to_path.is_dir() {
                merge_directory_contents(&from_path, &to_path)?;
                let _ = std::fs::remove_dir(&from_path); // now-empty, best-effort
            } else {
                // A genuine filename collision between two non-directory
                // entries — extremely unlikely given entries/ filenames
                // already embed a timestamp + short id, but refuse rather
                // than silently overwrite if it ever happens.
                anyhow::bail!(
                    "merge collision: {} already exists at destination and isn't \
                     a directory to merge into — resolve by hand before retrying",
                    to_path.display()
                );
            }
        } else {
            std::fs::rename(&from_path, &to_path)?;
        }
    }
    Ok(())
}

/// Field-by-field comparison between two manifests, formatted for a
/// human to read — not an automatic resolution. Uses Debug formatting
/// for comparison rather than requiring PartialEq on every nested
/// config type.
fn diff_manifests(source: &Manifest, destination: &Manifest) -> Vec<String> {
    let mut diffs = Vec::new();
    macro_rules! check {
        ($field:ident, $label:literal) => {
            if format!("{:?}", source.$field) != format!("{:?}", destination.$field) {
                diffs.push(format!(
                    "{}: source = {:?}  |  destination = {:?}",
                    $label, source.$field, destination.$field
                ));
            }
        };
    }
    check!(name, "name");
    check!(kind, "kind");
    check!(accepts, "accepts");
    check!(store, "store");
    check!(routing, "routing");
    check!(library, "library");
    check!(known_tags, "known_tags");
    check!(call_number, "call_number");
    diffs
}

/// The new manifest `path` field, relative to vault_root, matching the
/// convention documented on Manifest::path itself.
fn relative_manifest_path(new_dir: &Path, vault_root: &Path) -> anyhow::Result<Option<String>> {
    let rel = new_dir.strip_prefix(vault_root)
        .map_err(|_| anyhow::anyhow!("{} is not under vault_root {}", new_dir.display(), vault_root.display()))?;
    Ok(Some(rel.to_string_lossy().replace('\\', "/")))
}

/// Walk the vault looking for the nest file whose parsed id matches.
/// Same tolerance as Registry::load — a bad TOML file elsewhere in the
/// vault is skipped, not fatal to the search.
fn find_nest_by_id(vault_root: &Path, id: &str) -> anyhow::Result<Option<(PathBuf, Manifest)>> {
    for entry in WalkDir::new(vault_root).follow_links(true).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.file_name().and_then(|n| n.to_str()) != Some("nest") {
            continue;
        }
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let manifest: Manifest = match toml::from_str(&text) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if manifest.id == id {
            return Ok(Some((path.to_path_buf(), manifest)));
        }
    }
    Ok(None)
}
// ── tests ────────────────────────────────────────────────────────────────
// Append this whole block to the end of src/move_branch.rs.
//
// These exercise the paths that have never actually been run, even by
// hand: the merge strategies (only the no-conflict plain move has been
// tested against a real vault so far), the hardcoded-address warning,
// and routing-rule rewiring. Each test builds its own scratch vault in
// a tempdir, so nothing here touches your real Orchard.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{MailroomConfig, NodeKind, StoreKind};
    use std::fs;

    fn test_manifest(id: &str, name: &str) -> Manifest {
        Manifest {
            id: id.to_string(),
            name: name.to_string(),
            path: None,
            kind: NodeKind::Leaf,
            accepts: vec!["media/book".to_string()],
            store: Some(StoreKind::Overwrite),
            routing: None,
            library: None,
            mailroom: Some(MailroomConfig {
                active: true,
                requires_auth: false,
                notify_on_write: false,
                ai_classify: false,
                mints_subnests: false,
                child_addressing: None,
            }),
            about: None,
            known_tags: Vec::new(),
            call_number: None,
        }
    }

    /// Write a nest file (and an entries/ dir with one dummy file, so
    /// tests can confirm content actually moved, not just the manifest)
    /// at `vault_root/{folder_name}`. Returns that directory's path.
    fn seed_nest(vault_root: &Path, folder_name: &str, manifest: &Manifest) -> PathBuf {
        let dir = vault_root.join(folder_name);
        let entries = dir.join("entries");
        fs::create_dir_all(&entries).unwrap();
        // Unique per seeded nest, mirroring how real entries/ filenames
        // always embed a timestamp + short id. Using the same static
        // "dummy.txt" for every nest was a test-fixture bug, not a
        // move_branch bug — it made any merge between two independently
        // seeded nests collide every time, which isn't realistic and
        // isn't the case merge_directory_contents's collision-refusal
        // is meant to guard against.
        fs::write(entries.join(format!("{folder_name}.txt")), b"test content").unwrap();
        fs::write(dir.join("nest"), toml::to_string_pretty(manifest).unwrap()).unwrap();
        dir
    }

    #[test]
    fn plain_move_relocates_directory_and_updates_manifest() {
        let vault = tempfile::tempdir().unwrap();
        let manifest = test_manifest("52-B-aaaa1111", "Old Name");
        seed_nest(vault.path(), "52-B-aaaa1111_Old-Name", &manifest);

        let report = move_branch(
            vault.path(), "52-B-aaaa1111", "663.44",
            MergeStrategy::Compare, false,
        ).unwrap();

        assert!(!report.merged_into_existing, "no destination existed — shouldn't report a merge");
        assert!(report.new_path.exists(), "new directory should exist");
        assert!(!vault.path().join("52-B-aaaa1111_Old-Name").exists(), "old directory should be gone");

        let moved_manifest: Manifest = toml::from_str(
            &fs::read_to_string(report.new_path.join("nest")).unwrap()
        ).unwrap();
        assert_eq!(moved_manifest.id, "663.44");
        assert!(moved_manifest.path.unwrap().ends_with("663.44_Old-Name"));

        // content actually moved, not just the manifest
        assert!(report.new_path.join("entries/52-B-aaaa1111_Old-Name.txt").exists());
    }

    #[test]
    fn dry_run_touches_nothing() {
        let vault = tempfile::tempdir().unwrap();
        let manifest = test_manifest("52-B-bbbb2222", "Untouched");
        let old_dir = seed_nest(vault.path(), "52-B-bbbb2222_Untouched", &manifest);

        let report = move_branch(
            vault.path(), "52-B-bbbb2222", "663.45",
            MergeStrategy::Compare, true, // dry_run
        ).unwrap();

        assert!(report.dry_run);
        assert!(old_dir.exists(), "dry run must not move anything");
        assert!(!report.new_path.exists(), "dry run must not create the destination either");
    }

    #[test]
    fn compare_mode_writes_nothing_when_destination_exists() {
        let vault = tempfile::tempdir().unwrap();
        let source = test_manifest("52-B-cccc3333", "Source Book");
        let mut dest = test_manifest("663.46", "Destination Book");
        dest.known_tags = vec!["already-classified".to_string()];

        let old_dir = seed_nest(vault.path(), "52-B-cccc3333_Source-Book", &source);
        let new_dir = seed_nest(vault.path(), "663.46_Destination-Book", &dest);

        let report = move_branch(
            vault.path(), "52-B-cccc3333", "663.46",
            MergeStrategy::Compare, false,
        ).unwrap();

        assert!(report.merged_into_existing);
        assert!(!report.manifest_diffs.is_empty(), "name/known_tags genuinely differ — should be reported");
        // Compare must write nothing — both original directories untouched.
        assert!(old_dir.exists(), "Compare mode must not move source content");
        assert!(new_dir.exists());
        let dest_manifest: Manifest = toml::from_str(
            &fs::read_to_string(new_dir.join("nest")).unwrap()
        ).unwrap();
        assert_eq!(dest_manifest.name, "Destination Book", "Compare mode must not alter destination");
    }

    #[test]
    fn take_source_overwrites_destination_manifest_but_merges_content() {
        let vault = tempfile::tempdir().unwrap();
        let source = test_manifest("52-B-dddd4444", "Source Book");
        let dest = test_manifest("663.47", "Old Destination Name");

        seed_nest(vault.path(), "52-B-dddd4444_Source-Book", &source);
        let new_dir = seed_nest(vault.path(), "663.47_Old-Destination-Name", &dest);

        let report = move_branch(
            vault.path(), "52-B-dddd4444", "663.47",
            MergeStrategy::TakeSource, false,
        ).unwrap();

        assert!(report.merged_into_existing);
        assert!(!vault.path().join("52-B-dddd4444_Source-Book").exists(), "source dir should be cleaned up");

        let dest_manifest: Manifest = toml::from_str(
            &fs::read_to_string(new_dir.join("nest")).unwrap()
        ).unwrap();
        assert_eq!(dest_manifest.name, "Source Book", "TakeSource should replace destination's manifest");
        assert_eq!(dest_manifest.id, "663.47", "id must reflect the destination address, not the source's");
    }

    #[test]
    fn keep_destination_preserves_destination_manifest() {
        let vault = tempfile::tempdir().unwrap();
        let source = test_manifest("52-B-eeee5555", "Source Book");
        let dest = test_manifest("663.48", "Kept Destination");

        seed_nest(vault.path(), "52-B-eeee5555_Source-Book", &source);
        let new_dir = seed_nest(vault.path(), "663.48_Kept-Destination", &dest);

        move_branch(
            vault.path(), "52-B-eeee5555", "663.48",
            MergeStrategy::KeepDestination, false,
        ).unwrap();

        let dest_manifest: Manifest = toml::from_str(
            &fs::read_to_string(new_dir.join("nest")).unwrap()
        ).unwrap();
        assert_eq!(dest_manifest.name, "Kept Destination", "KeepDestination must leave the manifest untouched");
    }

    #[test]
    fn hardcoded_address_triggers_a_warning_not_an_error() {
        let vault = tempfile::tempdir().unwrap();
        let manifest = test_manifest("82.2", "Unclassified");
        seed_nest(vault.path(), "82.2_Unclassified", &manifest);

        let report = move_branch(
            vault.path(), "82.2", "82.9",
            MergeStrategy::Compare, false,
        ).unwrap();

        assert!(
            !report.hardcoded_warnings.is_empty(),
            "renaming a known hardcoded fallback address must produce a warning"
        );
    }

    #[test]
    fn routing_rules_pointing_at_the_old_id_get_rewired() {
        let vault = tempfile::tempdir().unwrap();
        let target = test_manifest("52-B-ffff6666", "Target");
        seed_nest(vault.path(), "52-B-ffff6666_Target", &target);

        // A separate Nest whose [routing] table points at the id we're
        // about to rename — this is exactly what /journal's route_for()
        // depends on resolving correctly after a rename.
        let mut router = test_manifest("34", "Router Nest");
        let mut rules = std::collections::HashMap::new();
        rules.insert("text/journal".to_string(), "52-B-ffff6666".to_string());
        router.routing = Some(crate::manifest::RoutingConfig { rules });
        seed_nest(vault.path(), "34_Router-Nest", &router);

        let report = move_branch(
            vault.path(), "52-B-ffff6666", "663.49",
            MergeStrategy::Compare, false,
        ).unwrap();

        assert_eq!(report.rewired_routing_rules.len(), 1);

        let router_manifest: Manifest = toml::from_str(
            &fs::read_to_string(vault.path().join("34_Router-Nest/nest")).unwrap()
        ).unwrap();
        assert_eq!(
            router_manifest.routing.unwrap().rules.get("text/journal").unwrap(),
            "663.49",
            "the [routing] rule should now point at the new id"
        );
    }
}