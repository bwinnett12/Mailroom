// src/coffer.rs
//
// The Coffer: where Mailroom keeps what it stores. It has an Orchard
// address like everything else (85 by default):
//
//   85_coffer/
//     85-A_overview/
//     85-B_contents/2c/2cf24dba5f…    file bytes, once each, named by SHA-256
//     85-C_ledger/<device>/           this machine's ledger + graph snapshot
//         events.jsonl  ledger.lock  snapshot.bin
//
// Nests hold pointers (.ref files) at the item's real address; the bytes
// live here. Every machine has every pointer (they sync with the vault)
// but only some of the bytes: Locomotive's coffer holds everything, other
// machines a subset. The ledger is per machine and never synced; the
// <device> folder is the name stamped on every record it holds.
//
// Everything below 85-B and 85-C is git-ignored (see `init`): Mailroom
// moves bytes, git never does, and ledgers never leave their machine.
//
// Mailroom finds the coffer by nest id, not by path, so renumbering is a
// config change: MAILROOM_COFFER_CONTENTS (default 85-B) and
// MAILROOM_COFFER_LEDGER (default 85-C). It never creates them on its own;
// a vault without them fails to open with instructions, and
// `mailroom coffer init` creates them when asked.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::manifest::{Manifest, NodeKind};

/// Version of the vault layout. 1: ledger in <vault>/.mailroom, blobs in
/// <root>/by-hash, entries possibly in a separate Library tree. 2: the
/// coffer. Written as `schema_version` on the ledger nest.
pub const LAYOUT_VERSION: u32 = 2;

pub const DEFAULT_CONTENTS_ID: &str = "85-B";
pub const DEFAULT_LEDGER_ID: &str = "85-C";

/// Where this vault's coffer is, on this machine.
#[derive(Debug, Clone)]
pub struct Coffer {
    /// Nest folder holding blobs: <contents>/<aa>/<sha256>.
    pub contents: PathBuf,
    /// This device's ledger folder: <ledger nest>/<device>/.
    pub ledger_dir: PathBuf,
    /// The ledger nest itself (holds one folder per device).
    pub ledger_nest: PathBuf,
}

/// The two nest ids, from the environment or the defaults.
pub fn ids() -> (String, String) {
    let get = |var: &str, default: &str| {
        std::env::var(var)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| default.to_string())
    };
    (
        get("MAILROOM_COFFER_CONTENTS", DEFAULT_CONTENTS_ID),
        get("MAILROOM_COFFER_LEDGER", DEFAULT_LEDGER_ID),
    )
}

/// Find the coffer nests in `vault`. Fails, saying exactly what to create,
/// if either is missing.
pub fn resolve(vault: &Path) -> anyhow::Result<Coffer> {
    let (contents_id, ledger_id) = ids();
    let find = |id: &str| -> anyhow::Result<Option<PathBuf>> {
        Ok(crate::move_branch::find_nest_by_id(vault, id)?
            .and_then(|(nest_file, _)| nest_file.parent().map(Path::to_path_buf)))
    };
    let contents = find(&contents_id)?;
    let ledger_nest = find(&ledger_id)?;
    match (contents, ledger_nest) {
        (Some(contents), Some(ledger_nest)) => Ok(Coffer {
            ledger_dir: ledger_nest.join(crate::ledger::device_name()),
            contents,
            ledger_nest,
        }),
        (c, l) => {
            let missing: Vec<&str> = [(c.is_none(), contents_id.as_str()), (l.is_none(), ledger_id.as_str())]
                .into_iter()
                .filter(|(m, _)| *m)
                .map(|(_, id)| id)
                .collect();
            anyhow::bail!(
                "the vault at {} has no coffer nest {} (where Mailroom keeps file bytes and \
                 its ledger). Create it with `mailroom coffer init`, or point \
                 MAILROOM_COFFER_CONTENTS / MAILROOM_COFFER_LEDGER at nests you already have.",
                vault.display(),
                missing.join(" or "),
            )
        }
    }
}

/// The coffer for this vault, resolved once per process.
pub fn for_vault(vault: &Path) -> anyhow::Result<Arc<Coffer>> {
    static FOUND: OnceLock<Mutex<HashMap<PathBuf, Arc<Coffer>>>> = OnceLock::new();
    let key = std::fs::canonicalize(vault)?;
    let mut found = FOUND
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if let Some(coffer) = found.get(&key) {
        return Ok(coffer.clone());
    }
    let coffer = Arc::new(resolve(&key)?);
    found.insert(key, coffer.clone());
    Ok(coffer)
}

