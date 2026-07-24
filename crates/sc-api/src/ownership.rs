//! Runtime enforcement of ownership formulas (§7.3, TODO Phase 5).
//!
//! The access rule this module implements, stated once: **allowed = the caller
//! meets the operation's `min_role` OR the table's ownership formula grants
//! this row** for this user and operation. Ownership *extends* access below
//! the role floor; it never narrows what a role already has — a caller at or
//! above the floor takes the ordinary unfiltered path and this module never
//! runs for them.
//!
//! Two evaluation strategies, chosen per formula:
//!
//! - **Symbolic** where possible: reads AND the translated predicate
//!   (`UserEnv::Inline`, flags folded) into the SELECT, and guarded writes AND
//!   it into the UPDATE/DELETE's WHERE — the database does the filtering and a
//!   denied row is indistinguishable from an absent one.
//! - **Reified** where not ([`TranslateError::Untranslatable`]): rows are
//!   fetched *with their Ⱶ-join values projected alongside* (one query, not a
//!   round trip per row) and the formula runs in the [`JsEvaluator`]. An
//!   evaluation error **denies** — the evaluator contract — and a missing
//!   evaluator is a configuration error, never an open door.
//!
//! Writes check the row **twice** where it matters (§6's USING/WITH CHECK
//! semantics, here at runtime): an update must be granted on the existing row
//! *and* on the proposed row, so a user cannot move a row out of their own
//! ownership; an insert is checked against the proposed row with `_insert`.

use std::collections::BTreeMap;
use std::sync::Arc;

use sc_auth::{COL_ID, COL_ROLE, ROLE_PUBLIC, User};
use sc_catalog::{CallerContext, Catalog, DataFieldKind, Table};
use sc_db::Row;
use sc_error::{Error, Result};
use sc_expr::{
    AggUse, Formula, FormulaCall, INVERSE, JsEvaluator, Operation, TranslateError, UserEnv,
    join_path_expr, translate,
};
use sc_query::{Expr, Projection, Select, Source, Value};
use serde_json::{Map, Value as Json};

use crate::convert::value_to_json;
use crate::rows;

/// The caller's role: their own, or public when nobody is logged in.
pub(crate) fn caller_role(user: Option<&User>) -> u8 {
    user.map_or(ROLE_PUBLIC, |u| u.role)
}

/// Whether the caller meets a `min_role` floor (lower role = more privileged).
pub(crate) fn meets(user: Option<&User>, min_role: u8) -> bool {
    caller_role(user) <= min_role
}

/// The [`CallerContext`] an RLS transaction (§7.3, §6) runs under: the caller's
/// role, and — when logged in — their fields as the JSON object the `sc.user`
/// GUC carries, exactly the shape `UserEnv::Guc`'s
/// `current_setting('sc.user', …)::jsonb ->> 'x'` reads. Anonymous callers
/// carry only the role, so the policies' `current_setting('sc.user', true)`
/// reads `NULL` and `user === null` decides.
pub(crate) fn caller_context(user: Option<&User>) -> CallerContext {
    let user_json = user_values(user).map(|map| {
        let obj: serde_json::Map<String, Json> = map
            .iter()
            .map(|(k, v)| (k.clone(), value_to_json(v)))
            .collect();
        Json::Object(obj).to_string()
    });
    CallerContext {
        role: caller_role(user),
        user_json,
    }
}

/// The user object as the formula sees it: `id`, `role`, and every extra field
/// — the same map both the `Inline` translation env and the reified
/// [`FormulaCall`] consume, so the two evaluators see one user by construction.
fn user_values(user: Option<&User>) -> Option<BTreeMap<String, Value>> {
    user.map(|u| {
        let mut map = u.extra.clone();
        map.insert(COL_ID.to_owned(), Value::Uuid(u.id));
        map.insert(COL_ROLE.to_owned(), Value::Int(i64::from(u.role)));
        map
    })
}

/// The rows of `table` the formula grants `user` for reading — the sub-floor
/// read path.
pub(crate) async fn list_owned_rows(
    cat: &Catalog,
    table: &Table,
    formula: &Formula,
    user: Option<&User>,
    evaluator: Option<&Arc<dyn JsEvaluator>>,
) -> Result<Json> {
    let shape = cat.schema_shape()?;
    let env = UserEnv::Inline(user_values(user));
    match translate(formula, Operation::Read, &env, &shape, &table.name) {
        // The database filters: one query, no V8 in the loop.
        Ok(pred) => rows::list_rows_where(cat, table, Some(pred), None).await,
        // The formula's shape needs JavaScript: fetch rows with their join
        // values projected alongside and let the evaluator decide per row.
        Err(TranslateError::Untranslatable(_)) => {
            let evaluator = require_evaluator(evaluator)?;
            let fetched = fetch_rows_with_joins(cat, table, formula, &shape, None).await?;
            let mut granted = Vec::with_capacity(fetched.len());
            for values in fetched {
                if allowed(evaluator, formula, Operation::Read, user, &values).await {
                    granted.push(table_row_json(table, &values));
                }
            }
            Ok(Json::Array(granted))
        }
        Err(e) => Err(e.into()),
    }
}

