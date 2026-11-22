// src/migrate.rs
//
// `mailroom migrate`: move a vault from layout 1 to layout 2 (the coffer).
//
//   layout 1                                  layout 2
//   <vault>/.mailroom/ledger/events.jsonl  →  <85-C_ledger>/<device>/events.jsonl
//   <vault>/.mailroom/graph/snapshot.bin   →  (dropped: rebuilt from the ledger)
//   <vault>/by-hash/aa/<sha>               →  <85-B_contents>/aa/<sha>
//   <library>/by-hash/aa/<sha>             →  <85-B_contents>/aa/<sha>
//   <library>/<nest path>/entries/…        →  <the nest's real folder>/entries/…
//   .ref  "ref: by-hash/aa/<sha>"          →  "ref: sha256:<sha>"
//
// Rules:
//   - Plan everything first; `--dry-run` prints the plan and touches nothing.
//   - Files are moved (rename), never copied, so nothing is left in two
//     places. Every blob is re-hashed at its new home before its old copy
//     counts as gone; a blob already in the coffer with the same hash makes
//     the old copy redundant, and it is removed.
//   - Nothing is overwritten. A destination that already exists with
//     different content is a conflict: reported, left where it is.
//   - Refuses to run while a Mailroom process holds the old ledger.
//   - Empty folders left behind are removed; non-empty ones are reported.
//
// <library> is MAILROOM_LIBRARY_ROOT if set, else <vault>/Library, or
// --library <path>. It is optional: a vault that never used one skips it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::coffer::{self, Coffer};

#[derive(Debug, PartialEq)]
pub enum Kind {
    Ledger,
    Blob,
    Entry,
}

#[derive(Debug)]
pub struct Move {
    pub from: PathBuf,
    pub to: PathBuf,
    pub kind: Kind,
}

#[derive(Debug, Default)]
pub struct Plan {
    /// The coffer nests have to be created first.
    pub create_coffer: bool,
    pub coffer: Option<Coffer>,
    pub moves: Vec<Move>,
    /// Old copies made redundant by an identical blob already in the coffer.
    pub redundant: Vec<PathBuf>,
    /// Disposable files to delete (old snapshot, old lock).
    pub drop: Vec<PathBuf>,
    /// .ref pointers in the old format, to rewrite after the moves.
    pub rewrite_refs: usize,
    /// Things left alone, and why.
    pub conflicts: Vec<String>,
    /// Folders to remove if they end up empty.
    pub old_roots: Vec<PathBuf>,
    pub notes: Vec<String>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.moves.is_empty() && self.redundant.is_empty() && self.drop.is_empty() && self.rewrite_refs == 0
    }
}

