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
use sc_catalog::{CallerContext, Catalog, Table, prefetch_bindings};
use sc_db::Row;
use sc_error::{Error, Result};
use sc_expr::{
    Env, Formula, FormulaCall, JsEvaluator, Operation, TranslateError, UserEnv, join_path_expr,
    translate,
};
use sc_query::{Expr, OrderBy, Projection, Select, Source, Value};
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

/// The [`CallerContext`] a row operation runs under: the caller's role, and —
/// when logged in — their fields as the JSON object the `sc.user` GUC carries,
/// exactly the shape `UserEnv::Guc`'s
/// `current_setting('sc.user', …)::jsonb ->> 'x'` reads. Anonymous callers
/// carry only the role, so the policies' `current_setting('sc.user', true)`
/// reads `NULL` and `user === null` decides.
///
/// It travels with **every** write, not only with the RLS ones it was built for:
/// off an RLS table it sets nothing and decides nothing, and is simply who the
/// table event reports as the cause (§10.2).
pub fn caller_context(user: Option<&User>) -> CallerContext {
    caller_context_at(caller_role(user), user)
}

/// [`caller_context`] at an **explicit** role — what the admin API's row
/// endpoints use: [`ROLE_ADMIN`](sc_auth::ROLE_ADMIN) clears every policy's role
/// floor, so the admin's own row editor works on a FORCE'd table, while the user
/// object still says which admin did it.
pub fn caller_context_at(role: u8, user: Option<&User>) -> CallerContext {
    let fields = user_values(user).map(|map| {
        let obj: serde_json::Map<String, Json> = map
            .iter()
            .map(|(k, v)| (k.clone(), value_to_json(v)))
            .collect();
        Json::Object(obj)
    });
    CallerContext::new(role, fields)
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

/// The rows of `table` a caller may read, applying §7.3's rule to a
/// [`RowQuery`](rows::RowQuery) rather than to "everything".
///
/// The one entry point for a reader that is **not** an API surface — today an
/// agent's `query_table` tool (§11.3), which must see exactly what the person
/// chatting would see through the REST API and not one row more. Sharing this
/// function rather than the rule is the point: a second implementation of "meets
/// the floor OR the formula grants it" is a second place for it to be subtly
/// wrong, and the one that is wrong is the one nobody is looking at.
///
/// `role` is passed explicitly rather than read off `user` because a caller is
/// not always a user: a trigger-started agent run carries the trigger's
/// authority (admin) with no user attached, and an anonymous reader carries
/// public with the same. A caller the floor does not admit and no formula
/// extends gets an [`Error::auth`] naming the table — the reader here is a tool
/// result a model has to be able to act on, not an HTTP status.
pub async fn read_rows_as(
    cat: &Catalog,
    table: &Table,
    query: &rows::RowQuery,
    role: u8,
    user: Option<&User>,
    evaluator: Option<&Arc<dyn JsEvaluator>>,
) -> Result<Json> {
    let granted = read_row_values_as(cat, table, query, role, user, evaluator).await?;
    Ok(Json::Array(
        granted
            .iter()
            .map(|values| table_row_json(table, values))
            .collect(),
    ))
}

/// [`read_rows_as`] as **query values** rather than as the JSON wire shape —
/// the same rule, the same statements, read by a caller that needs the values
/// typed and needs the [`RowQuery::extra`](rows::RowQuery::extra) projections
/// it asked for.
///
/// The GraphQL provider is that caller (§13.4): a `Decimal` must reach the wire
/// exact rather than through a JSON number, and a requested `manager { email }`
/// rides back as an aliased extra column that the row's JSON rendering would
/// drop. Sharing the *function* rather than the rule is the whole point — this
/// is where "meets the floor OR the formula grants it" lives, and
/// [`read_rows_as`] is now a rendering of it.
pub async fn read_row_values_as(
    cat: &Catalog,
    table: &Table,
    query: &rows::RowQuery,
    role: u8,
    user: Option<&User>,
    evaluator: Option<&Arc<dyn JsEvaluator>>,
) -> Result<Vec<BTreeMap<String, Value>>> {
    // The database enforces this table's ownership: run the read in a
    // caller-context transaction and let the policies decide, exactly as the
    // REST provider's RLS path does.
    if table.rls_enabled {
        let ctx = caller_context_at(role, user);
        return rows::list_row_values(cat, table, query, Some(&ctx)).await;
    }
    if role <= table.access.min_role_read {
        return rows::list_row_values(cat, table, query, None).await;
    }
    let Some(formula) = &table.ownership else {
        return Err(Error::auth(format!("you may not read `{}`", table.name)));
    };

    let shape = cat.schema_shape()?;
    let env = UserEnv::Inline(user_values(user));
    let calc = table.calc_formulas();
    match translate(
        formula,
        Operation::Read,
        &Env::new(&env).with_calc(&calc),
        &shape,
        &table.name,
    ) {
        // The database filters, and the caller's own filter, ordering and bound
        // ride along in the same statement.
        Ok(pred) => rows::list_row_values(cat, table, &query.clone().and_filter(pred), None).await,
        // The formula needs JavaScript. The filter and the ordering still go to
        // the database — the ordering survives because filtering preserves it —
        // but the **bound does not**: a `LIMIT`/`OFFSET` applied before the
        // evaluator has spoken would count rows the caller may not see, and
        // answer "10 rows" with three (or page past rows that were never
        // theirs). So both are applied here, after.
        Err(TranslateError::Untranslatable(_)) => {
            let evaluator = require_evaluator(evaluator)?;
            let fetched = fetch_rows_with_joins(
                cat,
                table,
                formula,
                &shape,
                query.filter.clone(),
                &query.order,
                &query.extra,
            )
            .await?;
            let limit = query.limit.unwrap_or(u64::MAX);
            let offset = query.offset.unwrap_or(0);
            let mut passed = 0_u64;
            let mut granted = Vec::new();
            for values in fetched {
                if granted.len() as u64 >= limit {
                    break;
                }
                if allowed(evaluator, formula, Operation::Read, user, &values).await {
                    passed += 1;
                    if passed > offset {
                        granted.push(values);
                    }
                }
            }
            Ok(granted)
        }
        Err(e) => Err(e.into()),
    }
}

/// Insert `body` into `table` as a caller who is **not** an API surface — an
/// agent's `insert_row` tool (§11.3).
///
/// The write half of [`read_rows_as`], and the same argument for its existence:
/// §7.3's rule for a write is *meets `min_role_write` OR the formula grants the
/// proposed row*, checked with `_insert` folded true (§6's WITH CHECK, at
/// runtime), and a second implementation of that is a second place for it to be
/// subtly wrong. The write itself goes through [`rows::create_row_ctx`], so it is
/// coerced, validated, `File`-field-checked and **observed by triggers** exactly
/// like an API caller's.
///
/// A caller the floor does not admit and no formula extends gets an
/// [`Error::auth`] naming the table, rather than an HTTP status: the reader is a
/// tool result a model has to be able to act on.
pub async fn insert_row_as(
    cat: &Catalog,
    table: &Table,
    body: &Json,
    role: u8,
    user: Option<&User>,
    evaluator: Option<&Arc<dyn JsEvaluator>>,
) -> Result<Json> {
    let caller = caller_context_at(role, user);
    // The database enforces this table's ownership: the policies decide, and a
    // `WITH CHECK` they refuse surfaces from `run_in_context`.
    if table.rls_enabled {
        return rows::create_row_ctx(cat, table, body, Some(&caller)).await;
    }
    if let Some(formula) = write_formula(table, role, "write")? {
        let proposed = rows::coerce_row_values(table, body)?;
        if !row_allowed(
            cat,
            table,
            formula,
            Operation::Insert,
            user,
            evaluator,
            &proposed,
        )
        .await?
        {
            return Err(Error::auth(format!(
                "this row is outside what you may write to `{}`",
                table.name
            )));
        }
    }
    rows::create_row_ctx(cat, table, body, Some(&caller)).await
}

/// Update the row of `table` addressed by `id`, as a caller who is not an API
/// surface (§11.3). [`insert_row_as`]'s sibling.
///
/// Checked **twice** where it matters, exactly as the REST path is: granted on
/// the existing row (USING) *and* on the row as it would become (WITH CHECK), so
/// an update cannot move a row out of the caller's own ownership. A row the
/// formula withholds is the **same** not-found an absent row gets — a tool must
/// not become a way to probe which rows exist.
pub async fn update_row_as(
    cat: &Catalog,
    table: &Table,
    id: &str,
    body: &Json,
    role: u8,
    user: Option<&User>,
    evaluator: Option<&Arc<dyn JsEvaluator>>,
) -> Result<Json> {
    let caller = caller_context_at(role, user);
    if table.rls_enabled {
        return rows::update_row_ctx(cat, table, id, body, Some(&caller)).await;
    }
    let Some(formula) = write_formula(table, role, "write")? else {
        return rows::update_row_ctx(cat, table, id, body, Some(&caller)).await;
    };
    let existing = owned_row(cat, table, formula, Operation::Update, user, evaluator, id).await?;
    let changes = rows::coerce_row_values(table, body)?;
    let merged = merged_row(table, &existing, &changes);
    if !row_allowed(
        cat,
        table,
        formula,
        Operation::Update,
        user,
        evaluator,
        &merged,
    )
    .await?
    {
        return Err(Error::auth(format!(
            "that update would move the row out of what you may reach in `{}`",
            table.name
        )));
    }
    // The translated predicate rides in the UPDATE's WHERE where it can, closing
    // the gap between the check and the write.
    let guard = write_guard(cat, table, formula, Operation::Update, user)?;
    rows::update_row_guarded(cat, table, id, body, guard, Some(&caller)).await
}

/// Delete the row of `table` addressed by `id`, as a caller who is not an API
/// surface (§11.3). [`insert_row_as`]'s sibling, and the same not-found rule.
pub async fn delete_row_as(
    cat: &Catalog,
    table: &Table,
    id: &str,
    role: u8,
    user: Option<&User>,
    evaluator: Option<&Arc<dyn JsEvaluator>>,
) -> Result<Json> {
    let caller = caller_context_at(role, user);
    if table.rls_enabled {
        return rows::delete_row_ctx(cat, table, id, Some(&caller)).await;
    }
    let Some(formula) = write_formula(table, role, "delete from")? else {
        return rows::delete_row_ctx(cat, table, id, Some(&caller)).await;
    };
    owned_row(cat, table, formula, Operation::Delete, user, evaluator, id).await?;
    let guard = write_guard(cat, table, formula, Operation::Delete, user)?;
    rows::delete_row_guarded(cat, table, id, guard, Some(&caller)).await
}

/// Which formula a sub-floor writer is judged by: `None` when the caller meets
/// `min_role_write` and takes the ordinary path, `Some` when they are below it
/// and the table has one, and an error when they are below it and it has none —
/// which is the same denial the `MinRole` gate would have produced, said in words
/// a tool result can carry.
fn write_formula<'a>(table: &'a Table, role: u8, verb: &str) -> Result<Option<&'a Formula>> {
    if role <= table.access.min_role_write {
        return Ok(None);
    }
    match &table.ownership {
        Some(formula) => Ok(Some(formula)),
        None => Err(Error::auth(format!("you may not {verb} `{}`", table.name))),
    }
}

