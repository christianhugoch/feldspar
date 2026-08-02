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
//! Eight traits over four things an agent can be given. **Tables**:
//! [`QueryTable`] reads one, and [`InsertRow`], [`UpdateRows`] and
//! [`DeleteRows`] are three separate opt-in grants over one — so a read-only
//! agent is the default shape and each way of changing data is a deliberate act
//! with a form field attached. **Actions**: [`RunTrigger`] exposes one configured
//! trigger, which is what connects an agent to the whole of §10 (and, once §10.3
//! lands, to workflows unchanged, because a workflow is a trigger). **Code**:
//! [`Coding`] is the whole loop over one configured file store (optionally
//! rooted at a sub-directory) — reading, listing and searching always, writing
//! and editing under one checkbox, and running a script the project's own
//! `package.json` declares under another, which is the bounded thing that ships
//! instead of a shell (decision 6) — and [`BuildApplication`] builds the
//! application whose source that store is and hands back its diagnostics. And
//! **the schema itself**: [`ManageTableAdmin`] describes and edits the catalog —
//! the first *app-building* trait, and the first that does not name a table in
//! its configuration, because the tables it makes do not exist when it is
//! configured.
//!
//! ## And one thing that is not a trait
//!
//! [`RunAgent`] is an `Action`, not an [`AgentTrait`](sc_agent::AgentTrait): it
//! is how a **trigger runs an agent** (§11.5), the other direction from
//! [`RunTrigger`]. It is in this crate for the same reason everything else here
//! is — it drives a loop whose tools reach the row layer — and it is registered
//! through [`register_agent_actions`] rather than with `sc-core-actions`' set,
//! because it needs two things assembled first that no other action does: the
//! trait registry the agents were validated against, and how this deployment
//! connects a provider.
//!
//! ## What every trait here has in common
//!
//! - **It names its target in its configuration.** There is no trait that can
//!   reach *any* table, because "which tables may this agent see?" is the first
//!   question an admin needs to be able to answer off the agent's definition.
//!   **[`ManageTableAdmin`] is the one exception**, and a deliberate one: a
//!   trait that creates tables cannot name them in advance, so it is scoped by
//!   *what it may do* — four grants — rather than by what it may reach, and it
//!   refuses any caller who is not an admin (§11.3).
//! - **Its tool names are derived from that configuration** (`query_books`, not
//!   `query`), so one trait enabled twice offers two distinguishable tools —
//!   which is what makes a collision refusable on save (§11.2). See
//!   [`tool_names`]. `ManageTableAdmin`'s two names are fixed for the same
//!   reason it names no table; enabling it twice therefore *collides*, which is
//!   the intended outcome.
//! - **A trait may offer several tools, and may withhold some of them.**
//!   [`Coding`] offers six over one scope and declares only the ones its grants
//!   allow, which is how "may this agent change the source?" became a checkbox
//!   rather than a second trait with the same form on it.
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

mod build_application;
mod coding;
mod delete_rows;
mod files;
mod insert_row;
mod manage_table_admin;
mod query_table;
mod run_agent;
mod run_trigger;
mod table;
mod update_rows;
mod write;

use std::sync::Arc;

use sc_action::ActionRegistry;
use sc_agent::{AgentRegistry, ProviderConnector};
use sc_error::Result;

pub use table::{CFG_FIELDS, CFG_MAX_ROWS, CFG_TABLE};

pub use files::{CFG_ROOT, CFG_STORE, FileScope, configured_scope, slugify};

pub use build_application::{BuildApplication, CFG_APPLICATION};
pub use coding::{
    CFG_MAX_CHARS, CFG_MAX_RESULTS, CFG_MAY_EDIT, CFG_MAY_RUN_SCRIPTS, CFG_TIMEOUT, Coding,
    DEFAULT_MAX_CHARS, DEFAULT_TIMEOUT_SECONDS, MAX_OUTPUT_CHARS,
};
pub use delete_rows::DeleteRows;
pub use insert_row::InsertRow;
pub use manage_table_admin::{
    CFG_ALLOW_ACCESS, CFG_ALLOW_CREATE, CFG_ALLOW_DROP, CFG_ALLOW_EDIT, ManageTableAdmin,
    TOOL_DESCRIBE, TOOL_EDIT,
};
pub use query_table::{DEFAULT_MAX_ROWS, QueryTable};
pub use run_agent::{CFG_AGENT, CFG_PROMPT, RunAgent};
pub use run_trigger::{CFG_TRIGGER, RunTrigger};
pub use update_rows::{DEFAULT_MAX_WRITE_ROWS, UpdateRows};

