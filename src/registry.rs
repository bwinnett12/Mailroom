// src/registry.rs
//
// The Registry is built once at startup by walking the vault directory tree.
// It finds every .mailroom file, parses it into a Manifest, and stores
// everything in a HashMap keyed by JD address.
//
// After startup, the Registry is read-only — handlers query it but never
// modify it. Modifications happen by editing .mailroom files and restarting,
// or later via a `mailroom refresh` command.

use std::{
    collections::HashMap,
    // HashMap<K, V> is Rust's dictionary / associative map.
    // K = key type, V = value type.
    // Lookup is O(1) average — fast enough for any realistic JD tree size.

    path::{Path},
    // Path    — a borrowed reference to a filesystem path (like &str for strings)
    // Path — an owned, heap-allocated path (like String for strings)
    // We use Path for function arguments (we just need to look at it)
    // and PathBuf for storing paths inside structs (we need to own it)
};

use walkdir::WalkDir;
// WalkDir produces an iterator over every file and directory
// under a given root, recursively. We'll filter for .mailroom files.

use crate::manifest::Manifest;
// Bring our Manifest type into scope from the manifest module.
// `crate::` means "start from the root of this crate" —
// like an absolute path but for Rust modules instead of the filesystem.

// ── Registry ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Registry {
    /// The core map: JD address string → parsed Manifest.
    /// e.g. "35.2" → Manifest { id: "35.2", name: "Physical", ... }
    entries: HashMap<String, Manifest>,

    /// How many .mailroom files were found during the last load.
    /// Useful for logging and health checks.
    pub node_count: usize,
}

impl Registry {
    /// Walk the vault and build a Registry from all .mailroom files found.
    ///
    /// This is called once at startup. It's synchronous — we block on
    /// filesystem I/O during startup rather than making this async,
    /// because the server shouldn't accept requests until the registry
    /// is ready. Startup blocking is acceptable; request blocking is not.
    ///
    /// Returns an error if the vault root doesn't exist or a .mailroom
    /// file contains invalid TOML.
    pub fn load(vault_root: &Path) -> anyhow::Result<Self> {
        // anyhow::Result<T> is shorthand for Result<T, anyhow::Error>.
        // anyhow::Error can wrap any error type — we don't have to define
        // our own. The ? operator below converts any error into anyhow::Error
        // automatically.

        let mut entries = HashMap::new();
        // `mut` = mutable. In Rust, variables are immutable by default.
        // You must explicitly opt in to mutation with `mut`.
        // entries starts empty; we fill it in the loop below.

        tracing::info!(
            vault_root = %vault_root.display(),
            "loading registry from vault"
        );
        // tracing::info! is structured logging.
        // `%` means "format this with Display" (the normal human-readable format).
        // `?` would mean "format with Debug" (more verbose, for development).

        // ── Walk the vault tree ───────────────────────────────────────────────
        for entry in WalkDir::new(vault_root)
            .follow_links(true)
            // Follow symlinks — useful if parts of your vault are symlinked.

            .into_iter()
            // Consume the WalkDir into an iterator of Result<DirEntry>.
            // Each item is a Result because reading a directory can fail
            // (permissions, broken symlinks, etc.)

            .filter_map(|e| e.ok())
            // filter_map keeps only the Ok values and discards errors silently.
            // For a personal vault this is fine — a single unreadable directory
            // shouldn't abort the whole registry load.
            // Later we might want to log these errors instead of ignoring them.
        	{
            let path = entry.path();
            // entry.path() returns a &Path — a borrowed reference to the path.
            // We don't need to own it here, just inspect it.

            // Check if this entry is a file named exactly ".mailroom"
            if path.file_name().and_then(|n| n.to_str()) == Some(".mailroom") {
                // path.file_name() returns the last component of the path
                // as an Option<&OsStr>. It's an Option because the path
                // might end in ".." or be empty.
                //
                // .and_then(|n| n.to_str()) converts OsStr → &str,
                // returning None if the filename isn't valid UTF-8.
                // .and_then chains operations on Option — if any step
                // returns None, the whole expression is None.
                //
                // == Some(".mailroom") compares to the expected filename.
                // We wrap in Some() because file_name returns Option<&OsStr>.

                match load_manifest(path) {
                    Ok(manifest) => {
                        // Rust concept — match:
                        // match is exhaustive pattern matching — you must
                        // handle every possible case. Here: Ok and Err.
                        // It's like a switch statement but much more powerful.

                        let id = manifest.id.clone();
                        // .clone() makes a copy of the String.
                        // We need this because we're about to move `manifest`
                        // into the HashMap while also using `id` as the key.
                        // Rust's ownership rules don't allow using a value
                        // after it's been moved — clone sidesteps this.

                        tracing::debug!(
                            id   = %id,
                            path = %path.display(),
                            "loaded manifest"
                        );

                        entries.insert(id, manifest);
                        // Insert into the HashMap. If a manifest with this
                        // id already exists, it gets replaced — last one wins.
                        // This could happen if two .mailroom files claim the
                        // same JD address. We log it above so it's visible.
                    }

                    Err(e) => {
                        // Don't abort the whole registry load for one bad file.
                        // Log it as a warning and continue.
                        // The node simply won't be in the registry.
                        tracing::warn!(
                            path  = %path.display(),
                            error = %e,
                            "failed to parse .mailroom — skipping"
                        );
                    }
                }
            }
        }

        let node_count = entries.len();
        // .len() returns the number of entries in the HashMap.

        tracing::info!(
            node_count = node_count,
            "registry loaded"
        );

        Ok(Registry { entries, node_count })
        // Ok(...) wraps our Registry in the success variant of Result.
        // `entries` and `node_count` — Rust shorthand: when the field name
        // and variable name are the same, you can write just the name once.
        // This is equivalent to: Registry { entries: entries, node_count: node_count }
    }

