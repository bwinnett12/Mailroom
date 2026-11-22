// src/adopt.rs
//
// Adopt: take in files dropped into a nest's folder.
//
// A nest that says `intake = "media/book"` accepts files dropped directly
// into its folder. Adopting one:
//
//   1. hashes it,
//   2. moves its bytes into the coffer (85-B_contents/<aa>/<sha256>): a
//      rename on the same disk, so a 40 GB file costs nothing extra,
//   3. in a Rookery (mailroom.mints_subnests), mints the file's own child
//      nest, <gate folder>/<gate>-<hash16>_<name>/,
//   4. runs the data type's route from routes.toml at that nest, so the
//      file is in the ledger and the graph and gets the route's hops
//      (store writes the .ref pointer and .meta.json into entries/),
//   5. removes the dropped file. If the route fails, the bytes are moved
//      back and the file is left as it was.
//
// Nothing is automatic: adopt runs when asked (`mailroom adopt`, the
// button, POST /adopt). GET /adopt and `--dry-run` only list what's waiting.
//
// Only files sitting directly in the nest folder are considered: entries/,
// child nests and other subfolders are never touched. Hidden files, partial
// downloads, and files modified in the last few seconds (still copying) are
// skipped.
//
// Each intake nest gets a .gitignore that ignores loose files at its top
// level, so a file dropped on a machine where adopt hasn't run yet is never
// committed whole.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;

use crate::envelope::{Envelope, Payload, Source};
use crate::manifest::Manifest;
use crate::registry::Registry;
use crate::route::{Route, Stop};
use crate::routing::RoutingTable;

/// A file modified more recently than this is assumed to be still copying.
pub const SETTLE: Duration = Duration::from_secs(5);

const GITIGNORE_MARK: &str = "# Written by Mailroom (adopt).";

/// A file waiting in an intake nest.
#[derive(Debug, Clone, Serialize)]
pub struct Waiting {
    pub path: PathBuf,
    pub nest: String,
    pub data_type: String,
    pub bytes: u64,
    /// Each file gets its own child nest.
    pub rookery: bool,
    /// Why it can't be adopted right now, if it can't.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Adopted {
    pub from: PathBuf,
    /// The nest it now belongs to (the minted child, in a Rookery).
    pub nest: String,
    pub content_hash: String,
    pub minted: bool,
    /// The same bytes were already in the coffer.
    pub duplicate: bool,
    pub pointer: Option<PathBuf>,
}

#[derive(Debug, Default, Serialize)]
pub struct Outcome {
    pub dry_run: bool,
    pub waiting: Vec<Waiting>,
    pub adopted: Vec<Adopted>,
    pub failed: Vec<Failed>,
    pub gitignores_written: Vec<PathBuf>,
}

#[derive(Debug, Serialize)]
pub struct Failed {
    pub path: PathBuf,
    pub error: String,
}

/// Every file waiting in an intake nest. Reads only.
pub fn waiting(vault: &Path, registry: &Registry, routing: &RoutingTable, settle: Duration) -> Vec<Waiting> {
    let mut found = Vec::new();
    let mut nests: Vec<&Manifest> = registry.all().into_iter().filter(|m| m.intake.is_some()).collect();
    nests.sort_by(|a, b| a.id.cmp(&b.id));

    for nest in nests {
        let data_type = nest.intake.clone().unwrap_or_default();
        let Some(dir) = registry.nest_dir(vault, &nest.id) else { continue };
        let rookery = nest.mailroom.as_ref().is_some_and(|m| m.mints_subnests);

        // Problems that block every file in this nest.
        let nest_problem = match routing.resolve(&data_type) {
            None => Some(format!("no route for data type '{data_type}' in routes.toml")),
            Some((dodo, _)) if !dodo.can_visit(&nest.id) => Some(format!(
                "the '{data_type}' route's dodo '{}' may not visit nest {}",
                dodo.id, nest.id
            )),
            Some(_) => None,
        };

        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut files: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        files.sort_by_key(|e| e.file_name());
        for entry in files {
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if ignored(&name) {
                continue;
            }
            let fresh = meta
                .modified()
                .ok()
                .and_then(|m| SystemTime::now().duration_since(m).ok())
                .is_some_and(|age| age < settle);
            let blocked = nest_problem.clone().or_else(|| {
                fresh.then(|| "modified in the last few seconds; may still be copying".to_string())
            });
            found.push(Waiting {
                path: entry.path(),
                nest: nest.id.clone(),
                data_type: data_type.clone(),
                bytes: meta.len(),
                rookery,
                blocked,
            });
        }
    }
    found
}