/// Whether the formula grants `op` on the row `values` — the single-row check
/// every sub-floor write (and file access) goes through. Join values missing
/// from `values` (a proposed row that has not been stored) are resolved
/// link-by-link first.
pub(crate) async fn row_allowed(
    cat: &Catalog,
    table: &Table,
    formula: &Formula,
    op: Operation,
    user: Option<&User>,
    evaluator: Option<&Arc<dyn JsEvaluator>>,
    values: &BTreeMap<String, Value>,
) -> Result<bool> {
    let evaluator = require_evaluator(evaluator)?;
    let shape = cat.schema_shape()?;
    let analysis = formula.validate(&shape, &table.name)?;
    let mut values = values.clone();
    for path in &analysis.join_paths {
        if !values.contains_key(&path.ident) {
            let value = resolve_join_value(cat, table, &path.segments, &values).await?;
            values.insert(path.ident.clone(), value);
        }
    }
    // Aggregations over incoming keys (Phase 7): the reified evaluator does no
    // I/O, so prefetch each relation's child rows and bind them under the
    // relation identifier (`childↃkey`) — the array the prelude aggregates.
    for agg in &analysis.agg_uses {
        let ident = format!("{}{}{}", agg.child_table, INVERSE, agg.key_field);
        if !values.contains_key(&ident) {
            let rows = resolve_agg_relation(cat, &shape, agg, &values).await?;
            values.insert(ident, rows);
        }
    }
    Ok(allowed(evaluator, formula, op, user, &values).await)
}

/// Fetch the child rows an aggregation ranges over, as a JSON array bound under
/// the relation identifier for the reified evaluator. The correlation is the
/// parent's own value in the column the child key targets; a null there (or no
/// matching children) is the empty relation.
async fn resolve_agg_relation(
    cat: &Catalog,
    shape: &sc_expr::SchemaShape,
    agg: &AggUse,
    values: &BTreeMap<String, Value>,
) -> Result<Value> {
    let empty = Value::Json(Json::Array(Vec::new()));
    // The parent column the child key references (the correlation target).
    let Some(parent_field) = shape
        .tables
        .get(&agg.child_table)
        .and_then(|t| t.fields.get(&agg.key_field))
        .and_then(|f| f.key.as_ref())
        .map(|k| k.target_field.clone())
    else {
        return Ok(empty);
    };
    let parent_value = values.get(&parent_field).cloned().unwrap_or(Value::Null);
    if parent_value.is_null() {
        return Ok(empty);
    }
    let child = cat.require(&agg.child_table)?;
    let select = Select::from(Source::table(child.name.clone()))
        .columns(vec![Projection::all()])
        .filter(Expr::col(agg.key_field.clone()).eq(Expr::lit(parent_value)));
    let fetched: Vec<Row> = cat
        .provider(&child)
        .query(&select)
        .await?
        .try_collect()
        .await?;
    let rows: Vec<Json> = fetched
        .iter()
        .map(|row| {
            let obj: Map<String, Json> = row
                .columns()
                .iter()
                .zip(row.values().iter())
                .map(|(name, value)| (name.clone(), value_to_json(value)))
                .collect();
            Json::Object(obj)
        })
        .collect();
    Ok(Value::Json(Json::Array(rows)))
}

