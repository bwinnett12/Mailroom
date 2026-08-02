// src/cli.rs
//
// Ad hoc, generic nest creator/initializer. Reuses Manifest directly so
// it can never produce a shape Registry can't parse back.
//
// Two distinct things this can do:
//
//   1. Initialize an EXISTING directory in place — just drop a nest file
//      into it, no new folder created. This is what you want when a
//      directory already has content and you just never seeded it.
//
//   2. Create a brand-new node — makes a new "{id}_{name}" subfolder
//      under a given parent (old behavior, still supported via --parent).
//
// Locating the target directory:
//   --target <path>   explicit — write directly into this existing dir.
//   --parent <path>   explicit — create a new "{id}_{name}" child under this.
//   (neither given)   search the vault for a directory whose name matches
//                     --id (case-insensitive, separator-agnostic — same
//                     matching rule the Python disk-checker uses), and
//                     initialize it in place if exactly one match is found.
//
// Vault root for that search comes from --vault, or the MAILROOM_VAULT
// env var (matching main.rs's own resolution), or "." if neither is set.
//
// Fresh-trees policy: if a nest already exists at the resolved location,
// it's left completely untouched — no overwrite, no field merging. Pass
// --force to override and write a fresh blank stub anyway. This matches
// orchard.rs's create_station, so this tool and the schema bootstrap
// agree on what "safe to re-run" means.
//
// Usage:
//   # explicit, brand-new node under a parent
//   cargo run -- add-nest --id "35-I" --name "new-thing" --parent /path/to/35_health --kind leaf
//
//   # explicit, initialize an existing directory in place
//   cargo run -- add-nest --id "35-I" --name "new-thing" --target /path/to/35_health/35-I_existing-folder
//
//   # auto-locate by code within the vault, initialize in place
//   MAILROOM_VAULT=/Users/b/Projects/test-orchard/Orchard \
//   cargo run -- add-nest --id "12-B" --kind leaf

use std::path::{Path, PathBuf};
use std::process::Command;

use walkdir::WalkDir;

use crate::manifest::{Manifest, NodeKind};
use crate::orchard::create_station;
use crate::nest::Nest;
use crate::schema::generate_fresh_tree;

pub fn add_nest(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut id: Option<String> = None;
    let mut name: Option<String> = None;
    let mut vault: Option<PathBuf> = None;
    let mut target: Option<PathBuf> = None;
    let mut parent: Option<PathBuf> = None;
    let mut kind = NodeKind::Inferred;
    let mut force = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--id" => id = Some(require_value(args, &mut i, "--id")?),
            "--name" => name = Some(require_value(args, &mut i, "--name")?),
            "--vault" => vault = Some(PathBuf::from(require_value(args, &mut i, "--vault")?)),
            "--target" => target = Some(PathBuf::from(require_value(args, &mut i, "--target")?)),
            "--parent" => parent = Some(PathBuf::from(require_value(args, &mut i, "--parent")?)),
            "--kind" => kind = parse_kind(&require_value(args, &mut i, "--kind")?),
            "--force" => { force = true; i += 1; }
            other => {
                eprintln!("Unrecognized argument: {other} (ignoring)");
                i += 1;
            }
        }
    }

    let id = id.ok_or("missing required --id")?;

    let vault_root = vault
        .or_else(|| std::env::var("MAILROOM_VAULT").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));

    let resolve = |p: PathBuf| -> PathBuf {
        if p.is_absolute() { p } else { vault_root.join(p) }
    };

    if let Some(parent) = parent {
        let parent = resolve(parent);
        let name = name.ok_or("--parent mode requires --name (a new folder is being created)")?;
        return create_new_node(&id, &name, kind, &parent);
    }

    if let Some(target) = target {
        let target = resolve(target);
        let name = name.unwrap_or_else(|| derive_name(&target, &id));
        return init_in_place(&id, &name, kind, &target, force);
    }

    let matches = find_matching_dirs(&vault_root, &id);
    match matches.len() {
        0 => Err(format!(
            "No directory under {} matches code \"{}\". Pass --target or --parent explicitly.",
            vault_root.display(),
            id
        )
        .into()),
        1 => {
            let found = &matches[0];
            let name = name.unwrap_or_else(|| derive_name(found, &id));
            println!("Found match for \"{id}\": {}", found.display());
            init_in_place(&id, &name, kind, found, force)
        }
        _ => {
            eprintln!("Multiple directories under {} match \"{}\":", vault_root.display(), id);
            for m in &matches {
                eprintln!("  {}", m.display());
            }
            Err("Ambiguous — pass --target to disambiguate.".into())
        }
    }
}