/// Names never adopted: Mailroom's own files, hidden files, partial downloads.
fn ignored(name: &str) -> bool {
    name == "nest"
        || name.starts_with('.')
        || name.ends_with('~')
        || [".part", ".partial", ".tmp", ".crdownload", ".download", ".ref", ".meta.json"]
            .iter()
            .any(|ext| name.ends_with(ext))
}

/// List what's waiting and, unless `dry_run`, adopt everything not blocked.
/// Holds `registry` mutably because a Rookery mints nests.
pub async fn run(
    vault: &Path,
    registry: &mut Registry,
    routing: &RoutingTable,
    dry_run: bool,
    settle: Duration,
) -> anyhow::Result<Outcome> {
    let waiting = waiting(vault, registry, routing, settle);
    let mut outcome = Outcome { dry_run, waiting: waiting.clone(), ..Default::default() };
    if dry_run {
        return Ok(outcome);
    }

    // The safety net goes in first, so it exists even if an adoption fails.
    let intake_nests: Vec<String> = registry
        .all()
        .into_iter()
        .filter(|m| m.intake.is_some())
        .map(|m| m.id.clone())
        .collect();
    for id in intake_nests {
        if let Some(dir) = registry.nest_dir(vault, &id) {
            if write_gitignore(&dir)? {
                outcome.gitignores_written.push(dir.join(".gitignore"));
            }
        }
    }

    for w in waiting.into_iter().filter(|w| w.blocked.is_none()) {
        match adopt_one(vault, registry, routing, &w).await {
            Ok(a) => outcome.adopted.push(a),
            Err(e) => {
                tracing::warn!(path = %w.path.display(), error = %e, "adopt failed; file left in place");
                outcome.failed.push(Failed { path: w.path.clone(), error: format!("{e:#}") });
            }
        }
    }
    Ok(outcome)
}

