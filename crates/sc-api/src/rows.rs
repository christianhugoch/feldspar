//! Table row CRUD, shared by every API surface (design §13.1, §13.4).
//!
//! Reading and writing a table's rows as JSON is the substance of *both* the
//! admin API's row endpoints and an application's REST API: same tables, same
//! query layer, same wire shape. So it lives here once, below both, rather than
//! in either. `sc-server`'s admin handlers call these functions, and so does
//! [`RestProvider`](crate::RestProvider) — which is what makes "the app's API and
//! the admin API are the same machinery" true in the code and not just in the
//! docs.
//!
//! Rows cross the wire as plain JSON objects keyed by column name;
//! [`crate::convert`] bridges those to the query layer's [`Value`]. A row is
//! addressed by its **single-column primary key** (composite keys are post-MVP).
//! These functions do no authorization of their own — the caller enforces the
//! endpoint's [`AuthRequirement`](crate::AuthRequirement) first, so every API
//! surface goes through the same §7 layer.

use sc_catalog::{CallerContext, Catalog, DataFieldKind, Table};
use sc_db::Row;
use sc_error::{Error, Repr, Result};
use sc_expr::{CalcFields, Env, Formula, TranslateError, UserEnv, translate_value};
use sc_query::{
    Assignment, Delete, Expr, Insert, Projection, Select, Source, Statement, Update, Value,
};
use sc_types::{BasicType, TypeRef};
use serde_json::{Map, Value as Json, json};

use crate::convert::{json_to_value, value_to_json};

/// Every row of `table`, as a JSON array of objects.
pub async fn list_rows(catalog: &Catalog, table: &Table) -> Result<Json> {
    list_rows_where(catalog, table, None, None).await
}

/// Every row of `table`, optionally through an RLS caller context — the entry
/// point the admin API uses so its own row viewer works on an RLS-enabled
/// (FORCE'd) table: role 1 clears every policy's role floor, so an admin sees
/// and edits everything, while a non-RLS table takes the ordinary path.
pub async fn list_rows_ctx(
    catalog: &Catalog,
    table: &Table,
    context: Option<&CallerContext>,
) -> Result<Json> {
    list_rows_where(catalog, table, None, context).await
}

/// [`update_row`] optionally through an RLS caller context (admin API).
pub async fn update_row_ctx(
    catalog: &Catalog,
    table: &Table,
    id: &str,
    body: &Json,
    context: Option<&CallerContext>,
) -> Result<Json> {
    update_row_guarded(catalog, table, id, body, None, context).await
}

/// [`delete_row`] optionally through an RLS caller context (admin API).
pub async fn delete_row_ctx(
    catalog: &Catalog,
    table: &Table,
    id: &str,
    context: Option<&CallerContext>,
) -> Result<Json> {
    delete_row_guarded(catalog, table, id, None, context).await
}

/// The rows of `table` matching `filter` (all of them for `None`), as a JSON
/// array. The filter is how ownership enforcement (§7.3) narrows a read to the
/// rows a formula grants — ANDed in by the caller as a translated predicate.
///
/// `context` routes the read through an RLS caller-context transaction (§7.3)
/// when the table's ownership is enforced by the database; `None` runs it on a
/// pooled connection through the provider, as every non-RLS read does.
pub async fn list_rows_where(
    catalog: &Catalog,
    table: &Table,
    filter: Option<Expr>,
    context: Option<&CallerContext>,
) -> Result<Json> {
    let mut columns = vec![Projection::all()];
    columns.extend(calc_projections(catalog, table)?);
    let mut select = Select::from(Source::table(table.name.clone())).columns(columns);
    if let Some(filter) = filter {
        select = select.filter(filter);
    }
    let rows = run_read(catalog, table, &select, context).await?;
    Ok(Json::Array(rows.iter().map(row_to_json).collect()))
}

/// Insert a row from a JSON object, returning the inserted row (with any
/// database-generated columns filled in).
pub async fn create_row(catalog: &Catalog, table: &Table, body: &Json) -> Result<Json> {
    create_row_ctx(catalog, table, body, None).await
}

