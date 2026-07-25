//! What an action's configured formulas range over, and how they are run.
//!
//! Every configuration value of a built-in action that reads the event is a
//! **formula** in the same `sc-expr` language as an ownership rule, a calculated
//! field and a trigger's `only_if` (decision 7). This module holds the one
//! answer to "what is in scope, and how is it evaluated", so the three actions
//! cannot drift into three answers.
//!
//! ## Scope
//!
//! - `user`, `row` and `old` are **ambient**: the caller and the event's rows,
//!   in scope exactly where the event has them (a `login` trigger's action gets
//!   `user` and no `row`, and naming `row` there is an error, not a null).
//! - **Bare identifiers are the row the formula ranges over.** For an
//!   `update_rows`/`delete_rows` predicate and for its assignments, that is the
//!   *target* table's row: `status === "draft"` is the row being written,
//!   `row.status` is the event's. For an `insert_row` value there is no such row
//!   — nothing is being read, only computed — so bare identifiers resolve
//!   against the empty [`EVENT_SCOPE`] and a formula naming one is refused by
//!   name at save time.
//! - The operation flags (`_insert`, …) are refused, for the reason an `only_if`
//!   refuses them: the trigger's own event *is* the operation.
//!
//! ## Authority
//!
//! An action's writes are the *admin's*, not the caller's: a trigger is
//! server-side configuration, and an audit row a user may not insert is the
//! archetype of what a trigger exists to write. So on an RLS-enforced table the
//! statement runs in a caller context at [`ROLE_ADMIN`] (which clears every
//! policy's role floor, exactly as the admin API's own row editor does), still
//! carrying the event's user so a policy that reads `user` sees who caused it.
//! Off an RLS table there is nothing to set, and the write takes the ordinary
//! pooled path.

use std::collections::BTreeMap;
use std::sync::Arc;

use sc_action::{ActionContext, Event};
use sc_auth::{ROLE_ADMIN, USERS_TABLE};
use sc_catalog::{CallerContext, Catalog, Table, prefetch_bindings};
use sc_db::Row;
use sc_error::{Error, Result};
use sc_expr::{
    Ambient, AmbientValues, Env, Formula, FormulaCall, JsEvaluator, Operation, SchemaShape,
    TableShape, TranslateError, UserEnv, translate,
};
use sc_query::{Expr, Projection, Select, Source, Value};
use sc_types::Attrs;
use serde_json::{Map, Value as Json};

use crate::convert::{json_to_natural_value, value_to_json};
use crate::rows;

/// The `table` setting every row-writing action takes.
pub(crate) const CFG_TABLE: &str = "table";
/// The predicate setting `update_rows` and `delete_rows` take.
pub(crate) const CFG_WHERE: &str = "where";
/// `insert_row`'s field → formula map.
pub(crate) const CFG_VALUES: &str = "values";
/// `update_rows`' field → formula map.
pub(crate) const CFG_ASSIGNMENTS: &str = "assignments";

/// The name a formula that ranges over **no table** is validated under.
///
/// `sc-expr` validates a formula in some table's scope, so the no-table scope is
/// a table with no fields. The name is unspellable as a real table on purpose
/// (nothing the catalog can hold collides with it) and reads as an explanation
/// where it surfaces: ``formula on `(the event)`: unknown identifier `title` ``
/// is the message an `insert_row` value gets for writing `title` where it meant
/// `row.title`.
pub(crate) const EVENT_SCOPE: &str = "(the event)";

/// The shape an action's formulas are validated and evaluated in: the catalog's
/// tables, the event's ambient objects (`sc_action::trigger_shape` — the same
/// function an `only_if` uses, so the two scopes cannot disagree), plus the
/// no-table [`EVENT_SCOPE`].
pub(crate) fn action_shape(catalog: &Catalog, channel: Option<&str>) -> Result<SchemaShape> {
    Ok(sc_action::trigger_shape(catalog, channel)?.table(EVENT_SCOPE, TableShape::new()))
}

/// A required string setting, or an error naming it.
///
/// [`validate_attrs`](sc_types::validate_attrs) has already run by the time an
/// action sees a stored configuration, so this is the belt for a row edited
/// around the API — and the message an admin gets from a half-filled form.
pub(crate) fn config_str(config: &Attrs, key: &str) -> Result<String> {
    match config.get(key) {
        Some(Json::String(s)) if !s.trim().is_empty() => Ok(s.trim().to_owned()),
        _ => Err(Error::invalid(format!("the `{key}` setting is required"))),
    }
}

/// The `where` predicate, parsed.
pub(crate) fn where_formula(config: &Attrs) -> Result<Formula> {
    let source = config_str(config, CFG_WHERE)?;
    Formula::parse(&source).map_err(|e| Error::invalid(format!("`{CFG_WHERE}`: {e}")))
}