async fn adopt_one(vault: &Path, registry: &mut Registry, routing: &RoutingTable, w: &Waiting) -> anyhow::Result<Adopted> {
    let (dodo, resolved) = routing
        .resolve(&w.data_type)
        .ok_or_else(|| anyhow::anyhow!("no route for '{}'", w.data_type))?;
    let gate = registry
        .get(&w.nest)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("nest {} is no longer registered", w.nest))?;

    // 1. Hash, off the async executor (large files).
    let src = w.path.clone();
    let hash = tokio::task::spawn_blocking(move || crate::blobs::hash_file(&src)).await??;

    // 2. Bytes into the coffer.
    let coffer = crate::coffer::for_vault(vault)?;
    let blob = coffer.contents.join(crate::blobs::rel_path(&hash));
    let duplicate = blob.exists();
    let original_perms = std::fs::metadata(&w.path)?.permissions();
    let mut moved = false;
    if !duplicate {
        std::fs::create_dir_all(blob.parent().expect("blob paths have a parent"))?;
        if std::fs::rename(&w.path, &blob).is_ok() {
            moved = true;
            set_read_only(&blob)?;
        } else {
            // Another filesystem: verified copy; the original goes once the
            // route has succeeded.
            crate::blobs::put(&coffer.contents, &w.path, &hash).await?;
        }
    }

    // 3. In a Rookery, the file's own nest.
    let stem = w.path.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let file_name = w.path.file_name().unwrap_or_default().to_string_lossy().to_string();
    let slug = {
        let s = crate::attendant::slugify(&stem);
        if s.is_empty() { hash[..16].to_string() } else { s }
    };
    let (target, minted) = if w.rookery {
        let child_id = crate::attendant::child_id_for_hash(&gate, &hash);
        let existed = registry.get(&child_id).is_some();
        crate::attendant::mint_child(vault, registry, &gate, &child_id, &slug, &w.data_type, None)?;
        (child_id, !existed)
    } else {
        (gate.id.clone(), false)
    };

    // 4. The route, at that nest.
    let mut envelope = Envelope::new(
        Source::Internal("adopt".into()),
        w.data_type.clone(),
        Payload::FilePath(blob.clone()),
        Some(target.clone()),
    );
    envelope.content_hash = Some(hash.clone());
    envelope.meta.insert("title".into(), stem.clone());
    envelope.meta.insert("filename".into(), file_name);
    envelope.meta.insert("original_path".into(), w.path.display().to_string());
    // The graph node: the child nest itself in a Rookery (one nest, one
    // node), else <nest>/<name>.
    let key = if w.rookery { target.clone() } else { format!("{}/{}", gate.id, slug) };
    envelope.meta.insert("key".into(), key);

    let mut route = Route::new(
        dodo.id.clone(),
        vec![Stop { nest_id: target.clone(), hops: resolved.hops.clone() }],
    )
    .watched_by(resolved.watchers.clone());

    if let Err(e) = route.run(vault, Some(&envelope)).await {
        // 5a. Undo the move; the ledger has the failed stop.
        if moved {
            let _ = std::fs::set_permissions(&blob, original_perms.clone());
            if std::fs::rename(&blob, &w.path).is_ok() {
                let _ = std::fs::set_permissions(&w.path, original_perms);
            }
        }
        return Err(e);
    }

    // 5b. The dropped file is now redundant (copied, or already stored).
    if !moved && w.path.exists() {
        let mut p = original_perms;
        #[allow(clippy::permissions_set_readonly_false)]
        p.set_readonly(false);
        let _ = std::fs::set_permissions(&w.path, p);
        std::fs::remove_file(&w.path)?;
    }

    Ok(Adopted {
        from: w.path.clone(),
        nest: target,
        content_hash: hash,
        minted,
        duplicate,
        pointer: route.stored.first().cloned(),
    })
}

/// `mailroom adopt [--dry-run]`. Asks the running server (MAILROOM_LISTEN)
/// to do it, since only one process may write the ledger; with no server
/// running, does it directly.
pub async fn cli(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let addr = std::env::var("MAILROOM_LISTEN").unwrap_or_else(|_| "127.0.0.1:3000".into());
    let url = format!("http://{addr}/adopt");
    let client = reqwest::Client::builder().timeout(Duration::from_secs(3600)).build()?;
    let request = if dry_run { client.get(&url) } else { client.post(&url) };

    let outcome: serde_json::Value = match request.send().await {
        Ok(resp) => {
            println!("(via the running Mailroom at {addr})");
            let status = resp.status();
            let body: serde_json::Value = resp.json().await?;
            if !status.is_success() {
                return Err(format!("server said {status}: {body}").into());
            }
            body
        }
        Err(e) if e.is_connect() => {
            let vault = PathBuf::from(std::env::var("MAILROOM_VAULT").map_err(|_| "set MAILROOM_VAULT")?);
            println!("(no Mailroom running at {addr}; adopting directly in {})", vault.display());
            let mut registry = Registry::load(&vault)?;
            let routing = RoutingTable::load(&vault.join("routes.toml"), &vault, &registry)?;
            let o = run(&vault, &mut registry, &routing, dry_run, SETTLE).await?;
            serde_json::to_value(&o)?
        }
        Err(e) => return Err(e.into()),
    };
    print_outcome(&outcome);
    Ok(())
}

