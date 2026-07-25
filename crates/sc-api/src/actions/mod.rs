//! The elementary row-writing actions (design §10.1, TODO Phase 3).
//!
//! Three actions — [`InsertRow`], [`UpdateRows`], [`DeleteRows`] — and
//! deliberately no more: GOALS asks for a minimal built-in set, and control flow
//! belongs to the workflow engine (§10.3), not to a proliferation of actions.
//! Every configuration value that reads the event is a **formula** in the same
//! `sc-expr` language as an ownership rule and a trigger's `only_if`, under one
//! scope rule; [`scope`] holds both.
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
//! Actions that need neither the catalog's rows nor its shape (`fetch`,
//! `run_js_code`) have no such pull upwards and belong in `sc-action`;
//! [`builtin_actions`] is where the two halves meet.

mod delete_rows;
mod insert_row;
mod scope;
mod update_rows;

use std::sync::Arc;

use sc_action::ActionRegistry;
use sc_error::Result;

pub use delete_rows::DeleteRows;
pub use insert_row::InsertRow;
pub use update_rows::UpdateRows;

/// The built-in action set a server installs: `sc-action`'s own actions plus the
/// row-writing ones defined here.
///
/// One constructor, so a deployment cannot end up with a registry that is missing
/// half the built-ins depending on which crate it remembered to ask.
pub fn builtin_actions() -> Result<ActionRegistry> {
    let mut registry = ActionRegistry::builtin();
    register_row_actions(&mut registry)?;
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
            vec!["delete_rows", "insert_row", "update_rows"]
        );
        // Every one of them describes itself and its configuration as data, which
        // is what lets the admin UI render a form it has never heard of.
        for action in registry.all() {
            assert!(!action.description().is_empty(), "{}", action.name());
            let spec = action.config_spec();
            assert_eq!(spec.first().map(|f| f.name()), Some(scope::CFG_TABLE));
            assert!(spec.iter().all(|f| f.required), "{}", action.name());
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
    }
}