/// The existing row `id`, granted to the caller by the formula for `op` — or the
/// **same not-found a missing row gets**, which is what makes a denial
/// indistinguishable from an absence. The twin of `RestProvider::owned_row`.
async fn owned_row(
    cat: &Catalog,
    table: &Table,
    formula: &Formula,
    op: Operation,
    user: Option<&User>,
    evaluator: Option<&Arc<dyn JsEvaluator>>,
    id: &str,
) -> Result<BTreeMap<String, Value>> {
    let not_found = || {
        let pk = rows::single_pk(table)?;
        Err(Error::not_found(format!("no row with {pk} = {id}")))
    };
    let Some(existing) = fetch_row_values(cat, table, formula, id).await? else {
        return not_found();
    };
    if !row_allowed(cat, table, formula, op, user, evaluator, &existing).await? {
        return not_found();
    }
    Ok(existing)
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
    let calc = table.calc_formulas();
    match translate(
        formula,
        Operation::Read,
        &Env::new(&env).with_calc(&calc),
        &shape,
        &table.name,
    ) {
        // The database filters: one query, no V8 in the loop.
        Ok(pred) => rows::list_rows_where(cat, table, Some(pred), None).await,
        // The formula's shape needs JavaScript: fetch rows with their join
        // values projected alongside and let the evaluator decide per row.
        Err(TranslateError::Untranslatable(_)) => {
            let evaluator = require_evaluator(evaluator)?;
            let fetched =
                fetch_rows_with_joins(cat, table, formula, &shape, None, &[], &[]).await?;
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
    // The join/relation values the evaluator needs but the row does not carry —
    // resolved by `sc_catalog::prefetch_bindings`, which a trigger's `only_if`
    // (Phase 2) shares, so there is one implementation of "what does a reified
    // formula get bound".
    prefetch_bindings(cat, table, &analysis, &shape, &mut values).await?;
    Ok(allowed(evaluator, formula, op, user, &values).await)
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
    let calc = table.calc_formulas();
    match translate(
        formula,
        op,
        &Env::new(&env).with_calc(&calc),
        &shape,
        &table.name,
    ) {
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
    let mut fetched =
        fetch_rows_with_joins(cat, table, formula, &shape, Some(filter), &[], &[]).await?;
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
        // An ownership formula has no triggering event, so no `row`/`old` is in
        // scope — validation refuses naming one (§7.3's scope is the row's own
        // fields, `user` and the flags).
        ambient: Default::default(),
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
    order: &[OrderBy],
    extra: &[Projection],
) -> Result<Vec<BTreeMap<String, Value>>> {
    let analysis = formula.validate(shape, &table.name)?;
    let mut columns = vec![Projection::all()];
    for path in &analysis.join_paths {
        let expr = join_path_expr(shape, &table.name, &path.ident).map_err(Error::from)?;
        columns.push(Projection::expr_as(expr, path.ident.clone()));
    }
    // The caller's own extra projections ride in the same statement as the
    // formula's; a GraphQL Ⱶ-join and an ownership Ⱶ-join are the same kind of
    // column and there is no reason for the reified path to cost two queries.
    columns.extend(extra.iter().cloned());
    let mut select = Select::from(Source::table(table.name.clone())).columns(columns);
    if let Some(filter) = filter {
        select = select.filter(filter);
    }
    select.order = order.to_vec();
    let fetched: Vec<Row> = cat
        .provider(table)
        .query(&select)
        .await?
        .try_collect()
        .await?;
    Ok(fetched.iter().map(crate::rows::row_values).collect())
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