/// Work out what migrating `vault` would do. Writes nothing.
pub fn plan(vault: &Path, library: Option<&Path>) -> anyhow::Result<Plan> {
    let (c, exists) = coffer::resolve_or_planned(vault)?;
    let mut plan = Plan { create_coffer: !exists, ..Default::default() };

    // ── Ledger ──────────────────────────────────────────────────────────────
    let old_dir = vault.join(".mailroom");
    let old_ledger = old_dir.join("ledger").join("events.jsonl");
    if old_ledger.exists() {
        let new_ledger = c.ledger_dir.join("events.jsonl");
        if new_ledger.exists() && std::fs::metadata(&new_ledger)?.len() > 0 {
            plan.conflicts.push(format!(
                "{} already has records; the old ledger {} was left where it is (two histories \
                 can't be merged by moving files)",
                new_ledger.display(),
                old_ledger.display()
            ));
        } else {
            if let Some(device) = first_device(&old_ledger) {
                let here = crate::ledger::device_name();
                if device != here {
                    plan.notes.push(format!(
                        "the old ledger was written by device '{device}'; this machine is '{here}', \
                         so it moves to the '{here}' folder. Set MAILROOM_DEVICE={device} first if \
                         that's wrong."
                    ));
                }
            }
            plan.moves.push(Move { from: old_ledger, to: new_ledger, kind: Kind::Ledger });
        }
        plan.old_roots.push(old_dir.join("ledger"));
    }
    for disposable in [old_dir.join("ledger").join("ledger.lock"), old_dir.join("graph").join("snapshot.bin")] {
        if disposable.exists() {
            plan.drop.push(disposable);
        }
    }
    if old_dir.join("graph").exists() {
        plan.old_roots.push(old_dir.join("graph"));
    }
    if old_dir.is_dir() {
        plan.old_roots.push(old_dir.clone());
    }

    // ── Blobs ───────────────────────────────────────────────────────────────
    let mut stores = vec![vault.join("by-hash")];
    if let Some(lib) = library {
        stores.push(lib.join("by-hash"));
    }
    let mut planned: HashMap<String, PathBuf> = HashMap::new();
    for store in stores.iter().filter(|s| s.is_dir()) {
        plan.old_roots.push(store.clone());
        for entry in WalkDir::new(store).into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()) {
            let from = entry.path().to_path_buf();
            let name = entry.file_name().to_string_lossy().to_string();
            if !crate::blobs::is_hash(&name) {
                if name.ends_with(".tmp") {
                    plan.drop.push(from); // a write a crash interrupted
                } else {
                    plan.conflicts.push(format!("{} is not a blob (name isn't a SHA-256); left alone", from.display()));
                }
                continue;
            }
            let to = c.contents.join(crate::blobs::rel_path(&name));
            if to.exists() || planned.contains_key(&name) {
                plan.redundant.push(from); // checked against the kept copy when applied
            } else {
                planned.insert(name, from.clone());
                plan.moves.push(Move { from, to, kind: Kind::Blob });
            }
        }
    }

    // ── Library entries ─────────────────────────────────────────────────────
    if let Some(lib) = library.filter(|l| l.is_dir()) {
        plan.old_roots.push(lib.to_path_buf());
        let places = nest_places(vault, lib)?;
        for entry in WalkDir::new(lib)
            .into_iter()
            .filter_entry(|e| e.path() != lib.join("by-hash"))
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let from = entry.path().to_path_buf();
            let rel = from.strip_prefix(lib)?.to_path_buf();
            match place(&rel, &places) {
                Some(dir) => {
                    let to = vault.join(dir);
                    if to.exists() {
                        plan.conflicts.push(format!("{} already exists; {} left where it is", to.display(), from.display()));
                    } else {
                        plan.moves.push(Move { from, to, kind: Kind::Entry });
                    }
                }
                None => plan.conflicts.push(format!(
                    "{}: no nest in the vault matches this path; left where it is",
                    from.display()
                )),
            }
        }
    }

    // ── Pointers ────────────────────────────────────────────────────────────
    let mut roots = vec![vault.to_path_buf()];
    if let Some(lib) = library.filter(|l| l.is_dir() && !l.starts_with(vault)) {
        roots.push(lib.to_path_buf());
    }
    for root in roots {
        for entry in WalkDir::new(&root)
            .into_iter()
            .filter_entry(|e| e.path() != c.contents && !stores.iter().any(|s| s == e.path()))
            .filter_map(|e| e.ok())
        {
            if is_old_ref(entry.path()) {
                plan.rewrite_refs += 1;
            }
        }
    }

    plan.coffer = Some(c);
    Ok(plan)
}

/// What applying a plan did.
#[derive(Debug, Default)]
pub struct Report {
    pub coffer_nests: Vec<PathBuf>,
    pub moved: usize,
    pub blobs_verified: usize,
    pub redundant_removed: usize,
    pub refs_rewritten: usize,
    pub folders_removed: usize,
    pub left_over: Vec<PathBuf>,
}