/// [`create_row`] routed through an RLS caller context (§7.3) when one is
/// given — the insert runs in a transaction with the caller's role/identity
/// set, so an `INSERT` a policy's `WITH CHECK` rejects surfaces as the same
/// not-found a missing row gets.
pub async fn create_row_ctx(
    catalog: &Catalog,
    table: &Table,
    body: &Json,
    context: Option<&CallerContext>,
) -> Result<Json> {
    let obj = require_object(body)?;
    reject_calc_writes(table, obj)?;
    let mut columns = Vec::with_capacity(obj.len());
    let mut values = Vec::with_capacity(obj.len());
    for (key, json) in obj {
        let value = column_value(table, key, json)?;
        validate_file_write(catalog, table, key, &value)?;
        columns.push(key.clone());
        values.push(Expr::lit(value));
    }
    if columns.is_empty() {
        return Err(Error::invalid("no fields to insert"));
    }
    let mut returning = vec![Projection::all()];
    returning.extend(calc_projections(catalog, table)?);
    let insert = Insert::row(table.name.clone(), columns, values).returning(returning);
    let rows = run_write(catalog, table, Statement::from(insert), context).await?;
    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| Error::not_found("the insert was refused"))?;
    Ok(row_to_json(&row))
}

/// Update the row of `table` whose primary key is `id`, returning the updated
/// row. The primary key addresses the row and is not reassignable through the
/// body.
pub async fn update_row(catalog: &Catalog, table: &Table, id: &str, body: &Json) -> Result<Json> {
    update_row_guarded(catalog, table, id, body, None, None).await
}

/// [`update_row`] with an extra `guard` predicate ANDed into the WHERE —
/// ownership enforcement's translated formula (§7.3) — and an optional RLS
/// caller `context`. A row the guard (or a policy) excludes produces the
/// **same** not-found as a row that is not there: a caller must not be able to
/// probe which rows exist beyond the ones they may reach.
pub(crate) async fn update_row_guarded(
    catalog: &Catalog,
    table: &Table,
    id: &str,
    body: &Json,
    guard: Option<Expr>,
    context: Option<&CallerContext>,
) -> Result<Json> {
    let obj = require_object(body)?;
    reject_calc_writes(table, obj)?;
    let pk = single_pk(table)?;
    let mut assignments = Vec::with_capacity(obj.len());
    for (key, json) in obj {
        if key == &pk {
            continue;
        }
        let value = column_value(table, key, json)?;
        validate_file_write(catalog, table, key, &value)?;
        assignments.push(Assignment::new(key.clone(), Expr::lit(value)));
    }
    if assignments.is_empty() {
        return Err(Error::invalid("no fields to update"));
    }
    let mut returning = vec![Projection::all()];
    returning.extend(calc_projections(catalog, table)?);
    let update = Update {
        table: table.name.clone(),
        assignments,
        filter: Some(guarded_filter(table, &pk, id, guard)?),
        returning,
    };
    let rows = run_write(catalog, table, Statement::from(update), context).await?;
    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| Error::not_found(format!("no row with {pk} = {id}")))?;
    Ok(row_to_json(&row))
}

/// Delete the row of `table` whose primary key is `id`. Deleting a row that is
/// not there is a [`NotFound`](Error::NotFound), not a silent success.
pub async fn delete_row(catalog: &Catalog, table: &Table, id: &str) -> Result<Json> {
    delete_row_guarded(catalog, table, id, None, None).await
}

/// [`delete_row`] with an extra `guard` predicate and optional RLS `context`;
/// same probe-free rule as [`update_row_guarded`].
pub(crate) async fn delete_row_guarded(
    catalog: &Catalog,
    table: &Table,
    id: &str,
    guard: Option<Expr>,
    context: Option<&CallerContext>,
) -> Result<Json> {
    let pk = single_pk(table)?;
    let delete = Delete {
        table: table.name.clone(),
        filter: Some(guarded_filter(table, &pk, id, guard)?),
        returning: vec![Projection::expr(Expr::col(pk.clone()))],
    };
    let rows = run_write(catalog, table, Statement::from(delete), context).await?;
    if rows.is_empty() {
        return Err(Error::not_found(format!("no row with {pk} = {id}")));
    }
    Ok(json!({ "deleted": true }))
}

/// `pk = id`, ANDed with the ownership guard when one applies.
fn guarded_filter(table: &Table, pk: &str, id: &str, guard: Option<Expr>) -> Result<Expr> {
    let base = pk_filter(table, pk, id)?;
    Ok(match guard {
        Some(guard) => base.and(guard),
        None => base,
    })
}

/// Coerce a whole JSON row body through [`column_value`], keyed by column — the
/// values an ownership formula is checked against before an insert or update
/// (§7.3) sees the database. Unknown columns are refused by name, exactly as
/// the write itself would.
pub(crate) fn coerce_row_values(
    table: &Table,
    body: &Json,
) -> Result<std::collections::BTreeMap<String, Value>> {
    let obj = require_object(body)?;
    let mut values = std::collections::BTreeMap::new();
    for (key, json) in obj {
        values.insert(key.clone(), column_value(table, key, json)?);
    }
    Ok(values)
}

