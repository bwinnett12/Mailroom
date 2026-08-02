// src/orchard.rs
//
// Bootstraps a fresh Orchard: walks a schema tree and creates the
// physical directory + nest file for every Station. This is a
// bootstrap-time concern only — once it runs, Registry::load() reads
// the result back exactly like any hand-written nest file.
//
// "Fresh trees" policy: an existing nest file always wins outright — see
// create_station below.

use std::path::Path;
use crate::nest::Nest;

/// A collection of Nests — the in-memory shape of an Orchard's schema
/// tree. Owns its Nests outright (Vec<Nest>, not references), since this
/// tree is built fresh from parsed schema data with nothing else to
/// borrow from.
pub struct Orchard {
    pub nests: Vec<Nest>,
}

impl Orchard {
    pub fn new(nests: Vec<Nest>) -> Self {
        Orchard { nests }
    }

    /// Materializes this Orchard onto disk under `vault_root`: creates the
    /// physical directory + nest file for every Station. `&self` — this
    /// only needs to read the tree, not consume it, so nothing is moved
    /// out of `self.nests`.
    pub fn materialize(&self, vault_root: &Path) -> anyhow::Result<()> {
        for node in &self.nests {
            create_station(node, vault_root)?;
        }
        Ok(())
    }
}

pub fn create_station(node: &Nest, parent_path: &Path) -> anyhow::Result<()> {
    let station_path = parent_path.join(node.manifest.effective_path());
    std::fs::create_dir_all(&station_path)?;

    let nest_path = station_path.join("nest");
    if nest_path.exists() {
        tracing::info!(path = %nest_path.display(), "nest already exists — leaving in place");
    } else {
        let toml_text = toml::to_string_pretty(&node.manifest)?;
        std::fs::write(&nest_path, toml_text)?;
        tracing::info!(path = %nest_path.display(), "created fresh nest");
    }

    for child in &node.children {
        create_station(child, &station_path)?;
    }
    Ok(())
}