/// The translated guard predicate for a write, when the formula is
/// translatable — ANDed into the UPDATE/DELETE WHERE so the row cannot change
/// hands between the check and the write. `None` for an untranslatable
/// formula, whose single-row reified check already ran.
pub(crate) fn write_guard(
    cat: &Catalog,
    table: &Table,
    formula: &Formula,
    op: Operation,
    user: Option<&User>,
) -> Result<Option<Expr>> {
    let shape = cat.schema_shape()?;
    let env = UserEnv::Inline(user_values(user));
    match translate(formula, op, &env, &shape, &table.name) {
        Ok(pred) => Ok(Some(pred)),
        Err(TranslateError::Untranslatable(_)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Fetch the full row addressed by `id` — its columns plus every Ⱶ-join value
/// the formula reads, projected in the same query. `None` when no such row.
pub(crate) async fn fetch_row_values(
    cat: &Catalog,
    table: &Table,
    formula: &Formula,
    id: &str,
) -> Result<Option<BTreeMap<String, Value>>> {
    let shape = cat.schema_shape()?;
    let pk = rows::single_pk(table)?;
    let filter = rows::pk_filter(table, &pk, id)?;
    let mut fetched = fetch_rows_with_joins(cat, table, formula, &shape, Some(filter)).await?;
    Ok(fetched.drain(..).next())
}

/// The proposed row of an update: the existing row with the body's coerced
/// values written over it — what §6's WITH CHECK sees, computed at runtime.
/// Stale join values are dropped so [`row_allowed`] re-resolves them against
/// the (possibly changed) foreign keys.
pub(crate) fn merged_row(
    table: &Table,
    existing: &BTreeMap<String, Value>,
    changes: &BTreeMap<String, Value>,
) -> BTreeMap<String, Value> {
    let mut merged: BTreeMap<String, Value> = existing
        .iter()
        .filter(|(name, _)| table.field(name).is_some())
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    for (name, value) in changes {
        merged.insert(name.clone(), value.clone());
    }
    merged
}

/// Strip a fetched values map back to the table's own columns and render it as
/// the wire row — the projected join values are enforcement inputs, not
/// response payload.
pub(crate) fn table_row_json(table: &Table, values: &BTreeMap<String, Value>) -> Json {
    let mut map = Map::new();
    for field in &table.fields {
        if let Some(value) = values.get(&field.base.name) {
            map.insert(field.base.name.clone(), value_to_json(value));
        }
    }
    Json::Object(map)
}

/// Evaluate the formula on one row. An `Err` from the evaluator **denies**, per
/// its contract — a throwing or timed-out formula must never grant. The reason
/// is not surfaced per row (a filtered list cannot carry one); the formula
/// author sees the behaviour, and the §16 error log is where the reason will
/// land once it exists.
async fn allowed(
    evaluator: &Arc<dyn JsEvaluator>,
    formula: &Formula,
    op: Operation,
    user: Option<&User>,
    values: &BTreeMap<String, Value>,
) -> bool {
    let call = FormulaCall {
        formula: formula.clone(),
        op,
        row: values.clone(),
        user: user_values(user),
    };
    evaluator.eval(call).await.unwrap_or(false)
}

/// Rows of `table` (optionally filtered) with one extra projected column per
/// Ⱶ-join path the formula uses, aliased as the join identifier itself — the
/// correlated subselect from the symbolic translator, reused as a projection.
/// One query serves both the row data and the evaluator's bindings.
async fn fetch_rows_with_joins(
    cat: &Catalog,
    table: &Table,
    formula: &Formula,
    shape: &sc_expr::SchemaShape,
    filter: Option<Expr>,
) -> Result<Vec<BTreeMap<String, Value>>> {
    let analysis = formula.validate(shape, &table.name)?;
    let mut columns = vec![Projection::all()];
    for path in &analysis.join_paths {
        let expr = join_path_expr(shape, &table.name, &path.ident).map_err(Error::from)?;
        columns.push(Projection::expr_as(expr, path.ident.clone()));
    }
    let mut select = Select::from(Source::table(table.name.clone())).columns(columns);
    if let Some(filter) = filter {
        select = select.filter(filter);
    }
    let fetched: Vec<Row> = cat
        .provider(table)
        .query(&select)
        .await?
        .try_collect()
        .await?;
    Ok(fetched
        .iter()
        .map(|row| {
            row.columns()
                .iter()
                .cloned()
                .zip(row.values().iter().cloned())
                .collect()
        })
        .collect())
}

/// Resolve one Ⱶ-join path from a row's values by walking the Key links — the
/// path for *proposed* rows, which are not in the database to be projected
/// from. A null anywhere propagates (the Ⱶ optional-chaining contract).
async fn resolve_join_value(
    cat: &Catalog,
    table: &Table,
    segments: &[String],
    values: &BTreeMap<String, Value>,
) -> Result<Value> {
    let Some(first) = segments.first() else {
        return Ok(Value::Null);
    };
    let mut current = table.clone();
    let mut value = values.get(first).cloned().unwrap_or(Value::Null);
    for i in 1..segments.len() {
        if value.is_null() {
            return Ok(Value::Null);
        }
        let link = &segments[i - 1];
        let field = current
            .field(link)
            .ok_or_else(|| Error::invalid(format!("`{}` has no field `{link}`", current.name)))?;
        let DataFieldKind::Key {
            target_table,
            target_field,
            ..
        } = &field.kind
        else {
            return Err(Error::invalid(format!(
                "`{}`.`{link}` is not a Key field",
                current.name
            )));
        };
        let target = cat.require(&target_table.0)?;
        let next = &segments[i];
        let select = Select::from(Source::table(target.name.clone()))
            .columns(vec![Projection::expr(Expr::col(next.clone()))])
            .filter(Expr::col(target_field.0.clone()).eq(Expr::lit(value)))
            .limit(1);
        let fetched: Vec<Row> = cat
            .provider(&target)
            .query(&select)
            .await?
            .try_collect()
            .await?;
        value = fetched
            .first()
            .and_then(|r| r.values().first().cloned())
            .unwrap_or(Value::Null);
        current = target;
    }
    Ok(value)
}

/// The evaluator, or the loud configuration error. Fail closed: a deployment
/// without an engine cannot run formulas, and the answer is "fix the server",
/// never "let the request through".
fn require_evaluator(evaluator: Option<&Arc<dyn JsEvaluator>>) -> Result<&Arc<dyn JsEvaluator>> {
    evaluator.ok_or_else(|| {
        Error::config(
            "this table's ownership formula needs the JavaScript evaluator, \
             and none is configured on this server",
        )
    })
}