/// The coffer's area (85) and the folder it is, or would be created in.
fn area(vault: &Path) -> anyhow::Result<(String, PathBuf)> {
    let (contents_id, _) = ids();
    let area = contents_id.split('-').next().unwrap_or("85").to_string();
    let dir = match crate::move_branch::find_nest_by_id(vault, &area)? {
        Some((nest_file, _)) => nest_file.parent().unwrap_or(vault).to_path_buf(),
        None => vault.join(format!("{area}_coffer")),
    };
    Ok((area, dir))
}

/// The coffer as it is, or as `init` would create it. For planning a
/// migration before anything is written.
pub fn resolve_or_planned(vault: &Path) -> anyhow::Result<(Coffer, bool)> {
    if let Ok(c) = resolve(vault) {
        return Ok((c, true));
    }
    let (contents_id, ledger_id) = ids();
    let (_, area_dir) = area(vault)?;
    let existing = |id: &str| -> anyhow::Result<Option<PathBuf>> {
        Ok(crate::move_branch::find_nest_by_id(vault, id)?
            .and_then(|(f, _)| f.parent().map(Path::to_path_buf)))
    };
    let contents = existing(&contents_id)?.unwrap_or_else(|| area_dir.join(format!("{contents_id}_contents")));
    let ledger_nest = existing(&ledger_id)?.unwrap_or_else(|| area_dir.join(format!("{ledger_id}_ledger")));
    Ok((
        Coffer { ledger_dir: ledger_nest.join(crate::ledger::device_name()), contents, ledger_nest },
        false,
    ))
}

/// Create the coffer nests that don't exist yet, under `<area>_coffer/` at
/// the top of the vault. Existing nests (found by id anywhere in the vault)
/// are left alone. Returns the nest files written.
pub fn init(vault: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let (contents_id, ledger_id) = ids();
    let (area, area_dir) = area(vault)?;

    let mut written = Vec::new();
    let mut make = |dir: &Path, id: &str, name: &str, kind: NodeKind, about: &str, version: Option<u32>| -> anyhow::Result<()> {
        if crate::move_branch::find_nest_by_id(vault, id)?.is_some() {
            return Ok(());
        }
        std::fs::create_dir_all(dir)?;
        let mut m = Manifest::stub(id, name, kind);
        m.schema_version = version;
        let file = dir.join("nest");
        std::fs::write(&file, format!("# {about}\n{}", toml::to_string_pretty(&m)?))?;
        written.push(file);
        Ok(())
    };

    make(&area_dir, &area, "coffer", NodeKind::Domain,
        "Where Mailroom keeps what it stores: file bytes by hash, and each machine's ledger.", None)?;
    make(&area_dir.join(format!("{area}-A_overview")), &format!("{area}-A"), "overview", NodeKind::Index,
        "What the coffer holds and how. Synced like any nest.", None)?;
    make(&area_dir.join(format!("{contents_id}_contents")), &contents_id, "contents", NodeKind::Archive,
        "File bytes, once each, under <first two hex>/<sha256>. Pointers live in the nests. \
         Partial on every machine except the canonical one. Never tracked by git.", None)?;
    make(&area_dir.join(format!("{ledger_id}_ledger")), &ledger_id, "ledger", NodeKind::Log,
        "One folder per machine: that machine's ledger and graph snapshot. Never synced.",
        Some(LAYOUT_VERSION))?;

    // Git: track the nest files, never the bytes or the ledgers.
    let ignore = area_dir.join(".gitignore");
    if !ignore.exists() {
        std::fs::write(
            &ignore,
            format!(
                "# Written by `mailroom coffer init`. Bytes move through Mailroom, not git;\n\
                 # ledgers never leave their machine.\n\
                 /{c}_contents/*\n!/{c}_contents/nest\n/{l}_ledger/*\n!/{l}_ledger/nest\n",
                c = contents_id,
                l = ledger_id
            ),
        )?;
    }
    Ok(written)
}