/// Carry out `plan`. Stops at the first blob whose bytes don't match its
/// name, leaving it (and everything not yet moved) in the old layout.
pub fn apply(vault: &Path, library: Option<&Path>, plan: &Plan) -> anyhow::Result<Report> {
    let mut report = Report::default();

    // Not while something is writing the old ledger.
    let old_lock = vault.join(".mailroom").join("ledger").join("ledger.lock");
    if old_lock.exists() {
        let f = std::fs::OpenOptions::new().write(true).open(&old_lock)?;
        if f.try_lock().is_err() {
            anyhow::bail!("a Mailroom process is using this vault's ledger; stop it, then migrate");
        }
    }

    if plan.create_coffer {
        report.coffer_nests = coffer::init(vault)?;
    }
    let c = coffer::resolve(vault)?;
    let have_version = std::fs::read_to_string(c.ledger_nest.join("nest"))
        .ok()
        .and_then(|t| toml::from_str::<crate::manifest::Manifest>(&t).ok())
        .and_then(|m| m.schema_version);
    if have_version.is_none() {
        // Nests that existed before `init`: record the layout on the ledger nest.
        let file = c.ledger_nest.join("nest");
        let mut m: crate::manifest::Manifest = toml::from_str(&std::fs::read_to_string(&file)?)?;
        m.schema_version = Some(coffer::LAYOUT_VERSION);
        std::fs::write(&file, toml::to_string_pretty(&m)?)?;
    }

    for mv in &plan.moves {
        if let Some(parent) = mv.to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if mv.to.exists() {
            anyhow::bail!("{} appeared since the plan was made; stopping", mv.to.display());
        }
        move_file(&mv.from, &mv.to)?;
        if mv.kind == Kind::Blob {
            let name = mv.to.file_name().unwrap_or_default().to_string_lossy().to_string();
            let actual = crate::blobs::hash_file(&mv.to)?;
            if actual != name {
                anyhow::bail!(
                    "{} does not hash to its name (got {actual}); moved but not trusted, stopping",
                    mv.to.display()
                );
            }
            report.blobs_verified += 1;
        }
        report.moved += 1;
    }

    for old in &plan.redundant {
        let name = old.file_name().unwrap_or_default().to_string_lossy().to_string();
        let kept = c.contents.join(crate::blobs::rel_path(&name));
        if crate::blobs::hash_file(&kept)? == name {
            remove_read_only(old)?;
            report.redundant_removed += 1;
        } else {
            anyhow::bail!("{} in the coffer doesn't match its name; {} kept", kept.display(), old.display());
        }
    }

    for f in &plan.drop {
        let _ = remove_read_only(f);
    }

    // Pointers, wherever they are now (entries just moved included).
    for entry in WalkDir::new(vault)
        .into_iter()
        .filter_entry(|e| e.path() != c.contents)
        .filter_map(|e| e.ok())
    {
        if is_old_ref(entry.path()) {
            let text = std::fs::read_to_string(entry.path())?;
            std::fs::write(entry.path(), new_ref(&text))?;
            report.refs_rewritten += 1;
        }
    }
    if let Some(lib) = library.filter(|l| l.is_dir() && !l.starts_with(vault)) {
        for entry in WalkDir::new(lib).into_iter().filter_map(|e| e.ok()) {
            if is_old_ref(entry.path()) {
                let text = std::fs::read_to_string(entry.path())?;
                std::fs::write(entry.path(), new_ref(&text))?;
                report.refs_rewritten += 1;
            }
        }
    }

    // Empty folders left behind, deepest first.
    for root in &plan.old_roots {
        if !root.exists() {
            continue;
        }
        let mut dirs: Vec<PathBuf> = WalkDir::new(root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_dir())
            .map(|e| e.path().to_path_buf())
            .collect();
        dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
        for d in dirs {
            if std::fs::remove_dir(&d).is_ok() {
                report.folders_removed += 1;
            }
        }
        if root.exists() {
            report.left_over.push(root.clone());
        }
    }
    Ok(report)
}

