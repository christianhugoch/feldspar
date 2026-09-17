//! `sc-repomap` — a map of a source tree for a coding agent (TODO §11, R§8).
//!
//! A port of Aider's repo map. Given the files of a project and what the agent
//! is working on, it answers "what in this repository matters here, and what
//! does it look like?" in a few hundred tokens:
//!
//! 1. **Tags** ([`tags`]): tree-sitter finds what each file defines and refers
//!    to, through vendored `tags.scm` queries for TypeScript, TSX, JavaScript
//!    and Python. Other files are entries by name.
//! 2. **Graph and ranking** ([`rank`]): a file that refers to a name links to
//!    the files defining it. PageRank over those links, personalised towards the
//!    files and names in play, ranks every definition.
//! 3. **Rendering** ([`render`]): the ranked definitions as `path` headers with
//!    `line│ signature` rows, and a binary search for the most of them that fit a
//!    token budget.
//!
//! The crate reads no store: the caller hands it paths and bytes. Its tag cache
//! ([`TagCache`]) is keyed by content hash, so the caller can keep one per store
//! and rebuild a map cheaply after an edit.
//!
//! The grammars compile C, so they sit behind the default `grammars` feature.

pub mod rank;
pub mod render;
pub mod tags;

pub use rank::{Focus, Ranked, rank};
pub use render::{DEFAULT_TOKENS, estimate_tokens, fit, render_entries};
pub use tags::{Language, MAX_PARSED_BYTES, Tag, TagCache, TagKind, extract};

use std::sync::Arc;

/// One file of the tree: its path (relative to the map's root) and its tags.
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub path: String,
    pub tags: Arc<Vec<Tag>>,
}

/// The map of `files`, focused on `focus`, fitted into `tokens`.
pub fn repo_map(files: &[SourceFile], focus: &Focus, tokens: usize) -> String {
    fit(&rank(files, focus), tokens, estimate_tokens)
}