fn create_new_node(
    id: &str,
    name: &str,
    kind: NodeKind,
    parent: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let manifest = blank_manifest(id, name, kind);
    let node = Nest { manifest, children: Vec::new() };
    create_station(&node, parent)?;
    println!("Created new node \"{id}\" (\"{name}\") under {}", parent.display());
    Ok(())
}

fn init_in_place(
    id: &str,
    name: &str,
    kind: NodeKind,
    target: &Path,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !target.is_dir() {
        eprintln!("Note: {} does not exist yet — creating it.", target.display());
    }
    std::fs::create_dir_all(target)?;

    let nest_path = target.join("nest");
    if nest_path.exists() && !force {
        println!(
            "Nest already exists at {} — leaving it in place (pass --force to overwrite).",
            nest_path.display()
        );
        return Ok(());
    }

    let manifest = blank_manifest(id, name, kind);
    let toml_text = toml::to_string_pretty(&manifest)?;
    std::fs::write(&nest_path, toml_text)?;

    let verb = if force { "Re-initialized (forced)" } else { "Initialized" };
    println!("{verb} \"{id}\" (\"{name}\") in place at {}", target.display());
    Ok(())
}

fn blank_manifest(id: &str, name: &str, kind: NodeKind) -> Manifest {
    Manifest {
        id: id.to_string(),
        name: name.to_string(),
        path: None,
        kind,
        accepts: Vec::new(),
        store: None,
        routing: None,
        library: None,
        mailroom: None,
        about: None,
        known_tags: Vec::new(),
        call_number: None,
    }
}

fn find_matching_dirs(vault_root: &Path, code: &str) -> Vec<PathBuf> {
    WalkDir::new(vault_root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_dir())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|n| matches_code(n, code))
                .unwrap_or(false)
        })
        .map(|e| e.path().to_path_buf())
        .collect()
}

fn matches_code(dirname: &str, code: &str) -> bool {
    let dirname_lower = dirname.to_lowercase();
    let code_lower = code.to_lowercase();
    if !dirname_lower.starts_with(&code_lower) {
        return false;
    }
    match dirname_lower.as_bytes().get(code_lower.len()) {
        None => true,
        Some(&b) => !(b as char).is_ascii_alphanumeric(),
    }
}

