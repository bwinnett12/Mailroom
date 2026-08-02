use std::path::Path;
use serde::Deserialize;

use crate::manifest::{Manifest, NodeKind};
use crate::orchard::Orchard;
use crate::nest::Nest;

#[derive(Debug, Deserialize)]
pub struct SchemaNode {
    pub code: String,
    pub name: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub children: Vec<SchemaNode>,
}

pub fn load_schema(path: &Path) -> anyhow::Result<Vec<SchemaNode>> {
    let text = std::fs::read_to_string(path)?;
    let nodes: Vec<SchemaNode> = ron::from_str(&text)?;
    Ok(nodes)
}

fn schema_to_orchard(node: &SchemaNode) -> Nest {
    let manifest = Manifest {
        id: node.code.clone(),
        name: node.name.clone(),
        path: None,
        kind: NodeKind::Inferred,
        accepts: Vec::new(),
        store: None,
        routing: None,
        library: None,
        mailroom: None,
        about: None,
        known_tags: Vec::new(), 
        call_number: None,
    };
    Nest {
        manifest,
        children: node.children.iter().map(schema_to_orchard).collect(),
    }
}

pub fn generate_fresh_tree(schema_path: &Path, vault_root: &Path) -> anyhow::Result<()> {
    let schema_nodes = load_schema(schema_path)?;
    let orchard_nodes: Vec<Nest> = schema_nodes.iter().map(schema_to_orchard).collect();
    Orchard::new(orchard_nodes).materialize(vault_root)
}