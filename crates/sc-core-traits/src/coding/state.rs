//! What `coding` remembers across one run's tool calls (TODO §6): which version
//! of each file the model last saw, what each touched file was before the run
//! touched it, and which files this model turn edited.
//!
//! It lives in the loop's per-run trait state (`TraitContext::state`), so it is
//! saved with the run after every step and restored on resume. It is JSON there,
//! and this module is the only code that knows its shape.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use sha2::{Digest, Sha256};

use super::check::Baseline;
use super::ledger::Ledger;

/// The trait's state for one run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CodingState {
    /// The content hash of each file as the model last saw it, by store path:
    /// from its last read, or from the run's own last write.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub seen: BTreeMap<String, String>,
    /// The pre-image of every file the run touched.
    #[serde(default, skip_serializing_if = "Ledger::is_empty")]
    pub ledger: Ledger,
    /// Files edited since the last post-turn feedback, by store path, in the
    /// order they were first edited.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub turn_edits: Vec<String>,
    /// Each check's result before the run changed anything, by check name
    /// (TODO 6.4).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub baseline: BTreeMap<String, Baseline>,
}

impl CodingState {
    /// The state from the loop's JSON. A value this code cannot read is a
    /// fresh state: it only ever holds what this module wrote.
    pub fn load(json: &Json) -> CodingState {
        match json {
            Json::Null => CodingState::default(),
            other => serde_json::from_value(other.clone()).unwrap_or_default(),
        }
    }

    /// Write the state back into the loop's JSON.
    pub fn store(&self, json: &mut Json) {
        *json = serde_json::to_value(self).unwrap_or(Json::Null);
    }

    /// Record that the model has seen `bytes` at `path`.
    pub fn saw(&mut self, path: &str, bytes: &[u8]) {
        self.seen.insert(path.to_owned(), content_hash(bytes));
    }

    /// Record that the file at `path` is gone.
    pub fn forget(&mut self, path: &str) {
        self.seen.remove(path);
    }

    /// Note an edit for the post-turn feedback.
    pub fn edited(&mut self, path: &str) {
        if !self.turn_edits.iter().any(|p| p == path) {
            self.turn_edits.push(path.to_owned());
        }
    }
}

/// Why a file may not be changed yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stale {
    /// The run never read it.
    Unread,
    /// It changed since the run last read or wrote it.
    Changed,
}

impl CodingState {
    /// Whether the model's view of an existing file is current.
    pub fn check_current(&self, path: &str, bytes: &[u8]) -> Result<(), Stale> {
        match self.seen.get(path) {
            None => Err(Stale::Unread),
            Some(hash) if *hash != content_hash(bytes) => Err(Stale::Changed),
            Some(_) => Ok(()),
        }
    }
}

/// The refusal for a stale or unread file, naming the read tool.
pub fn stale_message(stale: Stale, rel: &str, read_tool: &str) -> String {
    match stale {
        Stale::Unread => format!(
            "`{rel}` exists and has not been read in this run. Call `{read_tool}` on it \
             first, then make the change."
        ),
        Stale::Changed => format!(
            "`{rel}` has changed since it was last read. Call `{read_tool}` on it again, \
             then make the change against what is there now."
        ),
    }
}

/// A file's content hash: SHA-256, hex.
pub fn content_hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_survives_its_json_round_trip() {
        let mut state = CodingState::default();
        state.saw("web/a.ts", b"one");
        state.edited("web/a.ts");
        state.edited("web/a.ts");
        let mut json = Json::Null;
        state.store(&mut json);
        let back = CodingState::load(&json);
        assert_eq!(back, state);
        assert_eq!(back.turn_edits, vec!["web/a.ts"]);
        // The loop's initial `null` is a fresh state.
        assert_eq!(CodingState::load(&Json::Null), CodingState::default());
    }

    #[test]
    fn a_file_is_current_only_while_its_hash_matches() {
        let mut state = CodingState::default();
        assert_eq!(state.check_current("a", b"x"), Err(Stale::Unread));
        state.saw("a", b"x");
        assert_eq!(state.check_current("a", b"x"), Ok(()));
        assert_eq!(state.check_current("a", b"y"), Err(Stale::Changed));
    }
}