fn derive_name(dir: &Path, code: &str) -> String {
    let dirname = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if dirname.len() > code.len() {
        let after = &dirname[code.len()..];
        let trimmed = after.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    dirname.to_string()
}

fn require_value(args: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    let value = args
        .get(*i + 1)
        .ok_or_else(|| format!("{flag} requires a value"))?
        .clone();
    *i += 2;
    Ok(value)
}

fn parse_kind(raw: &str) -> NodeKind {
    match raw {
        "domain" => NodeKind::Domain,
        "collection" => NodeKind::Collection,
        "leaf" => NodeKind::Leaf,
        "log" => NodeKind::Log,
        "archive" => NodeKind::Archive,
        "index" => NodeKind::Index,
        "inferred" => NodeKind::Inferred,
        other => {
            eprintln!("Unrecognized --kind '{other}', defaulting to inferred");
            NodeKind::Inferred
        }
    }
}

// ── refresh-schema subcommand ──────────────────────────────────────────────
//
// Wraps the full schema-refresh Lane: pull the latest Orchard checkout,
// regenerate schema.ron from its master index via the Python script, then
// materialize (merge-safe — see orchard.rs's fresh-trees policy) against
// the same vault. Safe to run repeatedly: git pull is a no-op if already
// current, the Python step just rewrites schema.ron, and materialize only
// ever fills in genuinely missing nodes.
//
// Usage:
//   cargo run -- refresh-schema --vault /path/to/Orchard
//   cargo run -- refresh-schema --vault /path/to/Orchard --script /path/to/build-ron-from-index.py --skip-pull
//
// Flags:
//   --vault <path>    the Orchard checkout to refresh (falls back to
//                      MAILROOM_VAULT, then ".")
//   --script <path>   path to build-ron-from-index.py (falls back to
//                      MAILROOM_SCHEMA_SCRIPT, then
//                      "{vault}/18_shells-scripts/build-ron-from-index.py")
//   --schema-path <path>  where schema.ron should land (falls back to
//                      MAILROOM_SCHEMA_PATH, then "{vault}/schema.ron")
//   --skip-pull       don't attempt `git pull` first — useful for a vault
//                      that isn't a git checkout yet, or offline testing
pub fn refresh_schema(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut vault: Option<PathBuf> = None;
    let mut script: Option<PathBuf> = None;
    let mut schema_path: Option<PathBuf> = None;
    let mut skip_pull = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--vault" => vault = Some(PathBuf::from(require_value(args, &mut i, "--vault")?)),
            "--script" => script = Some(PathBuf::from(require_value(args, &mut i, "--script")?)),
            "--schema-path" => {
                schema_path = Some(PathBuf::from(require_value(args, &mut i, "--schema-path")?))
            }
            "--skip-pull" => {
                skip_pull = true;
                i += 1;
            }
            other => {
                eprintln!("Unrecognized argument: {other} (ignoring)");
                i += 1;
            }
        }
    }

    let vault_root = vault
        .or_else(|| std::env::var("MAILROOM_VAULT").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));

    let script_path = script
        .or_else(|| std::env::var("MAILROOM_SCHEMA_SCRIPT").ok().map(PathBuf::from))
        .unwrap_or_else(|| vault_root.join("18_shells-scripts/build-ron-from-index.py"));

    let schema_path = schema_path
        .or_else(|| std::env::var("MAILROOM_SCHEMA_PATH").ok().map(PathBuf::from))
        .unwrap_or_else(|| vault_root.join("schema.ron"));

    if !vault_root.is_dir() {
        return Err(format!("Vault root {} does not exist.", vault_root.display()).into());
    }
    if !script_path.is_file() {
        return Err(format!(
            "Schema script not found at {} (pass --script or set MAILROOM_SCHEMA_SCRIPT).",
            script_path.display()
        )
        .into());
    }

    if skip_pull {
        println!("Skipping git pull (--skip-pull).");
    } else {
        println!("Pulling latest Orchard at {}...", vault_root.display());
        let pull = Command::new("git")
            .arg("-C")
            .arg(&vault_root)
            .arg("pull")
            .output();

        match pull {
            Ok(output) if output.status.success() => {
                print!("{}", String::from_utf8_lossy(&output.stdout));
            }
            Ok(output) => {
                eprintln!(
                    "Warning: git pull failed (continuing with current checkout): {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Err(e) => {
                eprintln!("Warning: could not run git pull (continuing with current checkout): {e}");
            }
        }
    }

    println!(
        "Running schema generator: python3 {} -> {}",
        script_path.display(),
        schema_path.display()
    );
    let python = Command::new("python3")
        .arg(&script_path)
        .arg(&schema_path)
        .current_dir(&vault_root)
        .output()?;

    print!("{}", String::from_utf8_lossy(&python.stdout));
    if !python.status.success() {
        return Err(format!(
            "Schema generation failed:\n{}",
            String::from_utf8_lossy(&python.stderr)
        )
        .into());
    }

    println!("Materializing against {}...", vault_root.display());
    generate_fresh_tree(&schema_path, &vault_root)?;

    println!("Schema refresh complete.");
    Ok(())
}


