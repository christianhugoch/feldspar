//! What a workflow's formulas may name (design §10.3, decision 8).
//!
//! One function, [`workflow_shape`], and it is the **only** place a step's scope
//! is decided — for the reason `action_shape` is the only place an action's is:
//! a formula accepted on save and then unbound when the step runs would be the
//! worst of both, and two functions that answer "what is in scope" eventually
//! answer differently.
//!
//! A step sees everything an action's configuration sees — the catalog's tables,
//! the event's `row`/`old`/`user`/`payload`, and [`EVENT_SCOPE`] as the table a
//! formula that ranges over nothing is validated in — plus one thing: the
//! ambient **`context`**, which is the run.
//!
//! It is `context.x` rather than a bare `x` deliberately. Bare identifiers
//! already mean "a field of the row this formula ranges over" everywhere else in
//! the language (§10.1's `EVENT_SCOPE`), and quietly redefining them inside a
//! workflow would make one language mean two things depending on where it was
//! written. `context` is fieldless, like `payload`: nothing declares what a run
//! has accumulated, so `context.total` resolves and reads null before the step
//! that writes it has run.

use sc_action::{EVENT_SCOPE, action_shape};
use sc_catalog::Catalog;
use sc_error::Result;
use sc_expr::{Ambient, SchemaShape};

/// The scope a workflow step's formulas are validated and evaluated in:
/// [`action_shape`] plus the ambient `context`.
///
/// `channel` is the trigger's — the table for a table event, `None` otherwise —
/// so a step of a workflow on `orders` may read `row.total`, and the same step
/// in a workflow on a `login` trigger is told `row` is not in scope rather than
/// reading null.
pub fn workflow_shape(catalog: &Catalog, channel: Option<&str>) -> Result<SchemaShape> {
    Ok(action_shape(catalog, channel)?
        // `None` fields: in scope, unchecked. What is in a run context is what
        // the steps before this one put there, which no schema can declare.
        .ambient_fields(Ambient::Context, None::<[String; 0]>))
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
