//! What an action's configuration means: the scope its formulas are read in, and
//! the values the event puts in that scope.
//!
//! Every configuration value of a built-in action that reads the event is a
//! **formula** in the same `sc-expr` language as an ownership rule, a calculated
//! field and a trigger's `only_if` (decision 7). This module is the one answer to
//! "what is in scope, what is bound, and how is a setting parsed" — shared by the
//! actions in this crate *and* by the ones that live above it because they write
//! rows (`sc-api`'s `insert_row`/`update_rows`/`delete_rows`). Two crates, one
//! answer: an action whose configuration validated on save must bind the same
//! things at fire time, wherever it is implemented.
//!
//! ## Scope
//!
//! - `user`, `row` and `old` are **ambient**: the caller and the event's rows, in
//!   scope exactly where the event has them. A `login` trigger's action gets
//!   `user` and no `row`, and naming `row` there is an error, not a null.
//! - **Bare identifiers are the row the formula ranges over**, which for a
//!   formula that only reads the event is *nothing*: those are validated against
//!   [`EVENT_SCOPE`], an empty table, so `title` where `row.title` was meant is
//!   refused by name at save time.
//! - The operation flags (`_insert`, …) are refused, for the reason an `only_if`
//!   refuses them: the trigger's own event *is* the operation.

use std::collections::BTreeMap;

use sc_catalog::{Catalog, Table};
use sc_error::{Error, Result};
use sc_expr::{
    Ambient, AmbientValues, Formula, FormulaCall, Operation, SchemaShape, TableShape,
    value_from_json,
};
use sc_query::Value;
use sc_types::{Attrs, BasicType, TypeRef, json_to_value};
use serde_json::{Map, Value as Json};

use crate::action::ActionContext;
use crate::event::Event;
use crate::validate::trigger_shape;

/// The name a formula that ranges over **no table** is validated under.
///
/// `sc-expr` validates a formula in some table's scope, so the no-table scope is
/// a table with no fields. The name is unspellable as a real table on purpose
/// (nothing the catalog can hold collides with it) and reads as an explanation
/// where it surfaces: ``formula on `(the event)`: unknown identifier `title` `` is
/// the message an `insert_row` value gets for writing `title` where it meant
/// `row.title`.
pub const EVENT_SCOPE: &str = "(the event)";

/// The shape an action's formulas are validated and evaluated in: the catalog's
/// tables, the event's ambient objects ([`trigger_shape`] — the same function an
/// `only_if` uses, so the two scopes cannot disagree), plus [`EVENT_SCOPE`].
pub fn action_shape(catalog: &Catalog, channel: Option<&str>) -> Result<SchemaShape> {
    Ok(trigger_shape(catalog, channel)?.table(EVENT_SCOPE, TableShape::new()))
}

/// A required string setting, or an error naming it.
///
/// [`validate_attrs`](sc_types::validate_attrs) has already run by the time an
/// action sees a stored configuration, so this is the belt for a row edited around
/// the API — and the message an admin gets from a half-filled form.
pub fn config_str(config: &Attrs, key: &str) -> Result<String> {
    match config.get(key) {
        Some(Json::String(s)) if !s.trim().is_empty() => Ok(s.trim().to_owned()),
        _ => Err(Error::invalid(format!("the `{key}` setting is required"))),
    }
}

/// An optional formula setting, parsed. An absent or blank one is `None`.
pub fn optional_formula(config: &Attrs, key: &str) -> Result<Option<Formula>> {
    let Some(Json::String(source)) = config.get(key) else {
        return Ok(None);
    };
    if source.trim().is_empty() {
        return Ok(None);
    }
    Formula::parse(source)
        .map(Some)
        .map_err(|e| Error::invalid(format!("`{key}`: {e}")))
}

/// A required formula setting, parsed.
pub fn required_formula(config: &Attrs, key: &str) -> Result<Formula> {
    let source = config_str(config, key)?;
    Formula::parse(&source).map_err(|e| Error::invalid(format!("`{key}`: {e}")))
}