	/// Create an empty registry — used when the vault doesn't exist yet.
	/// The server can boot and serve /health without a vault present.
	pub fn empty() -> Registry { 
		Registry {
			entries:    HashMap::new(),
			node_count: 0,
		}
	}

    /// Look up a JD address and return a reference to its Manifest.
    ///
    /// Returns None if the address isn't in the registry.
    /// The `&` means we return a borrowed reference — the Manifest stays
    /// owned by the Registry. The caller can read it but not take ownership.
    pub fn get(&self, id: &str) -> Option<&Manifest> {
        // &self means this method takes an immutable reference to the Registry.
        // It can read but not modify. This is the correct signature for a
        // lookup — we're not changing anything.
        self.entries.get(id)
        // HashMap::get returns Option<&V> — Some(&manifest) if found, None if not.
        // No `return` keyword needed — in Rust, the last expression in a
        // function is implicitly returned if there's no semicolon.
    }

    /// Return all manifests whose JD address starts with a given prefix.
    /// e.g. prefix "35" returns everything under 35_Health.
    pub fn get_area(&self, prefix: &str) -> Vec<&Manifest> {
        self.entries
            .values()
            // .values() iterates over all values in the HashMap (the Manifests).

            .filter(|m| m.id.starts_with(prefix))
            // Keep only manifests whose id starts with our prefix.
            // |m| is a closure — an anonymous function. m is each Manifest.

            .collect()
            // .collect() gathers the iterator into a Vec.
            // Rust needs to know what type to collect into — it infers
            // Vec<&Manifest> from our return type annotation above.
    }

    /// Return all manifests in the registry, sorted by JD address.
    pub fn all(&self) -> Vec<&Manifest> {
        let mut all: Vec<&Manifest> = self.entries.values().collect();
        all.sort_by(|a, b| a.id.cmp(&b.id));
        // sort_by takes a closure that compares two elements.
        // .cmp() is string comparison — returns Ordering::Less/Equal/Greater.
        // This gives us a stable alphabetical sort by JD address.
        all
    }
}

// ── Helper: load a single .mailroom file ──────────────────────────────────────

fn load_manifest(path: &Path) -> anyhow::Result<Manifest> {
    // This function is private (no `pub`) — only used inside this module.

    let contents = std::fs::read_to_string(path)?;
    // read_to_string reads the entire file into a String.
    // The ? operator: if this returns Err, we immediately return that error
    // from load_manifest, wrapped in anyhow::Error.
    // This is the idiomatic Rust alternative to try/catch.

    let manifest: Manifest = toml::from_str(&contents)?;
    // toml::from_str parses a TOML string into any type that implements
    // Deserialize — which our Manifest does, thanks to #[derive(Deserialize)].
    // Again, ? propagates any parse error up to the caller.

    Ok(manifest)
}