/// A field → formula map setting (`{"title": "row.title", "at": "user.id"}`),
/// parsed in the order the object gives, with a parse failure named against the
/// field it belongs to.
///
/// An **empty** map is refused: an `insert_row` with no values and an
/// `update_rows` with no assignments have nothing to do, and an action that
/// quietly does nothing is the failure this project refuses to ship
/// (principle 5).
pub(crate) fn formula_map(config: &Attrs, key: &str) -> Result<Vec<(String, Formula)>> {
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
pub(crate) fn check_formula(
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

/// A field an action may write: one of the table's own, and not a calculated one
/// (which has no column to write).
pub(crate) fn writable_field(table: &Table, field: &str) -> Result<()> {
    match table.field(field) {
        None => Err(Error::invalid(format!(
            "`{}` has no field `{field}`",
            table.name
        ))),
        Some(f) if f.is_calc() => Err(Error::invalid(format!(
            "`{field}` is a calculated field and cannot be written"
        ))),
        Some(_) => Ok(()),
    }
}

/// One action run's formula environment: the scope, the values the event puts in
/// it, and the engine.
///
/// Built once per run ([`Scope::of`]) rather than per formula, because a
/// configuration with six value formulas would otherwise rebuild the catalog's
/// shape six times.
pub(crate) struct Scope<'a> {
    trigger: &'a str,
    catalog: &'a Catalog,
    event: &'a Event,
    evaluator: &'a Arc<dyn JsEvaluator>,
    shape: SchemaShape,
    /// `row`/`old`, in scope exactly where the event has them.
    ambient: AmbientValues,
    /// The caller's fields, or `None` for an anonymous event (`user === null`).
    user: Option<BTreeMap<String, Value>>,
}

impl<'a> Scope<'a> {
    /// The scope `ctx`'s formulas are read in.
    ///
    /// Fails when the context has no JavaScript engine, naming the trigger: an
    /// action whose configuration *is* formulas has nothing correct to do without
    /// one, and doing nothing quietly is the failure mode this refuses.
    pub(crate) fn of(ctx: &'a ActionContext<'_>) -> Result<Scope<'a>> {
        let event = ctx.event;
        let channel = event
            .channel
            .as_deref()
            .filter(|_| event.kind.is_table_event());
        Ok(Scope {
            trigger: ctx.trigger,
            catalog: ctx.catalog,
            event,
            evaluator: ctx.evaluator()?,
            shape: action_shape(ctx.catalog, channel)?,
            ambient: ambient_values(ctx.catalog, event),
            user: user_values(ctx.catalog, event),
        })
    }

    /// Evaluate one configured formula to a JSON value, with `row`'s values bound
    /// as the bare scope (empty for a formula that ranges over no table).
    ///
    /// Always reified: a value formula's result is a *value*, so there is no SQL
    /// statement for a translation to ride in on. `what` names the setting being
    /// computed, so a throwing formula points at the field it belongs to.
    pub(crate) async fn value(
        &self,
        formula: &Formula,
        row: &BTreeMap<String, Value>,
        op: Operation,
        what: &str,
    ) -> Result<Json> {
        let call = self.call(formula, row, op);
        self.evaluator
            .eval_value(call)
            .await
            .map_err(|e| self.failed(what, &e))
    }

    /// The rows of `table` the `where` predicate **selects**, each carrying
    /// everything the formulas in `also_bound` (the assignments) will read.
    ///
    /// The two-strategy rule, decided once for `update_rows` and `delete_rows`:
    /// the predicate is translated into the `SELECT`'s `WHERE` when it
    /// translates — the event's row and caller inline as literals, so no new SQL
    /// construct is needed — and falls back to fetch-then-filter through the
    /// reified evaluator when it does not, exactly as an ownership read does. The
    /// fallback costs nothing extra: the rows are being fetched either way,
    /// because each one is then written **by primary key, one at a time**, which
    /// is what makes its own triggers fire with its own row (decision 2).
    ///
    /// An evaluator error here **fails the action**, where the same error in an
    /// ownership check would deny. The contracts differ because the questions
    /// do: "may this caller see this row" has a safe answer when the formula
    /// breaks, and "which rows did the admin mean" does not.
    pub(crate) async fn matching_rows(
        &self,
        table: &Table,
        predicate: &Formula,
        also_bound: &[&Formula],
        op: Operation,
    ) -> Result<Vec<BTreeMap<String, Value>>> {
        let user_env = UserEnv::Inline(self.user.clone());
        let calc = table.calc_formulas();
        let env = Env::new(&user_env)
            .with_ambient(&self.ambient)
            .with_calc(&calc);
        let filter = match translate(predicate, op, &env, &self.shape, &table.name) {
            Ok(predicate) => Some(predicate),
            Err(TranslateError::Untranslatable(_)) => None,
            Err(TranslateError::Error(e)) => return Err(self.failed(CFG_WHERE, &e)),
        };
        let selected_by_sql = filter.is_some();

        // The bindings each surviving row needs beyond its own columns: a
        // Ⱶ-join path or a Ↄ-relation any formula that will run against it reads.
        // The read path could project a join into the query, but these rows are
        // going to be written one at a time anyway, so a resolution per row is
        // the shape this path already has — and `prefetch_bindings` is the one
        // implementation of it (it is also what a trigger's `only_if` uses).
        let mut analyses = Vec::with_capacity(also_bound.len() + 1);
        if !selected_by_sql {
            analyses.push(
                predicate
                    .validate(&self.shape, &table.name)
                    .map_err(|e| self.failed(CFG_WHERE, &e))?,
            );
        }
        for formula in also_bound {
            analyses.push(formula.validate(&self.shape, &table.name)?);
        }

        let fetched = self.fetch(table, filter).await?;
        let mut out = Vec::with_capacity(fetched.len());
        for mut values in fetched {
            for analysis in &analyses {
                prefetch_bindings(self.catalog, table, analysis, &self.shape, &mut values).await?;
            }
            if selected_by_sql || self.selects(predicate, &values, op).await? {
                out.push(values);
            }
        }
        Ok(out)
    }

    /// The authority a write on `table` runs under: none off an RLS table, and
    /// admin-in-the-caller's-name on one (see the module docs).
    pub(crate) fn authority(&self, table: &Table) -> Option<CallerContext> {
        table.rls_enabled.then(|| CallerContext {
            role: ROLE_ADMIN,
            user_json: self.event.user.as_ref().map(Json::to_string),
        })
    }

    /// Whether the predicate selects one fetched row — the reified half of
    /// [`matching_rows`].
    async fn selects(
        &self,
        predicate: &Formula,
        values: &BTreeMap<String, Value>,
        op: Operation,
    ) -> Result<bool> {
        let call = self.call(predicate, values, op);
        self.evaluator
            .eval(call)
            .await
            .map_err(|e| self.failed(CFG_WHERE, &e))
    }

    /// The rows of `table` (all of them when there is no filter), as value maps
    /// including any non-stored calculated field a formula may read.
    async fn fetch(
        &self,
        table: &Table,
        filter: Option<Expr>,
    ) -> Result<Vec<BTreeMap<String, Value>>> {
        let mut columns = vec![Projection::all()];
        columns.extend(rows::calc_projections(self.catalog, table)?);
        let mut select = Select::from(Source::table(table.name.clone())).columns(columns);
        if let Some(filter) = filter {
            select = select.filter(filter);
        }
        let fetched =
            rows::run_read(self.catalog, table, &select, self.authority(table).as_ref()).await?;
        Ok(fetched.iter().map(row_values).collect())
    }

    /// One evaluation request: the formula, the bare scope, and the ambient
    /// objects — built here so the two evaluator calls cannot bind different
    /// things.
    fn call(&self, formula: &Formula, row: &BTreeMap<String, Value>, op: Operation) -> FormulaCall {
        FormulaCall {
            formula: formula.clone(),
            op,
            row: row.clone(),
            user: self.user.clone(),
            ambient: self.ambient.clone(),
        }
    }

    /// An action failure attributed to the trigger and the setting that caused it.
    fn failed(&self, what: &str, e: &Error) -> Error {
        Error::invalid(format!("trigger `{}`: {what}: {e}", self.trigger))
    }
}