// ── move-branch subcommand ──────────────────────────────────────────────
//
// Safe JD renumbering: rename a Nest's id, move its physical directory
// to match, rewire any other nest file's [routing] rules that pointed
// at the old id, and merge properly if the destination id already has
// its own Nest (see move_branch.rs's MergeStrategy for the three
// explicit resolution options).
//
// Usage:
//   cargo run -- move-branch --from 52-B-bbc35551 --to 663.44
//   cargo run -- move-branch --from 52-B-bbc35551 --to 663.44 --dry-run
//   cargo run -- move-branch --from 52-B-bbc35551 --to 663.44 --merge take-source
//
// Flags:
//   --from <id>     required — the id to rename away from
//   --to <id>       required — the new id
//   --vault <path>  falls back to MAILROOM_VAULT, then "."
//   --dry-run       report what would happen, write nothing
//   --merge <mode>  keep-destination | take-source | compare (default) —
//                   only relevant if --to already has its own Nest
pub fn move_branch(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut from_id: Option<String> = None;
    let mut to_id: Option<String> = None;
    let mut vault: Option<PathBuf> = None;
    let mut dry_run = false;
    let mut merge_strategy = crate::move_branch::MergeStrategy::Compare;
 
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--from" => from_id = Some(require_value(args, &mut i, "--from")?),
            "--to"   => to_id   = Some(require_value(args, &mut i, "--to")?),
            "--vault" => vault = Some(PathBuf::from(require_value(args, &mut i, "--vault")?)),
            "--dry-run" => { dry_run = true; i += 1; }
            "--merge" => {
                let value = require_value(args, &mut i, "--merge")?;
                merge_strategy = match value.as_str() {
                    "keep-destination" => crate::move_branch::MergeStrategy::KeepDestination,
                    "take-source"      => crate::move_branch::MergeStrategy::TakeSource,
                    "compare"          => crate::move_branch::MergeStrategy::Compare,
                    other => {
                        return Err(format!(
                            "Unknown --merge value '{other}' — expected keep-destination, take-source, or compare"
                        ).into());
                    }
                };
            }
            other => {
                eprintln!("Unrecognized argument: {other} (ignoring)");
                i += 1;
            }
        }
    }
 
    let from_id = from_id.ok_or("missing required --from")?;
    let to_id = to_id.ok_or("missing required --to")?;
 
    let vault_root = vault
        .or_else(|| std::env::var("MAILROOM_VAULT").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
 
    let report = crate::move_branch::move_branch(&vault_root, &from_id, &to_id, merge_strategy, dry_run)?;
 
    if report.merged_into_existing && merge_strategy == crate::move_branch::MergeStrategy::Compare {
        println!("Destination '{to_id}' already has its own Nest — nothing written.");
        println!("Differences found:");
        for diff in &report.manifest_diffs {
            println!("  {diff}");
        }
        println!("Rerun with --merge take-source or --merge keep-destination to proceed.");
    } else if report.dry_run {
        println!(
            "Dry run: would move '{}' -> '{}' ({} -> {})",
            report.from_id, report.to_id, report.old_path.display(), report.new_path.display()
        );
    } else if report.merged_into_existing {
        println!(
            "Merged '{}' into existing '{}' at {} (strategy: {:?})",
            report.from_id, report.to_id, report.new_path.display(), merge_strategy
        );
    } else {
        println!(
            "Moved '{}' -> '{}' ({} -> {})",
            report.from_id, report.to_id, report.old_path.display(), report.new_path.display()
        );
    }
 
    for rule in &report.rewired_routing_rules {
        println!("  rewired [routing] rule for \"{}\" in {}", rule.1, rule.0);
    }
    for warning in &report.hardcoded_warnings {
        println!("\n⚠️  {warning}");
    }
 
    Ok(())
}