/// `mailroom migrate [--dry-run] [--library <path>]`
pub fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let library_arg = args
        .iter()
        .position(|a| a == "--library")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    let vault = PathBuf::from(std::env::var("MAILROOM_VAULT").map_err(|_| "set MAILROOM_VAULT to the vault to migrate")?);
    let library = library_arg
        .or_else(|| std::env::var("MAILROOM_LIBRARY_ROOT").ok().map(PathBuf::from))
        .or_else(|| Some(vault.join("Library")))
        .filter(|l| l.is_dir());

    let plan = plan(&vault, library.as_deref())?;
    let c = plan.coffer.as_ref().expect("plan always resolves a coffer");

    println!("vault:    {}", vault.display());
    println!("library:  {}", library.as_ref().map(|l| l.display().to_string()).unwrap_or_else(|| "(none)".into()));
    println!("coffer:   contents {}", c.contents.display());
    println!("          ledger   {}", c.ledger_dir.display());
    if plan.create_coffer {
        println!("          (nests will be created)");
    }
    let count = |k: Kind| plan.moves.iter().filter(|m| m.kind == k).count();
    println!();
    println!("move ledger:        {}", count(Kind::Ledger));
    println!("move blobs:         {}", count(Kind::Blob));
    println!("move entries:       {}", count(Kind::Entry));
    println!("remove duplicates:  {}", plan.redundant.len());
    println!("delete disposable:  {}", plan.drop.len());
    println!("rewrite .ref files: {}", plan.rewrite_refs);
    for n in &plan.notes {
        println!("note: {n}");
    }
    for c in &plan.conflicts {
        println!("left alone: {c}");
    }
    if dry_run {
        for m in &plan.moves {
            println!("  {} -> {}", m.from.display(), m.to.display());
        }
        println!("\n(dry run: nothing changed)");
        return Ok(());
    }
    if plan.is_empty() && !plan.create_coffer {
        println!("\nnothing to migrate");
        return Ok(());
    }

    let r = apply(&vault, library.as_deref(), &plan)?;
    println!();
    for n in &r.coffer_nests {
        println!("created {}", n.display());
    }
    println!(
        "moved {}, verified {} blobs, removed {} duplicates, rewrote {} pointers, removed {} empty folders",
        r.moved, r.blobs_verified, r.redundant_removed, r.refs_rewritten, r.folders_removed
    );
    for d in &r.left_over {
        println!("not empty, left in place: {}", d.display());
    }
    println!("\nnext: start Mailroom and check GET /graph/check");
    Ok(())
}

// ── helpers ─────────────────────────────────────────────────────────────────

/// Where each nest outside the library really lives: effective_path() (how
/// layout 1 named its library folder) → real folder relative to the vault.
fn nest_places(vault: &Path, library: &Path) -> anyhow::Result<Vec<(PathBuf, PathBuf)>> {
    let lib_rel = library.strip_prefix(vault).ok().map(Path::to_path_buf);
    let mut places = Vec::new();
    for entry in WalkDir::new(vault)
        .into_iter()
        .filter_entry(|e| e.path() != library)
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && e.file_name() == "nest")
    {
        let Ok(m) = toml::from_str::<crate::manifest::Manifest>(&std::fs::read_to_string(entry.path())?) else {
            continue;
        };
        let Some(dir) = entry.path().parent().and_then(|d| d.strip_prefix(vault).ok()) else { continue };
        if lib_rel.as_ref().is_some_and(|l| dir.starts_with(l)) {
            continue;
        }
        places.push((PathBuf::from(m.effective_path()), dir.to_path_buf()));
    }
    // Longest prefix wins: a nest inside another nest's path is more specific.
    places.sort_by_key(|(p, _)| std::cmp::Reverse(p.components().count()));
    Ok(places)
}

/// The vault-relative destination for a file at `rel` in the library.
fn place(rel: &Path, places: &[(PathBuf, PathBuf)]) -> Option<PathBuf> {
    places
        .iter()
        .find(|(old, _)| rel.starts_with(old))
        .map(|(old, real)| real.join(rel.strip_prefix(old).expect("checked by starts_with")))
}

fn is_old_ref(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "ref")
        && std::fs::read_to_string(path).is_ok_and(|t| t.starts_with("ref: by-hash/"))
}