/// The stored value of one column of the row addressed by `id` — a single cell,
/// for a caller that needs it for something other than serving the row (an
/// application's file endpoints resolve a `File` field's stored path this way,
/// §4). A missing row is a [`NotFound`](Error::not_found), exactly as
/// [`update_row`] reports one; an unknown column is refused by name.
pub async fn read_field(catalog: &Catalog, table: &Table, column: &str, id: &str) -> Result<Value> {
    read_field_ctx(catalog, table, column, id, None).await
}

/// [`read_field`] routed through an RLS caller context (§7.3) when one is
/// given, so a cell a policy withholds is a not-found rather than a leak.
pub(crate) async fn read_field_ctx(
    catalog: &Catalog,
    table: &Table,
    column: &str,
    id: &str,
    context: Option<&CallerContext>,
) -> Result<Value> {
    if table.field(column).is_none() {
        return Err(Error::invalid(format!(
            "`{}` has no field `{column}`",
            table.name
        )));
    }
    let pk = single_pk(table)?;
    let select = Select::from(Source::table(table.name.clone()))
        .columns(vec![Projection::expr(Expr::col(column))])
        .filter(pk_filter(table, &pk, id)?);
    let rows = run_read(catalog, table, &select, context).await?;
    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| Error::not_found(format!("no row with {pk} = {id}")))?;
    row.values()
        .first()
        .cloned()
        .ok_or_else(|| Error::msg("single-column select returned no column"))
}

/// A row as a JSON object keyed by column name, values in natural JSON.
pub fn row_to_json(row: &Row) -> Json {
    let mut map = Map::with_capacity(row.len());
    for (name, value) in row.columns().iter().zip(row.values()) {
        map.insert(name.clone(), value_to_json(value));
    }
    Json::Object(map)
}

/// The parsed calc-field expressions of `table` (Phase 8), keyed by field name.
/// Re-parses the stored source, which validated cleanly at merge time.
fn calc_map(table: &Table) -> CalcFields {
    table
        .calc_fields()
        .filter_map(|f| {
            let expr = f.calc_expression()?;
            Formula::parse(expr)
                .ok()
                .map(|fm| (f.base.name.clone(), fm))
        })
        .collect()
}

/// Extra `SELECT` projections that compute `table`'s non-stored calculated
/// fields on read (Phase 8), each aliased to its field name and in dependency
/// order (a calc field that reads another inlines it, so the SQL is
/// self-contained). A calc field whose inlined expression does not translate to
/// SQL is **skipped** — computing it needs the reified evaluator, a read-path
/// fallback not yet wired here (a genuinely untranslatable calc expression is
/// the rare case; the built-in field/Ⱶ/Ↄ forms all translate).
pub(crate) fn calc_projections(catalog: &Catalog, table: &Table) -> Result<Vec<Projection>> {
    let calc = calc_map(table);
    if calc.is_empty() {
        return Ok(Vec::new());
    }
    let shape = catalog.schema_shape()?;
    let env = UserEnv::Inline(None);
    let mut out = Vec::new();
    for field in table.calc_fields() {
        let Some(formula) = calc.get(&field.base.name) else {
            continue;
        };
        match translate_value(
            formula,
            &Env::new(&env).with_calc(&calc),
            &shape,
            &table.name,
        ) {
            Ok(expr) => out.push(Projection::expr_as(expr, field.base.name.clone())),
            Err(TranslateError::Untranslatable(_)) => {}
            Err(TranslateError::Error(e)) => return Err(e),
        }
    }
    Ok(out)
}

/// Refuse a write that names a non-stored calculated field — it has no column
/// (Phase 8). Called by insert and update before building the statement.
fn reject_calc_writes(table: &Table, columns: &Map<String, Json>) -> Result<()> {
    for key in columns.keys() {
        if table.field(key).is_some_and(|f| f.is_calc()) {
            return Err(Error::invalid(format!(
                "`{key}` is a calculated field and cannot be written"
            )));
        }
    }
    Ok(())
}

/// Coerce a JSON value for a named column of `table`, validating it against the
/// field's type and attributes, and rejecting unknown columns.
///
/// Two steps (§2.3). First the JSON is coerced to a [`Value`] of the column's
/// **storage** type — the SQL type the column actually has, which a rich type
/// sits on. Then the value is checked against the field's full
/// [`TypeRef`](sc_types::TypeRef) *and* its configured attributes via
/// [`validate_with`](sc_types::TypeRef::validate_with): a basic type checks the
/// value family; a rich type also enforces its attributes (a `String`'s
/// `max_length`/`options`/`regex`, an `Integer`'s `min`/`max`).
///
/// Both failures are reported as an [`Error::invalid`] (an HTTP 400) **naming the
/// field**, because this message is shown to a user of an application, not only
/// to the admin — "`age`: must be at most 120, got 999" lands them on the input
/// to fix.
pub fn column_value(table: &Table, column: &str, json: &Json) -> Result<Value> {
    let field = table
        .field(column)
        .ok_or_else(|| Error::invalid(format!("`{}` has no field `{column}`", table.name)))?;
    let type_ = &field.base.type_;

    let value = json_to_value(&storage_type(type_), json).map_err(|e| field_error(column, e))?;
    type_
        .validate_with(&value, &field.base.attributes)
        .map_err(|e| field_error(column, e))?;
    Ok(value)
}

