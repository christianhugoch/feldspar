//! What a workflow's formulas may name (design §10.3, decision 8).
//!
//! One function, [`workflow_shape`], and it is the **only** place a step's scope
//! is decided — for the reason `action_shape` is the only place an action's is:
//! a formula accepted on save and then unbound when the step runs would be the
//! worst of both, and two functions that answer "what is in scope" eventually
//! answer differently.
//!
//! It is **`sc-action`'s** [`step_shape`], named here rather than reimplemented,
//! and that is the point: a step's own formulas (a `Set`, a branch guard, a
//! loop's collection) and the settings of the action it runs are the same
//! language read in the same place, so the engine and the action have to be
//! given one answer, not two that agree today.
//!
//! A step sees everything an action's configuration sees — the catalog's tables,
//! the event's `row`/`old`/`user`/`payload`, and [`EVENT_SCOPE`] as the table a
//! formula that ranges over nothing is validated in — plus one thing: the
//! ambient **`context`**, which is the run.

use sc_action::{EVENT_SCOPE, step_shape};
use sc_catalog::Catalog;
use sc_error::Result;
use sc_expr::SchemaShape;

/// The scope a workflow step's formulas are validated and evaluated in:
/// [`step_shape`] — an action's own scope plus the ambient `context`.
///
/// `channel` is the trigger's — the table for a table event, `None` otherwise —
/// so a step of a workflow on `orders` may read `row.total`, and the same step
/// in a workflow on a `login` trigger is told `row` is not in scope rather than
/// reading null.
pub fn workflow_shape(catalog: &Catalog, channel: Option<&str>) -> Result<SchemaShape> {
    step_shape(catalog, channel)
}

/// The name a workflow's formulas that range over **no table** are validated
/// under — `sc-action`'s, unchanged, because they are the same formulas in the
/// same language and a second spelling of "no table" would read as a second rule.
pub const WORKFLOW_SCOPE: &str = EVENT_SCOPE;

#[cfg(test)]
mod tests {
    use super::*;

    // What can be asserted without a catalog is that the one thing a workflow
    // adds is added, and that it is added to what an action already has. The
    // full scope needs a database, so it is pinned in `tests/versions.rs`.
    #[test]
    fn the_scope_name_for_a_formula_over_no_table_is_the_actions_own() {
        assert_eq!(WORKFLOW_SCOPE, EVENT_SCOPE);
    }

    #[test]
    fn context_is_ambient_and_fieldless() {
        use sc_expr::Ambient;
        let shape = SchemaShape::new().ambient_fields(Ambient::Context, None::<[String; 0]>);
        assert!(shape.declares_ambient(Ambient::Context));
        // Fieldless: nothing to check `context.x` against, so `context.x`
        // resolves wherever it is written and reads null when it is not there.
        assert!(shape.ambient_field_set(Ambient::Context).is_none());
        // And it is not in scope by default — presence is scope, so an ordinary
        // trigger's formula naming `context` is an unknown identifier.
        assert!(!SchemaShape::new().declares_ambient(Ambient::Context));
    }
}