/// What each built-in trait calls the tool it derives from its configuration —
/// the answer to "what will this be called?" the admin UI wants before an agent
/// is saved and the collision check (§11.2) wants at the moment of saving.
pub mod tool_names {
    pub use crate::build_application::tool_name as build_application;
    pub use crate::coding::tool_names as coding;
    pub use crate::coding::{
        edit_file_tool_name as edit_file, list_files_tool_name as list_files,
        read_file_tool_name as read_file, run_script_tool_name as run_project_script,
        search_files_tool_name as search_files, write_file_tool_name as write_file,
    };
    pub use crate::delete_rows::tool_name as delete_rows;
    pub use crate::insert_row::tool_name as insert_row;
    pub use crate::manage_table_admin::tool_names as manage_table_admin;
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
/// Register the actions that **run** an agent, rather than the traits an agent
/// runs (§11.5).
///
/// There is exactly one — [`RunAgent`] — and it lives in this crate for the
/// reason the traits do: it drives a loop whose tools reach the row layer. It is
/// registered separately from `sc-core-actions`' built-in set because it needs
/// two things assembled first: the trait registry the agents were validated
/// against, and how this deployment connects a provider. A server calls
/// [`builtin_traits`], puts the result in its agent services, and passes both
/// here while building the action registry the trigger dispatcher will hold.
pub fn register_agent_actions(
    registry: &mut ActionRegistry,
    traits: Arc<AgentRegistry>,
    providers: Arc<dyn ProviderConnector>,
) -> Result<()> {
    registry.register(Arc::new(RunAgent::new(traits, providers)))
}

pub fn register_builtin_traits(registry: &mut AgentRegistry) -> Result<()> {
    registry.register(Arc::new(QueryTable))?;
    registry.register(Arc::new(InsertRow))?;
    registry.register(Arc::new(UpdateRows))?;
    registry.register(Arc::new(DeleteRows))?;
    registry.register(Arc::new(RunTrigger))?;
    registry.register(Arc::new(Coding))?;
    registry.register(Arc::new(BuildApplication))?;
    registry.register(Arc::new(ManageTableAdmin))?;
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
                "build_application",
                "coding",
                "delete_rows",
                "insert_row",
                "manage_table_admin",
                "query_table",
                "run_trigger",
                "update_rows",
            ]
        );
        // Every one of them describes itself and its configuration as data,
        // which is what lets the admin UI render a form for a trait it has never
        // heard of — and every one that names a *target* names it as required, so
        // a blank form cannot be saved. `coding` names one too (its store); what
        // its blank checkboxes then decide is what it may *do* there.
        //
        // `manage_table_admin` is the exception, and the reason is the phase's
        // point: it names no table, because the tables it makes do not exist when
        // it is configured. Its form is four grants, each with a default, and a
        // blank one is a meaningful (read-only) configuration rather than an
        // incomplete one.
        for trait_ in registry.all() {
            assert!(!trait_.description().is_empty(), "{}", trait_.name());
            let spec = trait_.config_spec();
            assert!(!spec.is_empty(), "{}", trait_.name());
            if trait_.name() != "manage_table_admin" {
                assert!(spec.iter().any(|f| f.required), "{}", trait_.name());
            }
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

        // The whole coding loop is **one** form: the scope filled in once — one
        // store, optionally one directory in it — then what the agent may do
        // there, then the bound on each thing that brings something back. Six
        // tools, one place to say where they work, so the scope cannot disagree
        // with itself.
        assert_eq!(
            spec("coding"),
            vec![
                CFG_STORE,
                CFG_ROOT,
                CFG_MAY_EDIT,
                CFG_MAY_RUN_SCRIPTS,
                CFG_MAX_CHARS,
                CFG_MAX_RESULTS,
                CFG_TIMEOUT
            ]
        );
        // The build names an application rather than a store: which store the
        // source is in is the application's own configuration (§13.3), and
        // asking the admin for it twice would be two places to get it wrong.
        assert_eq!(spec("build_application"), vec![CFG_APPLICATION]);
        // The trait that names no table: four grants, scoping it by what it may
        // do rather than by what it may reach (§11.3).
        assert_eq!(
            spec("manage_table_admin"),
            vec![
                CFG_ALLOW_CREATE,
                CFG_ALLOW_EDIT,
                CFG_ALLOW_DROP,
                CFG_ALLOW_ACCESS
            ]
        );
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
        let scope = FileScope {
            store: "app-src".to_owned(),
            root: "web".to_owned(),
        };
        let names = [
            tool_names::query_table("books"),
            tool_names::insert_row("books"),
            tool_names::update_rows("books"),
            tool_names::delete_rows("books"),
            tool_names::run_trigger("reindex"),
            tool_names::read_file(&scope),
            tool_names::write_file(&scope),
            tool_names::list_files(&scope),
            tool_names::edit_file(&scope),
            tool_names::search_files(&scope),
            tool_names::run_project_script(&scope),
            tool_names::build_application("todo"),
        ];
        assert_eq!(
            names,
            [
                "query_books",
                "insert_into_books",
                "update_books",
                "delete_from_books",
                "run_reindex",
                "read_file_app_src_web",
                "write_file_app_src_web",
                "list_files_app_src_web",
                "edit_file_app_src_web",
                "search_files_app_src_web",
                "run_script_app_src_web",
                "build_todo",
            ]
        );
        let unique: std::collections::BTreeSet<&String> = names.iter().collect();
        assert_eq!(unique.len(), names.len());
        // The six file names above are `coding`'s whole set, which is what the
        // collision check compares when the trait is enabled twice: two
        // instances over one scope produce these same six and are refused.
        assert_eq!(
            tool_names::coding(&scope),
            [
                tool_names::read_file(&scope),
                tool_names::list_files(&scope),
                tool_names::search_files(&scope),
                tool_names::write_file(&scope),
                tool_names::edit_file(&scope),
                tool_names::run_project_script(&scope),
            ]
        );
        // `manage_table_admin`'s names are fixed rather than derived, and say
        // the same two things every deployment's do.
        assert_eq!(
            tool_names::manage_table_admin(),
            ["describe_schema", "edit_schema"]
        );
        for name in tool_names::manage_table_admin() {
            assert!(!names.iter().any(|n| n == name));
        }
    }
}