/// A field → formula map setting (`{"title": "row.title", "at": "user.id"}`),
/// parsed in the order the stored document gives (which is the order the admin
/// entered), with a parse failure named against the field it belongs to.
///
/// An **empty** map is refused: an `insert_row` with no values and an
/// `update_rows` with no assignments have nothing to do, and an action that
/// quietly does nothing is the failure this project refuses to ship
/// (principle 5).
pub fn formula_map(config: &Attrs, key: &str) -> Result<Vec<(String, Formula)>> {
    let Some(Json::Object(map)) = config.get(key) else {
        return Err(Error::invalid(format!(
            "`{key}` must be an object of field name → formula"
        )));
    };
    if map.is_empty() {
        return Err(Error::invalid(format!("`{key}` names no fields")));
    }
    let mut out = Vec::with_capacity(map.len());
    for (field, source) in map {
        let Json::String(source) = source else {
            return Err(Error::invalid(format!(
                "`{key}`.`{field}` must be a formula, given as a string"
            )));
        };
        let formula =
            Formula::parse(source).map_err(|e| Error::invalid(format!("`{field}`: {e}")))?;
        out.push((field.clone(), formula));
    }
    Ok(out)
}

/// Check one configured formula in the scope it will be evaluated in: every
/// identifier resolves, and none of the operation flags is used.
pub fn check_formula(
    shape: &SchemaShape,
    scope: &str,
    formula: &Formula,
    what: &str,
) -> Result<()> {
    let analysis = formula
        .validate(shape, scope)
        .map_err(|e| Error::invalid(format!("{what}: {e}")))?;
    if !analysis.flags.is_empty() {
        return Err(Error::invalid(format!(
            "{what}: the operation flags (`_insert`, `_update`, …) are not available — \
             the trigger's own event is the operation"
        )));
    }
    Ok(())
}

/// The values an event puts in scope: the ambient `row`/`old` and the caller.
///
/// **Presence is scope.** Neither `row` nor `old` is in the map for an event with
/// no row, so a formula naming one fails rather than reading null; on an insert or
/// a delete `old` *is* in the map with no value — in scope and null — which is
/// what makes `old.x` there a null rather than an error. Those rules are the
/// drift-prone part, so they live here once and both crates' actions build their
/// bindings through this type.
#[derive(Debug, Clone, Default)]
pub struct EventBindings {
    /// `row`/`old`, present exactly where the event has them.
    pub ambient: AmbientValues,
    /// The caller's fields, or `None` for an anonymous event (`user === null`).
    pub user: Option<BTreeMap<String, Value>>,
}

impl EventBindings {
    /// The event's values, each read as the [`Value`] its own JSON shape implies.
    ///
    /// Enough for **reified** evaluation, which is all an action that only reads
    /// the event needs: the evaluator renders every binding back through
    /// `value_to_json`, so a typed and an untyped reading of the same JSON reach
    /// JavaScript identically. A caller that also *translates* a formula to SQL
    /// needs real column types and builds its values with [`with_values`] instead.
    ///
    /// [`with_values`]: EventBindings::with_values
    pub fn of(event: &Event) -> EventBindings {
        EventBindings::with_values(event, |_, _, json| value_from_json(json))
    }

    /// The event's values with a caller-supplied reading of each field, given the
    /// object it belongs to (`row`/`old` are the event's table, `user` the users
    /// table) — how `sc-api` types them against real columns so an inlined
    /// `user.id` can be compared to a `uuid` column in SQL.
    pub fn with_values(
        event: &Event,
        value: impl Fn(Ambient, &str, &Json) -> Value,
    ) -> EventBindings {
        let object = |ambient: Ambient, obj: &Map<String, Json>| -> BTreeMap<String, Value> {
            obj.iter()
                .map(|(name, json)| (name.clone(), value(ambient, name, json)))
                .collect()
        };
        let mut ambient = AmbientValues::new();
        if event.kind.is_table_event() {
            ambient.insert(
                Ambient::Row,
                Some(object(Ambient::Row, &event.row_object())),
            );
            ambient.insert(
                Ambient::Old,
                event
                    .old_row
                    .is_some()
                    .then(|| object(Ambient::Old, &event.old_row_object())),
            );
        }
        let user = event
            .user
            .as_ref()
            .and_then(Json::as_object)
            .map(|obj| object(Ambient::User, obj));
        EventBindings { ambient, user }
    }