/// A fetched row as a value map keyed by column name.
fn row_values(row: &Row) -> BTreeMap<String, Value> {
    row.columns()
        .iter()
        .cloned()
        .zip(row.values().iter().cloned())
        .collect()
}

/// The event's `row`/`old` as the values both evaluators bind (decision 7).
///
/// Presence is scope: neither is in the map for an event with no row, so a
/// formula naming `row` there fails validation instead of reading null. On an
/// insert or a delete, `old` **is** in the map with no value — in scope and
/// null — which is what makes `old.x` there a null rather than an error.
fn ambient_values(catalog: &Catalog, event: &Event) -> AmbientValues {
    let mut ambient = AmbientValues::new();
    if !event.kind.is_table_event() {
        return ambient;
    }
    let table = event
        .channel
        .as_deref()
        .and_then(|name| catalog.get(name).ok().flatten());
    ambient.insert(
        Ambient::Row,
        Some(typed_values(table.as_ref(), &event.row_object())),
    );
    ambient.insert(
        Ambient::Old,
        event
            .old_row
            .is_some()
            .then(|| typed_values(table.as_ref(), &event.old_row_object())),
    );
    ambient
}

/// The caller as the formulas see it, typed by the users table where it is
/// available — so `user.id` is the uuid a `uuid` column can be compared against
/// in SQL, not a string that would fail the comparison.
fn user_values(catalog: &Catalog, event: &Event) -> Option<BTreeMap<String, Value>> {
    let user = event.user.as_ref()?.as_object()?;
    let users = catalog.get(USERS_TABLE).ok().flatten();
    Some(typed_values(users.as_ref(), user))
}