fn print_outcome(o: &serde_json::Value) {
    let empty = Vec::new();
    let list = |k: &str| o[k].as_array().unwrap_or(&empty).clone();
    let waiting = list("waiting");
    if o["dry_run"].as_bool() == Some(true) {
        if waiting.is_empty() {
            println!("nothing waiting");
        }
        for w in &waiting {
            println!(
                "{:<8} {}  ({}, {} bytes){}{}",
                w["nest"].as_str().unwrap_or(""),
                w["path"].as_str().unwrap_or(""),
                w["data_type"].as_str().unwrap_or(""),
                w["bytes"],
                if w["rookery"].as_bool() == Some(true) { ", own nest" } else { "" },
                w["blocked"].as_str().map(|b| format!("  BLOCKED: {b}")).unwrap_or_default()
            );
        }
        return;
    }
    for g in list("gitignores_written") {
        println!("wrote {}", g.as_str().unwrap_or(""));
    }
    for a in list("adopted") {
        println!(
            "took in {} -> {}{}{}",
            a["from"].as_str().unwrap_or(""),
            a["nest"].as_str().unwrap_or(""),
            if a["minted"].as_bool() == Some(true) { " (new nest)" } else { "" },
            if a["duplicate"].as_bool() == Some(true) { " (bytes already in the coffer)" } else { "" },
        );
    }
    for f in list("failed") {
        println!("FAILED {}: {}", f["path"].as_str().unwrap_or(""), f["error"].as_str().unwrap_or(""));
    }
    for w in waiting.iter().filter(|w| !w["blocked"].is_null()) {
        println!("waiting {}: {}", w["path"].as_str().unwrap_or(""), w["blocked"].as_str().unwrap_or(""));
    }
    println!(
        "{} taken in, {} failed",
        list("adopted").len(),
        list("failed").len()
    );
}

/// Ignore loose files at the top of an intake nest; keep everything else
/// (the nest file, entries/, child nests). Leaves an existing .gitignore
/// alone. Returns whether one was written.
fn write_gitignore(nest_dir: &Path) -> anyhow::Result<bool> {
    let path = nest_dir.join(".gitignore");
    if path.exists() {
        return Ok(false);
    }
    std::fs::write(
        &path,
        format!(
            "{GITIGNORE_MARK} Files dropped here wait for `mailroom adopt`,\n\
             # which moves their bytes to the coffer; only pointers and nests are tracked.\n\
             /*\n!/*/\n!/nest\n!/.gitignore\n"
        ),
    )?;
    Ok(true)
}

#[cfg(unix)]
fn set_read_only(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444))
}