/// Pieces of the old (version 1) layout still present in `vault`. A vault
/// with any of these must be migrated before it is opened, or the new
/// ledger would start empty beside the old one and history would fork.
pub fn legacy_layout(vault: &Path) -> Vec<PathBuf> {
    [
        vault.join(".mailroom").join("ledger").join("events.jsonl"),
        vault.join("by-hash"),
    ]
    .into_iter()
    .filter(|p| p.exists())
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vault_without_a_coffer_says_what_to_create() {
        let vault = tempfile::tempdir().unwrap();
        let err = resolve(vault.path()).unwrap_err().to_string();
        assert!(err.contains("85-B or 85-C"), "{err}");
        assert!(err.contains("mailroom coffer init"), "{err}");
    }

    #[test]
    fn init_creates_the_nests_and_resolve_finds_them() {
        let vault = tempfile::tempdir().unwrap();
        let written = init(vault.path()).unwrap();
        assert_eq!(written.len(), 4);
        let coffer = resolve(vault.path()).unwrap();
        assert!(coffer.contents.ends_with("85_coffer/85-B_contents"));
        assert!(coffer.ledger_dir.starts_with(vault.path().join("85_coffer/85-C_ledger")));
        assert_eq!(coffer.ledger_dir.file_name().unwrap().to_string_lossy(), crate::ledger::device_name());

        let ledger_nest: Manifest =
            toml::from_str(&std::fs::read_to_string(coffer.ledger_nest.join("nest")).unwrap()).unwrap();
        assert_eq!(ledger_nest.schema_version, Some(LAYOUT_VERSION));

        let ignore = std::fs::read_to_string(vault.path().join("85_coffer/.gitignore")).unwrap();
        assert!(ignore.contains("/85-B_contents/*") && ignore.contains("!/85-C_ledger/nest"));
    }

    #[test]
    fn init_twice_writes_nothing_new() {
        let vault = tempfile::tempdir().unwrap();
        init(vault.path()).unwrap();
        assert!(init(vault.path()).unwrap().is_empty());
    }

    #[test]
    fn init_uses_an_existing_area_folder() {
        let vault = tempfile::tempdir().unwrap();
        let area = vault.path().join("80_stream/85_Coffer");
        std::fs::create_dir_all(&area).unwrap();
        std::fs::write(area.join("nest"), toml::to_string_pretty(&Manifest::stub("85", "Coffer", NodeKind::Domain)).unwrap()).unwrap();
        let written = init(vault.path()).unwrap();
        assert_eq!(written.len(), 3, "the area nest already existed");
        assert!(resolve(vault.path()).unwrap().contents.starts_with(&area));
    }

    #[test]
    fn the_ledger_refuses_a_vault_still_in_the_old_layout() {
        let vault = tempfile::tempdir().unwrap();
        init(vault.path()).unwrap();
        std::fs::create_dir_all(vault.path().join(".mailroom/ledger")).unwrap();
        std::fs::write(vault.path().join(".mailroom/ledger/events.jsonl"), "").unwrap();
        let err = crate::ledger::for_vault(vault.path()).err().expect("must refuse").to_string();
        assert!(err.contains("old layout") && err.contains("mailroom migrate"), "{err}");
    }

    #[test]
    fn the_registry_knows_each_nests_real_folder() {
        let vault = tempfile::tempdir().unwrap();
        init(vault.path()).unwrap();
        let books = vault.path().join("52_media/52-B_books");
        std::fs::create_dir_all(&books).unwrap();
        std::fs::write(books.join("nest"), "id = \"52-B\"\nname = \"books\"\nkind = \"leaf\"\n").unwrap();
        let registry = crate::registry::Registry::load(vault.path()).unwrap();
        assert_eq!(registry.nest_dir(vault.path(), "52-B"), Some(books));
        assert_eq!(registry.dir_of("85-B"), Some(PathBuf::from("85_coffer/85-B_contents")));
        assert_eq!(registry.dir_of("nope"), None);
    }

    #[test]
    fn old_layout_is_detected() {
        let vault = tempfile::tempdir().unwrap();
        assert!(legacy_layout(vault.path()).is_empty());
        std::fs::create_dir_all(vault.path().join("by-hash/2c")).unwrap();
        assert_eq!(legacy_layout(vault.path()), vec![vault.path().join("by-hash")]);
    }
}
