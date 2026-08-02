// src/dodo.rs

use crate::nest::Nest;
use crate::route::Route;
use crate::manifest::Manifest;

/// One node in the schema tree. Wraps a Manifest (the same type Registry
/// parses) plus its children — Manifest itself has no notion of physical
/// nesting, only routing rules, so this wrapper carries that instead.
pub struct Dodo {

	pub top_nest: Nest,
	pub available_nests: Vec<Nest>,
	pub current_route: Route,

}

impl Dodo {

	pub fn get_home_nest(&self) -> &Nest {
		return(&self.top_nest)
	}

}