/// Validate a value written to a `File` field against its store, folder and MIME
/// rules (§3.5). A no-op for any field that is not a `File` kind, or a null/empty
/// value (nullability is a separate check).
///
/// The path must resolve to a **connected** store — an unresolvable store is an
/// error naming the store and the field, because a reference into a store that is
/// not there points at nothing — and its shape must satisfy the field's folder
/// and MIME constraints ([`sc_files::validate_file_path`]). The error names the
/// field, as it is shown to a user of an application, not only the admin.
fn validate_file_write(
    catalog: &Catalog,
    table: &Table,
    column: &str,
    value: &Value,
) -> Result<()> {
    let Some(field) = table.field(column) else {
        return Ok(());
    };
    let DataFieldKind::File {
        store,
        folder,
        mime_allow,
    } = &field.kind
    else {
        return Ok(());
    };
    // A null or empty path is absence, governed by the column's nullability, not
    // by the file rules.
    let path = match value {
        Value::Text(path) if !path.is_empty() => path.as_str(),
        _ => return Ok(()),
    };

    if catalog.file_store(&store.0)?.is_none() {
        return Err(Error::invalid(format!(
            "`{column}`: file store `{}` is not resolvable",
            store.0
        )));
    }
    sc_files::validate_file_path(path, folder.as_deref(), mime_allow)
        .map_err(|e| field_error(column, e))
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

/// Re-raise a value error against a named field, preserving the `Invalid` kind
/// (so it stays a 400) and prefixing the field name (§2.3). The specific
/// violation — from either coercion or validation, both `Invalid` — is kept; any
/// other kind falls back to its full display.
pub(crate) fn field_error(column: &str, e: Error) -> Error {
    let detail = match e.repr() {
        Repr::Invalid(message) => message.clone(),
        _ => e.to_string(),
    };
    Error::invalid(format!("`{column}`: {detail}"))
}

/// The single primary-key column of `table`, or an error when the table has a
/// composite or absent key (row addressing needs exactly one — composite keys
/// are post-MVP).
pub fn single_pk(table: &Table) -> Result<String> {
    match table.primary_key.as_slice() {
        [pk] => Ok(pk.clone()),
        [] => Err(Error::invalid(format!(
            "table `{}` has no primary key to address rows by",
            table.name
        ))),
        _ => Err(Error::invalid(format!(
            "table `{}` has a composite primary key (unsupported for row addressing)",
            table.name
        ))),
    }
}

/// `pk = <id>`, coercing the path-parameter string to the key column's type.
pub(crate) fn pk_filter(table: &Table, pk: &str, id: &str) -> Result<Expr> {
    let value = column_value(table, pk, &Json::String(id.to_owned()))?;
    Ok(Expr::col(pk).eq(Expr::lit(value)))
}

/// Run a `SELECT`, collecting its rows — through an RLS caller-context
/// transaction when `context` is given (§7.3), else on a pooled connection via
/// the table's provider.
pub(crate) async fn run_read(
    catalog: &Catalog,
    table: &Table,
    select: &Select,
    context: Option<&CallerContext>,
) -> Result<Vec<Row>> {
    match context {
        Some(context) => {
            sc_catalog::run_in_context(
                catalog,
                context,
                &Statement::Select(Box::new(select.clone())),
            )
            .await
        }
        None => {
            catalog
                .provider(table)
                .query(select)
                .await?
                .try_collect()
                .await
        }
    }
}

/// Run a write statement, collecting `RETURNING` rows — through an RLS
/// caller-context transaction when `context` is given, else via the provider.
async fn run_write(
    catalog: &Catalog,
    table: &Table,
    statement: Statement,
    context: Option<&CallerContext>,
) -> Result<Vec<Row>> {
    match context {
        Some(context) => sc_catalog::run_in_context(catalog, context, &statement).await,
        None => {
            catalog
                .provider(table)
                .write(&statement)
                .await?
                .try_collect()
                .await
        }
    }
}

/// A JSON body that must be an object, e.g. a row.
pub fn require_object(body: &Json) -> Result<&Map<String, Json>> {
    body.as_object()
        .ok_or_else(|| Error::invalid("expected a JSON object body"))
}