    /// One evaluation request: a formula, the bare scope (`row`'s own fields for a
    /// formula that ranges over a table, empty for one that only reads the event),
    /// and these bindings.
    ///
    /// Built here so the two evaluator entry points — and the two crates' actions
    /// — cannot bind different things.
    pub fn call(
        &self,
        formula: &Formula,
        op: Operation,
        row: &BTreeMap<String, Value>,
    ) -> FormulaCall {
        FormulaCall {
            formula: formula.clone(),
            op,
            row: row.clone(),
            user: self.user.clone(),
            ambient: self.ambient.clone(),
        }
    }
}

/// One value read as the type of the **column** it belongs to, where the table
/// has one — and as its own JSON shape where it does not (a calculated field, a
/// value the column could not hold, a row from a table since dropped, which is
/// the event's problem to report rather than this conversion's).
///
/// Reified evaluation cannot tell the difference: the evaluator renders every
/// binding back through `value_to_json`. It matters for everything *around* the
/// evaluation that reaches SQL — a `where` predicate translated with `user.id`
/// inlined against a `uuid` column, and the [`prefetch`] a Ⱶ-path needs, which
/// correlates on the row's own key value. A uuid compared as text is a SQL error,
/// not a mismatch, so this is what makes those two paths work at all.
///
/// [`prefetch`]: sc_catalog::prefetch_bindings
pub fn typed_value(table: Option<&Table>, field: &str, json: &Json) -> Value {
    let Some(type_) = table
        .and_then(|t| t.field(field))
        .map(|f| &f.base.type_)
        .filter(|_| !json.is_null())
    else {
        return value_from_json(json);
    };
    json_to_value(&storage_type(type_), json).unwrap_or_else(|_| value_from_json(json))
}

/// The basic (storage) type a JSON value is coerced through: the type itself for
/// a basic field, or the SQL type a rich field sits on (a `String` stores as
/// `text`, an `Integer` as `int8`).
fn storage_type(type_: &TypeRef) -> BasicType {
    match type_.as_basic() {
        Some(basic) => basic.clone(),
        None => BasicType::from_sql_type(type_.sql_type()),
    }
}

