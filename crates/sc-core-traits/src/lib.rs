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
//! (`sc_api::read_rows_as`), and a write is the same write, under the same rule's
//! other half (`sc_api::insert_row_as` and its siblings), with the same coercion,
//! the same rich-type and `File`-field validation and the same emitted events.
//! That fixes these *above* layer 8, while `sc-agent` (layer 7) stays what a
//! plugin needs to write a trait of its own: the
//! [`AgentTrait`](sc_agent::AgentTrait) seam, and nothing that knows which traits
//! exist.
//!
//! `sc-agent` therefore registers **nothing**, and this crate is the first
//! consumer of that seam rather than a privileged one.
//!
//! ## The set
//!
//! Five traits over two things an agent can be given. **Tables**: [`QueryTable`]
//! reads one, and [`InsertRow`], [`UpdateRows`] and [`DeleteRows`] are three
//! separate opt-in grants over one — so a read-only agent is the default shape
//! and each way of changing data is a deliberate act with a form field attached.
//! **Actions**: [`RunTrigger`] exposes one configured trigger, which is what
//! connects an agent to the whole of §10 (and, once §10.3 lands, to workflows
//! unchanged, because a workflow is a trigger).
//!
//! ## What every trait here has in common
//!
//! - **It names its target in its configuration.** There is no trait that can
//!   reach *any* table, because "which tables may this agent see?" is the first
//!   question an admin needs to be able to answer off the agent's definition.
//! - **Its tool names are derived from that configuration** (`query_books`, not
//!   `query`), so one trait enabled twice offers two distinguishable tools —
//!   which is what makes a collision refusable on save (§11.2). See
//!   [`tool_names`].
//! - **Its tool is described by what it is configured against**: the table's own
//!   fields, with their types, in the description *and* in the JSON schema. A
//!   model left to guess a column name will guess, and the guess costs a turn.
//! - **It runs as the run's caller** ([`RunCaller`](sc_agent::RunCaller)), never
//!   as the server. An agent is not a way around ownership or row-level
//!   security: the same table read by two callers gives two answers, and a write
//!   reaches only the rows that caller could have been shown.
//! - **Everything it refuses, it refuses by name**, listing the alternatives
//!   where there are any. These errors are read by a model that can only recover
//!   if it is told, so they are written for that reader.
//! - **Its `validate_config` checks what the spec cannot** — that the table
//!   exists, that it is addressable by primary key, that a named field is real
//!   and writable, that the trigger exists — on save *and* on load, so an agent
//!   whose world changed underneath it leaves the live set with a reason instead
//!   of failing mid-conversation.

mod delete_rows;
mod insert_row;
mod query_table;
mod run_trigger;
mod table;
mod update_rows;
mod write;

use std::sync::Arc;

use sc_agent::AgentRegistry;
use sc_error::Result;

pub use table::{CFG_FIELDS, CFG_MAX_ROWS, CFG_TABLE};

pub use delete_rows::DeleteRows;
pub use insert_row::InsertRow;
pub use query_table::{DEFAULT_MAX_ROWS, QueryTable};
pub use run_trigger::{CFG_TRIGGER, RunTrigger};
pub use update_rows::{DEFAULT_MAX_WRITE_ROWS, UpdateRows};

/// What each built-in trait calls the tool it derives from its configuration —
/// the answer to "what will this be called?" the admin UI wants before an agent
/// is saved and the collision check (§11.2) wants at the moment of saving.
pub mod tool_names {
    pub use crate::delete_rows::tool_name as delete_rows;
    pub use crate::insert_row::tool_name as insert_row;
    pub use crate::query_table::tool_name as query_table;
    pub use crate::run_trigger::tool_name as run_trigger;
    pub use crate::update_rows::tool_name as update_rows;
}

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
    registry.register(Arc::new(InsertRow))?;
    registry.register(Arc::new(UpdateRows))?;
    registry.register(Arc::new(DeleteRows))?;
    registry.register(Arc::new(RunTrigger))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtins_are_registered_under_their_stored_names() {
        let registry = builtin_traits().unwrap();
        assert_eq!(
            registry.names(),
            vec![
                "delete_rows",
                "insert_row",
                "query_table",
                "run_trigger",
                "update_rows",
            ]
        );
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
        let spec = |name: &str| -> Vec<String> {
            registry
                .require(name)
                .unwrap()
                .config_spec()
                .iter()
                .map(|f| f.name().to_owned())
                .collect()
        };
        assert_eq!(
            spec("query_table"),
            vec![CFG_TABLE, CFG_FIELDS, CFG_MAX_ROWS]
        );
        // An insert has no row bound to declare — it writes one row — and a
        // delete has no field allow-list, because it takes the whole row and a
        // setting that narrowed nothing would suggest a grant that does not
        // exist.
        assert_eq!(spec("insert_row"), vec![CFG_TABLE, CFG_FIELDS]);
        assert_eq!(
            spec("update_rows"),
            vec![CFG_TABLE, CFG_FIELDS, CFG_MAX_ROWS]
        );
        assert_eq!(spec("delete_rows"), vec![CFG_TABLE, CFG_MAX_ROWS]);
        assert_eq!(spec("run_trigger"), vec![CFG_TRIGGER]);
    }

    /// Every tool name a built-in derives carries **what it does** and **what it
    /// does it to**, and no two traits over one table collide.
    ///
    /// Worth pinning: these are the names the model chooses between, and the
    /// collision check (§11.2) refuses a save that produces two of the same. If
    /// `insert_row` and `update_rows` over `books` both derived `books_write`,
    /// an agent could not have both.
    #[test]
    fn the_derived_tool_names_are_distinct_and_say_what_they_do() {
        let names = [
            tool_names::query_table("books"),
            tool_names::insert_row("books"),
            tool_names::update_rows("books"),
            tool_names::delete_rows("books"),
            tool_names::run_trigger("reindex"),
        ];
        assert_eq!(
            names,
            [
                "query_books",
                "insert_into_books",
                "update_books",
                "delete_from_books",
                "run_reindex",
            ]
        );
        let unique: std::collections::BTreeSet<&String> = names.iter().collect();
        assert_eq!(unique.len(), names.len());
    }
}
