// src/orchard.rs
//
// Bootstraps a fresh Orchard: walks a schema tree and creates the
// physical directory + .mailroom file for every Station. This is a
// bootstrap-time concern only — once it runs, Registry::load() reads
// the result back exactly like any hand-written .mailroom file. There's
// no separate runtime "Orchard" type; Registry already is the index.

use std::path::Path;
use crate::manifest::Manifest;

/// One node in the schema tree. Wraps a Manifest (the same type Registry
/// parses) plus its children — Manifest itself has no notion of physical
/// nesting, only routing rules, so this wrapper carries that instead.
pub struct OrchardNode {
    pub manifest: Manifest,
    pub children: Vec<OrchardNode>,
}

pub fn create_orchard(schema: &[OrchardNode], vault_root: &Path) -> anyhow::Result<()> {
    for node in schema {
        create_station(node, vault_root)?;
    }
    Ok(())
}

fn create_station(node: &OrchardNode, parent_path: &Path) -> anyhow::Result<()> {
    let station_path = parent_path.join(node.manifest.effective_path());
    std::fs::create_dir_all(&station_path)?;

    let toml_text = toml::to_string_pretty(&node.manifest)?;
    std::fs::write(station_path.join(".mailroom"), toml_text)?;

    for child in &node.children {
        create_station(child, &station_path)?;
    }
    Ok(())
}