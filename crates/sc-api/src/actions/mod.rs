//! The built-in actions (design §10.1, TODO Phase 3).
//!
//! Four actions — [`InsertRow`], [`UpdateRows`], [`DeleteRows`], [`Fetch`] — and
//! deliberately few: GOALS asks for a minimal built-in set, and control flow
//! belongs to the workflow engine (§10.3), not to a proliferation of actions.
//! Every configuration value that reads the event is a **formula** in the same
//! `sc-expr` language as an ownership rule and a trigger's `only_if`, under one
//! scope rule; [`scope`] holds that, and every action's configuration goes through
//! its parsers so there is one answer per question rather than one per action.
//!
//! ## Why these live here and not in `sc-action`
//!
//! An action that writes a row must write it through the **`rows` layer**, so the
//! target table's type coercion, rich-type attributes, `File`-field validation
//! and — from Phase 4 — its own triggers all apply. That layer is this crate, and
//! the [`Action`](sc_action::Action) trait is defined below it, so the actions
//! that need it are implemented *here* and registered into the same registry as
//! everything else. Nothing in `sc-action` names this crate in return: a write
//! reaches a trigger through the seam the catalog holds, never through a
//! dependency, which is what keeps the trigger machinery at layer 6.
//!
//! **`fetch` joined them** (revising the note this module carried when it held
//! only the row actions). It needs no rows — but it does need the same
//! configuration-and-scope machinery [`scope`] holds, and that machinery is here
//! because *the row actions* need it in a form that knows column types. Putting
//! `fetch` in `sc-action` would have meant either a second copy of it or pushing
//! the JSON↔`Value` coercions down two layers to serve one caller. One home for
//! the built-ins, one [`builtin_actions`] constructor, is the better trade.

mod delete_rows;
mod fetch;
mod insert_row;
mod scope;
mod update_rows;

use std::sync::Arc;

use sc_action::ActionRegistry;
use sc_error::Result;

pub use delete_rows::DeleteRows;
pub use fetch::Fetch;
pub use insert_row::InsertRow;
pub use update_rows::UpdateRows;

/// The built-in action set a server installs: `sc-action`'s own actions plus the
/// ones defined here.
///
/// One constructor, so a deployment cannot end up with a registry that is missing
/// half the built-ins depending on which crate it remembered to ask. Fallible
/// because [`Fetch`] builds an HTTP client (and therefore a TLS stack), which a
/// deployment should hear about at boot rather than at the first firing.
pub fn builtin_actions() -> Result<ActionRegistry> {
    let mut registry = ActionRegistry::builtin();
    register_row_actions(&mut registry)?;
    registry.register(Arc::new(Fetch::new()?))?;
    Ok(registry)
}

/// Add the row-writing actions to an existing registry — for a deployment (or a
/// test) that assembles its own set.
///
/// Fails if one of the names is already taken, as any duplicate registration
/// does: which implementation answers to `insert_row` must not depend on load
/// order.
pub fn register_row_actions(registry: &mut ActionRegistry) -> Result<()> {
    registry.register(Arc::new(InsertRow))?;
    registry.register(Arc::new(UpdateRows))?;
    registry.register(Arc::new(DeleteRows))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtins_are_registered_under_their_stored_names() {
        let registry = builtin_actions().unwrap();
        assert_eq!(
            registry.names(),
            vec!["delete_rows", "fetch", "insert_row", "update_rows"]
        );
        // Every one of them describes itself and its configuration as data, which
        // is what lets the admin UI render a form it has never heard of — and
        // every one names something required, so a blank form cannot be saved.
        for action in registry.all() {
            assert!(!action.description().is_empty(), "{}", action.name());
            let spec = action.config_spec();
            assert!(!spec.is_empty(), "{}", action.name());
            assert!(spec.iter().any(|f| f.required), "{}", action.name());
        }
    }

    #[test]
    fn registering_the_row_actions_twice_is_refused() {
        let mut registry = builtin_actions().unwrap();
        let err = register_row_actions(&mut registry).unwrap_err();
        assert!(err.to_string().contains("insert_row"), "{err}");
    }

    #[test]
    fn each_action_declares_the_settings_its_semantics_need() {
        let registry = builtin_actions().unwrap();
        let names = |action: &str| -> Vec<String> {
            registry
                .require(action)
                .unwrap()
                .config_spec()
                .iter()
                .map(|f| f.name().to_owned())
                .collect()
        };
        assert_eq!(names("insert_row"), vec!["table", "values"]);
        assert_eq!(names("update_rows"), vec!["table", "where", "assignments"]);
        assert_eq!(names("delete_rows"), vec!["table", "where"]);
        assert_eq!(
            names("fetch"),
            vec!["url", "method", "headers", "body", "timeout_ms"]
        );
    }
}