/// "ref: by-hash/2c/<sha>\n…" → "ref: sha256:<sha>\n…"
fn new_ref(text: &str) -> String {
    text.lines()
        .map(|line| match line.strip_prefix("ref: by-hash/") {
            Some(rest) => format!("ref: sha256:{}", rest.rsplit('/').next().unwrap_or(rest)),
            None => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn first_device(ledger: &Path) -> Option<String> {
    let text = std::fs::read_to_string(ledger).ok()?;
    let line = text.lines().next()?;
    serde_json::from_str::<serde_json::Value>(line).ok()?["device"].as_str().map(String::from)
}

/// Rename; across filesystems, copy then remove.
fn move_file(from: &Path, to: &Path) -> anyhow::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to)?;
    remove_read_only(from)
}

fn remove_read_only(path: &Path) -> anyhow::Result<()> {
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        if perms.readonly() {
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
    std::fs::remove_file(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const HELLO: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    fn nest(dir: &Path, id: &str, name: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("nest"), format!("id = \"{id}\"\nname = \"{name}\"\nkind = \"leaf\"\n")).unwrap();
    }

    /// A vault in layout 1: a ledger made by the old code path, one blob,
    /// and a pointer to it in a nested nest.
    fn old_vault() -> tempfile::TempDir {
        let vault = tempfile::tempdir().unwrap();
        let v = vault.path();
        nest(&v.join("52_media/52-B_books"), "52-B", "books");
        let led = crate::ledger::Ledger::open(&v.join(".mailroom/ledger")).unwrap();
        for _ in 0..3 {
            led.append(crate::event::Event::new("a", "b", None, None, crate::event::EventKind::Transition, None)).unwrap();
        }
        led.snapshot_now().unwrap();
        drop(led);
        fs::rename(v.join(".mailroom/ledger/snapshot.bin"), v.join(".mailroom/graph_snapshot.tmp")).unwrap();
        fs::create_dir_all(v.join(".mailroom/graph")).unwrap();
        fs::rename(v.join(".mailroom/graph_snapshot.tmp"), v.join(".mailroom/graph/snapshot.bin")).unwrap();
        fs::create_dir_all(v.join("by-hash/2c")).unwrap();
        fs::write(v.join(format!("by-hash/2c/{HELLO}")), "hello").unwrap();
        let entries = v.join("52_media/52-B_books/entries");
        fs::create_dir_all(&entries).unwrap();
        fs::write(entries.join("book.ref"), format!("ref: by-hash/2c/{HELLO}\noriginal: /in/book.txt\n")).unwrap();
        vault
    }

    #[test]
    fn a_dry_run_plans_everything_and_changes_nothing() {
        let vault = old_vault();
        let p = plan(vault.path(), None).unwrap();
        assert!(p.create_coffer);
        assert_eq!(p.moves.iter().filter(|m| m.kind == Kind::Ledger).count(), 1);
        assert_eq!(p.moves.iter().filter(|m| m.kind == Kind::Blob).count(), 1);
        assert_eq!(p.rewrite_refs, 1);
        assert!(p.conflicts.is_empty(), "{:?}", p.conflicts);
        assert!(vault.path().join(".mailroom/ledger/events.jsonl").exists());
        assert!(!vault.path().join("85_coffer").exists(), "planning writes nothing");
    }

    #[test]
    fn migrating_moves_ledger_blobs_and_pointers_and_the_ledger_still_reads() {
        let vault = old_vault();
        let v = vault.path();
        let p = plan(v, None).unwrap();
        let r = apply(v, None, &p).unwrap();

        assert_eq!(r.moved, 2);
        assert_eq!(r.blobs_verified, 1);
        assert_eq!(r.refs_rewritten, 1);
        assert!(r.left_over.is_empty(), "{:?}", r.left_over);
        assert!(crate::coffer::legacy_layout(v).is_empty());
        assert!(!v.join(".mailroom").exists() && !v.join("by-hash").exists());

        let c = crate::coffer::resolve(v).unwrap();
        assert!(c.contents.join(format!("2c/{HELLO}")).exists());
        let pointer = fs::read_to_string(v.join("52_media/52-B_books/entries/book.ref")).unwrap();
        assert_eq!(pointer, format!("ref: sha256:{HELLO}\noriginal: /in/book.txt\n"));

        let led = crate::ledger::Ledger::open(&c.ledger_dir).unwrap();
        assert_eq!(led.records().unwrap().len(), 3, "every record came along");
        assert!(led.verify().problems.is_empty());
        drop(led);

        // Running it again finds nothing to do.
        assert!(plan(v, None).unwrap().is_empty());
    }

    #[test]
    fn library_entries_go_to_the_nests_real_folders() {
        let vault = old_vault();
        let v = vault.path();
        let lib = v.join("Library");
        // Layout 1 wrote <library>/<effective_path>/entries; for 52-B that
        // was "52-B_books", though the nest really lives in 52_media/.
        fs::create_dir_all(lib.join("52-B_books/entries")).unwrap();
        fs::write(lib.join("52-B_books/entries/old.md"), "an old entry").unwrap();
        // A subnest the attendant minted inside the library.
        nest(&lib.join("52-B_books/52-B-abcd_dune"), "52-B-abcd", "dune");
        fs::create_dir_all(lib.join("52-B_books/52-B-abcd_dune/entries")).unwrap();
        fs::write(lib.join("52-B_books/52-B-abcd_dune/entries/dune.md"), "dune").unwrap();
        // A second copy of the same blob in the library's store.
        fs::create_dir_all(lib.join("by-hash/2c")).unwrap();
        fs::write(lib.join(format!("by-hash/2c/{HELLO}")), "hello").unwrap();
        // Something with no nest to go to.
        fs::create_dir_all(lib.join("34_My-story")).unwrap();
        fs::write(lib.join("34_My-story/stray.md"), "?").unwrap();

        let p = plan(v, Some(&lib)).unwrap();
        assert_eq!(p.redundant.len(), 1, "the second copy of the blob");
        assert_eq!(p.conflicts.len(), 1, "{:?}", p.conflicts);
        let r = apply(v, Some(&lib), &p).unwrap();

        let books = v.join("52_media/52-B_books");
        assert_eq!(fs::read_to_string(books.join("entries/old.md")).unwrap(), "an old entry");
        assert!(books.join("52-B-abcd_dune/nest").exists(), "the minted subnest moved under its gate");
        assert!(books.join("52-B-abcd_dune/entries/dune.md").exists());
        assert_eq!(r.redundant_removed, 1);
        assert_eq!(r.left_over, vec![lib.clone()], "the stray file keeps the library folder");
        assert!(lib.join("34_My-story/stray.md").exists());
        assert!(!lib.join("by-hash").exists());
    }

    #[test]
    fn a_ledger_already_in_the_coffer_is_never_overwritten() {
        let vault = old_vault();
        let v = vault.path();
        crate::coffer::init(v).unwrap();
        let c = crate::coffer::resolve(v).unwrap();
        let led = crate::ledger::Ledger::open(&c.ledger_dir).unwrap();
        led.append(crate::event::Event::new("x", "y", None, None, crate::event::EventKind::Transition, None)).unwrap();
        drop(led);

        let p = plan(v, None).unwrap();
        assert!(p.moves.iter().all(|m| m.kind != Kind::Ledger));
        assert!(p.conflicts.iter().any(|c| c.contains("already has records")), "{:?}", p.conflicts);
    }

    #[test]
    fn a_blob_whose_bytes_dont_match_stops_the_migration() {
        let vault = old_vault();
        let v = vault.path();
        fs::write(v.join(format!("by-hash/2c/{HELLO}")), "not hello").unwrap();
        let p = plan(v, None).unwrap();
        let err = apply(v, None, &p).unwrap_err().to_string();
        assert!(err.contains("does not hash to its name"), "{err}");
    }
}