#[cfg(not(unix))]
fn set_read_only(path: &Path) -> std::io::Result<()> {
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(path, perms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const HELLO: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    const ROUTES: &str = r#"
        [dodos.librarian]
        home_nest       = "52-B"
        available_nests = ["52-B", "52-E"]

        [hops.store]
        store = true

        [hops.link]
        link = "replace"

        [[routes]]
        data_type = "media/book"
        dodo      = "librarian"
        nest      = "52-B"
        hops      = ["store", "link"]

        [[routes]]
        data_type = "media/movie"
        dodo      = "librarian"
        nest      = "52-E"
        hops      = ["store"]
    "#;

    /// A vault with a coffer, a books Rookery and a plain movies nest.
    fn vault() -> (tempfile::TempDir, Registry, RoutingTable) {
        let vault = tempfile::tempdir().unwrap();
        let v = vault.path();
        crate::coffer::init(v).unwrap();
        let books = v.join("52_media/52-B_books");
        fs::create_dir_all(&books).unwrap();
        fs::write(
            books.join("nest"),
            "id = \"52-B\"\nname = \"books\"\nkind = \"collection\"\nintake = \"media/book\"\n\n\
             [mailroom]\nactive = true\nrequires_auth = false\nnotify_on_write = false\n\
             ai_classify = false\nmints_subnests = true\n",
        )
        .unwrap();
        let movies = v.join("52_media/52-E_movies");
        fs::create_dir_all(&movies).unwrap();
        fs::write(movies.join("nest"), "id = \"52-E\"\nname = \"movies\"\nkind = \"leaf\"\nintake = \"media/movie\"\n").unwrap();
        let registry = Registry::load(v).unwrap();
        let routing = RoutingTable::from_toml(ROUTES, v, &registry).unwrap();
        (vault, registry, routing)
    }

    fn records(v: &Path) -> Vec<crate::ledger::Record> {
        crate::ledger::for_vault(v).unwrap().records().unwrap()
    }

    #[tokio::test]
    async fn a_book_dropped_into_a_rookery_gets_its_own_nest_and_its_bytes_go_to_the_coffer() {
        let (vault, mut registry, routing) = vault();
        let v = vault.path();
        let drop = v.join("52_media/52-B_books/Designing Data Apps.txt");
        fs::write(&drop, "hello").unwrap();

        let out = run(v, &mut registry, &routing, false, Duration::ZERO).await.unwrap();

        assert_eq!(out.adopted.len(), 1, "{:?}", out.failed);
        let a = &out.adopted[0];
        let child = format!("52-B-{}", &HELLO[..16]);
        assert_eq!(a.nest, child);
        assert!(a.minted && !a.duplicate);
        assert!(!drop.exists(), "the dropped file is gone");

        let child_dir = v.join(format!("52_media/52-B_books/{child}_Designing-Data-Apps"));
        assert!(child_dir.join("nest").exists(), "the book's own nest");
        let pointer = a.pointer.clone().unwrap();
        assert!(pointer.starts_with(child_dir.join("entries")));
        let text = fs::read_to_string(&pointer).unwrap();
        assert!(text.starts_with(&format!("ref: sha256:{HELLO}\noriginal: ")), "{text}");
        assert!(text.contains("Designing Data Apps.txt"));

        let blob = v.join(format!("85_coffer/85-B_contents/2c/{HELLO}"));
        assert_eq!(fs::read_to_string(&blob).unwrap(), "hello");
        assert!(fs::metadata(&blob).unwrap().permissions().readonly());

        // In the ledger and the graph: the book's node is its nest.
        assert!(records(v).iter().any(|r| r.event.kind == crate::event::EventKind::Received));
        let ledger = crate::ledger::for_vault(v).unwrap();
        assert!(ledger.with_graph(|g| g.has_edge(&child, "VISITED", &format!("nest:{child}"))));
        assert!(ledger.verify().problems.is_empty());

        assert!(v.join("52_media/52-B_books/.gitignore").exists());
    }

    #[tokio::test]
    async fn a_plain_intake_nest_keeps_the_file_in_itself() {
        let (vault, mut registry, routing) = vault();
        let v = vault.path();
        fs::write(v.join("52_media/52-E_movies/Dune.mkv"), "hello").unwrap();

        let out = run(v, &mut registry, &routing, false, Duration::ZERO).await.unwrap();

        let a = &out.adopted[0];
        assert_eq!(a.nest, "52-E");
        assert!(!a.minted);
        assert!(a.pointer.as_ref().unwrap().starts_with(v.join("52_media/52-E_movies/entries")));
        let ledger = crate::ledger::for_vault(v).unwrap();
        assert!(ledger.with_graph(|g| g.node("52-E/Dune").is_some()));
    }

    #[tokio::test]
    async fn a_dry_run_lists_and_touches_nothing() {
        let (vault, mut registry, routing) = vault();
        let v = vault.path();
        let drop = v.join("52_media/52-B_books/book.txt");
        fs::write(&drop, "hello").unwrap();
        fs::write(v.join("52_media/52-B_books/.hidden"), "x").unwrap();
        fs::write(v.join("52_media/52-B_books/movie.mkv.part"), "x").unwrap();
        fs::create_dir_all(v.join("52_media/52-B_books/entries")).unwrap();
        fs::write(v.join("52_media/52-B_books/entries/old.ref"), "x").unwrap();

        let out = run(v, &mut registry, &routing, true, Duration::ZERO).await.unwrap();

        assert_eq!(out.waiting.len(), 1, "hidden, partial and entries/ files are not candidates");
        assert_eq!(out.waiting[0].path, drop);
        assert!(out.waiting[0].rookery);
        assert!(out.adopted.is_empty() && drop.exists());
        assert!(!v.join("85_coffer/85-B_contents/2c").exists());
        assert!(!v.join("52_media/52-B_books/.gitignore").exists());
    }

    #[tokio::test]
    async fn a_file_still_being_copied_waits() {
        let (vault, mut registry, routing) = vault();
        let v = vault.path();
        fs::write(v.join("52_media/52-B_books/book.txt"), "hello").unwrap();
        let out = run(v, &mut registry, &routing, false, Duration::from_secs(60)).await.unwrap();
        assert!(out.adopted.is_empty());
        assert!(out.waiting[0].blocked.as_deref().unwrap().contains("still be copying"));
    }

    #[tokio::test]
    async fn the_same_bytes_dropped_twice_make_one_nest_and_one_blob() {
        let (vault, mut registry, routing) = vault();
        let v = vault.path();
        fs::write(v.join("52_media/52-B_books/a.txt"), "hello").unwrap();
        run(v, &mut registry, &routing, false, Duration::ZERO).await.unwrap();
        fs::write(v.join("52_media/52-B_books/a copy.txt"), "hello").unwrap();
        let out = run(v, &mut registry, &routing, false, Duration::ZERO).await.unwrap();

        let a = &out.adopted[0];
        assert!(a.duplicate && !a.minted, "same content, same nest");
        assert!(!v.join("52_media/52-B_books/a copy.txt").exists());
        let blobs = fs::read_dir(v.join("85_coffer/85-B_contents/2c")).unwrap().count();
        assert_eq!(blobs, 1);
    }

    #[tokio::test]
    async fn a_failing_route_puts_the_file_back() {
        let (vault, mut registry, _) = vault();
        let v = vault.path();
        let failing = ROUTES.replace(
            "[hops.store]\n        store = true",
            "[hops.store]\n        run = \"false\"",
        );
        let routing = RoutingTable::from_toml(&failing, v, &registry).unwrap();
        let drop = v.join("52_media/52-E_movies/Dune.mkv");
        fs::write(&drop, "hello").unwrap();

        let out = run(v, &mut registry, &routing, false, Duration::ZERO).await.unwrap();

        assert_eq!(out.failed.len(), 1);
        assert_eq!(fs::read_to_string(&drop).unwrap(), "hello", "the file is back where it was");
        assert!(!fs::metadata(&drop).unwrap().permissions().readonly());
        assert!(!v.join(format!("85_coffer/85-B_contents/2c/{HELLO}")).exists());
        assert!(records(v).iter().any(|r| r.event.kind == crate::event::EventKind::Error));
    }

    #[tokio::test]
    async fn a_nest_whose_data_type_has_no_route_is_reported_not_adopted() {
        let (vault, mut registry, _) = vault();
        let v = vault.path();
        let routing = RoutingTable::from_toml(&ROUTES.replace("media/movie", "media/film"), v, &registry).unwrap();
        fs::write(v.join("52_media/52-E_movies/Dune.mkv"), "hello").unwrap();
        let out = run(v, &mut registry, &routing, false, Duration::ZERO).await.unwrap();
        assert!(out.adopted.is_empty());
        assert!(out.waiting[0].blocked.as_deref().unwrap().contains("no route"));
        assert!(v.join("52_media/52-E_movies/Dune.mkv").exists());
    }
}
