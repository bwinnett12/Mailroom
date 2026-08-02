// src/nest.rs

use crate::manifest::Manifest;

/// One node in the schema tree. Wraps a Manifest (the same type Registry
/// parses) plus its children — Manifest itself has no notion of physical
/// nesting, only routing rules, so this wrapper carries that instead.
#[derive(Debug, Clone)]
pub struct Nest {
    pub manifest: Manifest,
    pub children: Vec<Nest>,
}