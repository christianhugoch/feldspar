//! The core built-in agent traits (layer 9; technical design §11.3, TODO Phase 3).
//!
//! Every trait Saltcorn ships an agent with, in one crate — the counterpart of
//! `sc-core-actions` and, deliberately, at the same layer for the same reason.
//! [`builtin_traits`] is the single constructor that assembles the set.
//!
//! ## Why a crate of its own, above the row layer
//!
//! A trait that touches rows must go through **`sc-api`**, not around it: a read
//! is the same read an API caller makes, under the same §7.3 access rule
//! (`sc_api::read_rows_as`), and a write — the later items of Phase 3 — is the
//! same write, with the same coercion, the same rich-type and `File`-field
//! validation and the same emitted events. That fixes these *above* layer 8,
//! while `sc-agent` (layer 7) stays what a plugin needs to write a trait of its
//! own: the [`AgentTrait`](sc_agent::AgentTrait) seam, and nothing that knows
//! which traits exist.
//!
//! `sc-agent` therefore registers **nothing**, and this crate is the first
//! consumer of that seam rather than a privileged one.
//!
//! ## What every trait here has in common
//!
//! - **It names its target in its configuration.** There is no trait that can
//!   reach *any* table, because "which tables may this agent see?" is the first
//!   question an admin needs to be able to answer off the agent's definition.
//! - **Its tool names are derived from that configuration** (`query_books`, not
//!   `query`), so one trait enabled twice offers two distinguishable tools —
//!   which is what makes a collision refusable on save (§11.2).
//! - **It runs as the run's caller** ([`RunCaller`](sc_agent::RunCaller)), never
//!   as the server. An agent is not a way around ownership or row-level
//!   security: the same table read by two callers gives two answers.
//! - **Its `validate_config` checks what the spec cannot** — that the table
//!   exists, that it is addressable by primary key, that a named field is real —
//!   on save *and* on load, so an agent whose world changed underneath it leaves
//!   the live set with a reason instead of failing mid-conversation.

mod query_table;

use std::sync::Arc;

use sc_agent::AgentRegistry;
use sc_error::Result;

pub use query_table::{
    CFG_FIELDS, CFG_MAX_ROWS, CFG_TABLE, DEFAULT_MAX_ROWS, QueryTable, tool_name,
};

/// The built-in trait set a server installs.
///
/// One constructor, so a deployment cannot end up with half the built-ins
/// depending on what it remembered to register.
pub fn builtin_traits() -> Result<AgentRegistry> {
    let mut registry = AgentRegistry::new();
    register_builtin_traits(&mut registry)?;
    Ok(registry)
}

/// Add the built-in traits to an existing registry — for a deployment (or a
/// test) that assembles its own set from these plus its plugins'.
///
/// Fails if one of the names is already taken, as any duplicate registration
/// does: which implementation answers to `query_table` must not depend on load
/// order.
pub fn register_builtin_traits(registry: &mut AgentRegistry) -> Result<()> {
    registry.register(Arc::new(QueryTable))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtins_are_registered_under_their_stored_names() {
        let registry = builtin_traits().unwrap();
        assert_eq!(registry.names(), vec!["query_table"]);
        // Every one of them describes itself and its configuration as data,
        // which is what lets the admin UI render a form for a trait it has never
        // heard of — and every one names something required, so a blank form
        // cannot be saved.
        for trait_ in registry.all() {
            assert!(!trait_.description().is_empty(), "{}", trait_.name());
            let spec = trait_.config_spec();
            assert!(!spec.is_empty(), "{}", trait_.name());
            assert!(spec.iter().any(|f| f.required), "{}", trait_.name());
        }
    }

    #[test]
    fn registering_the_builtins_twice_is_refused() {
        let mut registry = builtin_traits().unwrap();
        let err = register_builtin_traits(&mut registry).unwrap_err();
        assert!(err.to_string().contains("query_table"), "{err}");
    }

    #[test]
    fn each_trait_declares_the_settings_its_semantics_need() {
        let registry = builtin_traits().unwrap();
        let names: Vec<String> = registry
            .require("query_table")
            .unwrap()
            .config_spec()
            .iter()
            .map(|f| f.name().to_owned())
            .collect();
        assert_eq!(names, vec![CFG_TABLE, CFG_FIELDS, CFG_MAX_ROWS]);
    }
}