/// Evaluate one configured formula against the event, to a JSON value.
///
/// For an action whose formulas only read the event: the bare scope is empty, so
/// the formula sees `row`/`old`/`user` and nothing else. `what` names the setting
/// being computed, so a throwing formula points at the setting it belongs to
/// rather than at the trigger as a whole.
pub async fn event_formula_value(
    ctx: &ActionContext<'_>,
    formula: &Formula,
    what: &str,
) -> Result<Json> {
    let bindings = EventBindings::of(ctx.event);
    let call = bindings.call(formula, Operation::Read, &BTreeMap::new());
    ctx.evaluator()?
        .eval_value(call)
        .await
        .map_err(|e| Error::invalid(format!("trigger `{}`: {what}: {e}", ctx.trigger)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventKind;
    use serde_json::json;

    #[test]
    fn presence_is_scope_for_the_ambient_objects() {
        // A table event: `row` bound, `old` in scope and null on an insert.
        let insert = Event::new(EventKind::Insert)
            .on("books")
            .row(json!({ "id": 1, "title": "A" }));
        let bindings = EventBindings::of(&insert);
        assert_eq!(
            bindings.ambient[&Ambient::Row].as_ref().map(BTreeMap::len),
            Some(2)
        );
        assert!(bindings.ambient.contains_key(&Ambient::Old));
        assert!(bindings.ambient[&Ambient::Old].is_none(), "in scope, null");
        // Anonymous: `user` is null rather than an empty object.
        assert!(bindings.user.is_none());

        // An update carries both rows.
        let update = insert
            .clone()
            .old_row(json!({ "id": 1, "title": "was" }))
            .caller(1, Some(json!({ "email": "a@b.c" })));
        let bindings = EventBindings::of(&update);
        assert_eq!(
            bindings.ambient[&Ambient::Old]
                .as_ref()
                .and_then(|old| old.get("title")),
            Some(&Value::Text("was".into()))
        );
        assert_eq!(
            bindings.user.as_ref().and_then(|u| u.get("email")),
            Some(&Value::Text("a@b.c".into()))
        );

        // An event with no row: neither object is in scope at all, so a formula
        // naming `row` gets `unknown identifier` instead of a silent null.
        let login = Event::new(EventKind::Login).caller(1, Some(json!({ "email": "a@b.c" })));
        let bindings = EventBindings::of(&login);
        assert!(bindings.ambient.is_empty());
        assert!(bindings.user.is_some());
    }

    #[test]
    fn a_caller_supplied_reading_is_used_for_every_field() {
        let event = Event::new(EventKind::Insert)
            .on("books")
            .row(json!({ "id": 1 }))
            .caller(1, Some(json!({ "id": 2 })));
        // The hook is told which object each field came from, which is how a
        // caller picks the table to type it against.
        let bindings = EventBindings::with_values(&event, |ambient, name, _| {
            Value::Text(format!("{ambient}.{name}"))
        });
        assert_eq!(
            bindings.ambient[&Ambient::Row]
                .as_ref()
                .and_then(|r| r.get("id")),
            Some(&Value::Text("row.id".into()))
        );
        assert_eq!(
            bindings.user.as_ref().and_then(|u| u.get("id")),
            Some(&Value::Text("user.id".into()))
        );
    }

    #[test]
    fn the_settings_parsers_name_what_is_wrong() {
        let config: Attrs = [
            ("url".to_owned(), json!("https://x.test")),
            ("blank".to_owned(), json!("  ")),
            ("body".to_owned(), json!("row.title")),
            ("broken".to_owned(), json!("row.")),
            ("values".to_owned(), json!({ "b": "1", "a": "2" })),
        ]
        .into_iter()
        .collect();

        assert_eq!(config_str(&config, "url").unwrap(), "https://x.test");
        for key in ["blank", "missing"] {
            let msg = config_str(&config, key).unwrap_err().to_string();
            assert!(msg.contains(key) && msg.contains("required"), "{msg}");
        }
        assert!(optional_formula(&config, "body").unwrap().is_some());
        // Blank and absent are both "not configured", not an error.
        assert!(optional_formula(&config, "blank").unwrap().is_none());
        assert!(optional_formula(&config, "missing").unwrap().is_none());
        assert!(required_formula(&config, "missing").is_err());
        let msg = optional_formula(&config, "broken").unwrap_err().to_string();
        assert!(
            msg.contains("broken") && msg.contains("parse error"),
            "{msg}"
        );

        // The map keeps the document's order and names the field that fails.
        let fields: Vec<String> = formula_map(&config, "values")
            .unwrap()
            .into_iter()
            .map(|(f, _)| f)
            .collect();
        assert_eq!(fields, vec!["b", "a"]);
        for (key, expected) in [("url", "object of field name"), ("missing", "object")] {
            let msg = formula_map(&config, key).unwrap_err().to_string();
            assert!(msg.contains(expected), "{msg}");
        }
    }

    #[test]
    fn the_operation_flags_are_refused_in_an_actions_formula() {
        let shape = SchemaShape::new()
            .table(EVENT_SCOPE, TableShape::new())
            .ambient_fields(Ambient::Row, Some(["title"]));
        let ok = Formula::parse("row.title").unwrap();
        check_formula(&shape, EVENT_SCOPE, &ok, "`body`").unwrap();

        let flag = Formula::parse("_insert").unwrap();
        let msg = check_formula(&shape, EVENT_SCOPE, &flag, "`body`")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("`body`") && msg.contains("_insert"), "{msg}");

        // A bare identifier has nothing to resolve against in the event scope.
        let bare = Formula::parse("title").unwrap();
        let msg = check_formula(&shape, EVENT_SCOPE, &bare, "`body`")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("unknown identifier"), "{msg}");
        assert!(msg.contains(EVENT_SCOPE), "the scope is named: {msg}");
    }
}
