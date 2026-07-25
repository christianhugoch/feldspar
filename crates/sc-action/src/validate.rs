//! Validating a [`Trigger`] before it is stored — and again when it is loaded.
//!
//! Every way a trigger can be wrong is checked in one place, and checked **on
//! save**, because that is when the admin is standing in front of the form: an
//! unknown action, a configuration the action does not declare, an event/channel
//! mismatch, a formula naming a field that does not exist. Discovering any of
//! those when the event fires means a trigger that silently does nothing (or the
//! wrong thing) at the worst possible moment.
//!
//! The same function runs at **load** ([`Triggers`](crate::Triggers)), where a
//! trigger that no longer validates — a table was dropped, a plugin that provided
//! its action is gone, a restored dump — is dropped from the live set with its
//! reason reported. That is the fail-closed reading, and it matches what an
//! invalid ownership formula does: grant nothing, say why.
//!
//! ## The `only_if` scope
//!
//! The formula language is `sc-expr`, with decision 7's scope: bare identifiers
//! are the affected row's fields, `row`/`old` are the event's rows ambiently,
//! `user` is the caller — and the **operation flags are refused by name**, because
//! the event already *is* the operation, so `_insert` inside an insert trigger is
//! a tautology and inside a delete trigger a lie.

use sc_catalog::Catalog;
use sc_error::{Error, Result};
use sc_expr::{Ambient, Formula, SchemaShape};
use sc_types::validate_attrs;

use crate::registry::ActionRegistry;
use crate::trigger::Trigger;

/// Check everything about `trigger` that can be checked without firing it.
///
/// Called by [`save_trigger`](crate::save_trigger) and by the cache on load. The
/// error names the trigger first, then the problem, because the admin is looking
/// at a list of triggers when they see it.
pub async fn validate_trigger(
    catalog: &Catalog,
    registry: &ActionRegistry,
    trigger: &Trigger,
) -> Result<()> {
    let name = trigger.name.trim();
    if name.is_empty() {
        return Err(Error::invalid("a trigger needs a name"));
    }
    let problem = |msg: String| Error::invalid(format!("trigger `{name}`: {msg}"));

    // The action must exist *and* be configured the way it declares. The registry
    // error already lists the alternatives.
    let action = registry
        .require(trigger.action.trim())
        .map_err(|e| problem(e.to_string()))?;
    validate_attrs(&action.config_spec(), &trigger.configuration)
        .map_err(|e| problem(format!("action `{}`: {e}", action.name())))?;

    if let Some(role) = trigger.min_role
        && !(1..=100).contains(&role)
    {
        return Err(problem(format!(
            "`min_role` must be a role between 1 and 100, got {role}"
        )));
    }

    // The event and the channel have to agree: a table event without a table
    // cannot know what to listen to, and a login event with one is a
    // misunderstanding worth naming rather than a channel silently ignored.
    let channel = trigger
        .channel
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());
    match (trigger.when.is_table_event(), channel) {
        (true, None) => {
            return Err(problem(format!(
                "an `{}` event must name the table it listens to",
                trigger.when
            )));
        }
        (true, Some(table)) => {
            if catalog.get(table)?.is_none() {
                return Err(problem(format!("no table named `{table}`")));
            }
        }
        (false, Some(channel)) => {
            return Err(problem(format!(
                "an `{}` event has no table, but `{channel}` was given as one",
                trigger.when
            )));
        }
        (false, None) => {}
    }

    if let Some(source) = trigger
        .only_if
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        // Only a table event has a row to test. Elsewhere an "only if" would have
        // nothing to range over, and accepting one that can never be true is
        // worse than refusing it. (A caller-only condition on `login` is a real
        // want; it needs a table-less scope, which is a later phase's.)
        let Some(table) = channel else {
            return Err(problem(format!(
                "an `only if` formula needs a row to test, which an `{}` event does not have",
                trigger.when
            )));
        };
        let formula = Formula::parse(source).map_err(|e| problem(format!("`only if`: {e}")))?;
        let shape = trigger_shape(catalog, table)?;
        let analysis = formula
            .validate(&shape, table)
            .map_err(|e| problem(format!("`only if`: {e}")))?;
        if !analysis.flags.is_empty() {
            return Err(problem(
                "`only if`: the operation flags (`_insert`, `_update`, …) are not available — \
                 the trigger's own event is the operation"
                    .to_owned(),
            ));
        }
        // Every ambient object is allowed here; this asserts the set has not
        // silently grown past what evaluation binds.
        if let Some(ambient) = analysis.ambient_outside(&Ambient::ALL) {
            return Err(problem(format!("`only if`: `{ambient}` is not available")));
        }
    }
    Ok(())
}

/// The schema shape a trigger's formula is validated and evaluated against: the
/// catalog's shape plus `row`/`old` in scope, carrying `table`'s fields
/// (decision 7).
///
/// One function, so validation and evaluation cannot drift into disagreeing about
/// what is in scope — a formula accepted on save and then unbound at fire time
/// would be the worst of both.
pub fn trigger_shape(catalog: &Catalog, table: &str) -> Result<SchemaShape> {
    let fields: Vec<String> = catalog
        .require(table)?
        .fields
        .iter()
        .map(|f| f.base.name.clone())
        .collect();
    Ok(catalog
        .schema_shape()?
        .ambient_fields(Ambient::Row, Some(fields.clone()))
        .ambient_fields(Ambient::Old, Some(fields)))
}