/// A JSON object as query values, each field coerced to its **column's** type
/// where the table declares one and read as its own JSON shape where it does not
/// (a calculated field, a row from a table since dropped, a value the column
/// cannot hold — which is the event's problem to report, not this conversion's).
fn typed_values(table: Option<&Table>, obj: &Map<String, Json>) -> BTreeMap<String, Value> {
    obj.iter()
        .map(|(name, json)| {
            let typed = table
                .filter(|t| t.field(name).is_some())
                .and_then(|t| rows::column_value(t, name, json).ok());
            (
                name.clone(),
                typed.unwrap_or_else(|| json_to_natural_value(json)),
            )
        })
        .collect()
}

/// The primary key of a selected row as the string the `rows` layer addresses a
/// row by, coercing back through the same JSON rendering an API response uses.
pub(crate) fn row_id(
    table: &Table,
    pk: &str,
    values: &BTreeMap<String, Value>,
) -> Result<(String, Json)> {
    let value = values.get(pk).filter(|v| !v.is_null()).ok_or_else(|| {
        Error::msg(format!(
            "a row selected from `{}` carries no `{pk}` to address it by",
            table.name
        ))
    })?;
    let json = value_to_json(value);
    let id = match &json {
        Json::String(s) => s.clone(),
        other => other.to_string(),
    };
    Ok((id, json))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `tasks(id, status, count)` with the event's `row` in scope carrying
    /// `books`' fields — the shape the integration tests' triggers are read in,
    /// built by hand so this test needs no database.
    fn shape() -> SchemaShape {
        SchemaShape::new()
            .table(
                "tasks",
                TableShape::new()
                    .primary_key("id")
                    .field("id")
                    .field("status")
                    .field("count"),
            )
            .ambient_fields(Ambient::Row, Some(["id", "title", "status", "pages"]))
    }

    /// The two `where` spellings the integration test asserts the *same rows*
    /// for really do take the two different strategies. Without this, that test
    /// could be comparing the SQL path against itself and pass while the reified
    /// fallback was never run.
    #[test]
    fn the_two_where_strategies_are_chosen_by_translatability() {
        let shape = shape();
        let user = UserEnv::Inline(None);
        let mut ambient = AmbientValues::new();
        ambient.insert(
            Ambient::Row,
            Some(BTreeMap::from([(
                "status".to_owned(),
                Value::Text("done".to_owned()),
            )])),
        );
        let env = Env::new(&user).with_ambient(&ambient);
        let translated = |source: &str| {
            let formula = Formula::parse(source).expect("parses");
            translate(&formula, Operation::Update, &env, &shape, "tasks")
        };
        assert!(translated("status === row.status").is_ok());
        assert!(matches!(
            translated("[status].some(s => s === row.status)"),
            Err(TranslateError::Untranslatable(_))
        ));
    }

    #[test]
    fn a_formula_map_is_parsed_in_order_and_its_failures_are_named() {
        let config: Attrs = [(
            "values".to_owned(),
            json!({ "b": "row.title", "a": "user.id" }),
        )]
        .into_iter()
        .collect();
        let parsed = formula_map(&config, "values").expect("parses");
        // The stored document's own order, which is the order the admin entered
        // the fields in — not re-sorted underneath them.
        let fields: Vec<&str> = parsed.iter().map(|(f, _)| f.as_str()).collect();
        assert_eq!(fields, vec!["b", "a"]);

        // Every refusal names the setting, and a formula failure names its field.
        for (config, expected) in [
            (json!({}), "no fields"),
            (json!({ "a": 7 }), "must be a formula"),
            (json!("row.title"), "object of field name"),
        ] {
            let config: Attrs = [("values".to_owned(), config)].into_iter().collect();
            let msg = formula_map(&config, "values").unwrap_err().to_string();
            assert!(msg.contains(expected), "{msg}");
        }
        let config: Attrs = [("values".to_owned(), json!({ "a": "row." }))]
            .into_iter()
            .collect();
        let msg = formula_map(&config, "values").unwrap_err().to_string();
        assert!(msg.contains("`a`") && msg.contains("parse error"), "{msg}");
    }
}
