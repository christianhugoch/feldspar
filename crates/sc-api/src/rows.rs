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

use sc_catalog::{CallerContext, Catalog, DataFieldKind, Table, TableWrite, WriteOp};
use sc_db::Row;
use sc_error::{Error, Repr, Result};
use sc_expr::{CalcFields, Env, Formula, TranslateError, UserEnv, translate_value};
use sc_query::{
    Assignment, BinOp, Delete, Expr, Insert, OrderBy, Projection, Select, Source, Statement,
    Update, Value,
};
use sc_types::{BasicType, TypeRef};
use serde_json::{Map, Value as Json};

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
    list_rows_query(catalog, table, &RowQuery::new().where_(filter), context).await
}

/// Which rows of a table to read, in what order, and how many at most.
///
/// The shape of a read that is not "everything": a filter, an ordering and a
/// bound. It exists because an agent's `query_table` tool (§11.3) asks for
/// exactly those three and must not be able to ask for anything else — a tool
/// that could name its own projection or its own SQL would be a second row layer
/// with none of this one's rules. Every field is optional, so
/// [`RowQuery::new`] is "every row, in no particular order".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RowQuery {
    /// `WHERE`, if any. Ownership enforcement ANDs its own predicate into this.
    pub filter: Option<Expr>,
    /// `ORDER BY` keys, in precedence order.
    pub order: Vec<OrderBy>,
    /// The most rows to return.
    pub limit: Option<u64>,
    /// How many rows to skip first.
    pub offset: Option<u64>,
    /// Extra projections computed in the **same** `SELECT` as the row, each
    /// aliased to the name the caller will read the value back by.
    ///
    /// What a GraphQL `manager { email }` lowers to (§13.4): the Ⱶ-join's
    /// correlated subquery, projected as another column of this query rather
    /// than fetched in a second one. It is the same trick ownership's reified
    /// path already uses for the join values a formula reads — one query serves
    /// the row and everything derived from it.
    pub extra: Vec<Projection>,
    /// A bound applied **within each group** of rows rather than to the read as
    /// a whole. `None` for every ordinary read.
    pub partition: Option<Partition>,
    /// Whether this read must run inside a caller-context transaction even
    /// though its own table is not protected by row-level security.
    ///
    /// Ordinarily the **table** decides (see `in_context`): its policies are the
    /// only thing a `SET LOCAL` here would be read by. But an [`extra`](Self::extra)
    /// projection can be a correlated subquery over a *different* table — a
    /// GraphQL `employees_aggregate { count }` inside a `departments` read
    /// (§13.4) — and if that table is RLS-protected, its policies apply to the
    /// subquery and see whatever an unset GUC makes of them: no caller, so no
    /// rows, so a count of zero nobody would investigate. Setting this makes the
    /// statement a caller-context one so the child's policies decide as they
    /// would in a read of their own.
    ///
    /// It only ever *adds* a transaction; it never removes a table's own.
    pub in_caller_context: bool,
}

/// At most `limit` rows for each distinct value of `by`, after skipping
/// `offset` of them, in the query's own order.
///
/// The one thing a batched child list needs that `LIMIT` cannot express: one
/// `SELECT` answers `employees(limit: 3)` for *every* department at once, and
/// "three each" is not "three". It lowers to `row_number() OVER (PARTITION BY
/// … ORDER BY …)` numbered inside the read and filtered outside it — after the
/// ownership predicate has been ANDed in, so the numbering never counts a row
/// the caller may not see.
#[derive(Debug, Clone, PartialEq)]
pub struct Partition {
    /// The column the rows are grouped by — a child table's key, in the one
    /// case that needs this.
    pub by: String,
    /// The most rows to return per group.
    pub limit: Option<u64>,
    /// How many rows to skip in each group first.
    pub offset: Option<u64>,
}

/// The alias the per-partition row number rides back under. Structural — chosen
/// here, never user data — and prefixed so it cannot be a column of a table an
/// admin declared.
const PARTITION_ROW_NUMBER: &str = "_sc_rn";

/// The alias the partitioned read's inner query is exposed under. Same rule.
const PARTITION_SOURCE: &str = "_sc_part";

impl RowQuery {
    /// Every row, unordered and unbounded.
    pub fn new() -> RowQuery {
        RowQuery::default()
    }

    /// Restrict to the rows matching `filter` (`None` leaves it unrestricted).
    pub fn where_(mut self, filter: Option<Expr>) -> RowQuery {
        self.filter = filter;
        self
    }

    /// Order by these keys.
    pub fn order_by(mut self, order: Vec<OrderBy>) -> RowQuery {
        self.order = order;
        self
    }

    /// Return at most `n` rows.
    pub fn limit(mut self, n: u64) -> RowQuery {
        self.limit = Some(n);
        self
    }

    /// Skip the first `n` rows.
    pub fn offset(mut self, n: u64) -> RowQuery {
        self.offset = Some(n);
        self
    }

    /// Project `extra` alongside the row's own columns.
    pub fn projecting(mut self, extra: Vec<Projection>) -> RowQuery {
        self.extra = extra;
        self
    }

    /// Bound the rows *within each group* of `partition` rather than as a whole.
    pub fn per_partition(mut self, partition: Partition) -> RowQuery {
        self.partition = Some(partition);
        self
    }

    /// Require this read to run inside a caller-context transaction — see
    /// [`in_caller_context`](Self::in_caller_context). `false` leaves the
    /// decision where it normally lives, with the table.
    pub fn requiring_caller_context(mut self, required: bool) -> RowQuery {
        self.in_caller_context = required;
        self
    }

    /// This query with `extra` ANDed into its filter — how an ownership
    /// predicate joins a caller's own one without either being able to drop the
    /// other.
    pub fn and_filter(mut self, extra: Expr) -> RowQuery {
        self.filter = Some(match self.filter {
            Some(existing) => existing.and(extra),
            None => extra,
        });
        self
    }
}

/// [`list_rows_where`] with an ordering and a bound: the general read.
pub async fn list_rows_query(
    catalog: &Catalog,
    table: &Table,
    query: &RowQuery,
    context: Option<&CallerContext>,
) -> Result<Json> {
    let rows = run_read(
        catalog,
        table,
        &read_select(catalog, table, query)?,
        context,
        query.in_caller_context,
    )
    .await?;
    Ok(Json::Array(rows.iter().map(row_to_json).collect()))
}

/// [`list_rows_query`] as **query values** keyed by column rather than as the
/// JSON wire shape — the same statement, the same RLS routing, read by a caller
/// that needs the values typed.
///
/// A GraphQL read is that caller: `Decimal` must reach the wire exact rather
/// than through a JSON number, and the [`RowQuery::extra`] projections it adds
/// are not columns of the table, so the JSON renderer would drop them.
pub async fn list_row_values(
    catalog: &Catalog,
    table: &Table,
    query: &RowQuery,
    context: Option<&CallerContext>,
) -> Result<Vec<std::collections::BTreeMap<String, Value>>> {
    let rows = run_read(
        catalog,
        table,
        &read_select(catalog, table, query)?,
        context,
        query.in_caller_context,
    )
    .await?;
    Ok(rows.iter().map(row_values).collect())
}

/// The one row an **aggregate** read returns: `aggregates` projected over the
/// rows of `table` that `filter` leaves, with no `GROUP BY`.
///
/// The counterpart of [`list_row_values`] for a question about rows rather than
/// about a row. It is a separate function because an aggregate `SELECT` is not a
/// row read with extra columns: `SELECT *, count(*)` is not a legal statement,
/// and the calculated fields have nothing to compute over here. Every projection
/// is the caller's, aliased by the key it will be read back under — the
/// expressions themselves come from `sc_expr`'s shared builder, so the wire and
/// a formula answer the same question the same way.
///
/// A scalar aggregate always has exactly one row; an empty result would mean the
/// database answered something else, and the empty map that comes back then
/// resolves as nulls rather than as invented zeroes.
pub async fn aggregate_values(
    catalog: &Catalog,
    table: &Table,
    aggregates: Vec<Projection>,
    filter: Option<Expr>,
    context: Option<&CallerContext>,
) -> Result<std::collections::BTreeMap<String, Value>> {
    let mut select = Select::from(Source::table(table.name.clone())).columns(aggregates);
    if let Some(filter) = filter {
        select = select.filter(filter);
    }
    let rows = run_read(catalog, table, &select, context, false).await?;
    Ok(rows.first().map(row_values).unwrap_or_default())
}

/// The alias `count_rows` reads its one value back under. Structural — chosen
/// here, never user data — so it cannot collide with a column an admin declared.
const ROW_COUNT_KEY: &str = "_sc_count";

/// How many rows `table` has, as [`aggregate_values`] answers it.
///
/// A count rather than the length of a listing: the admin's table page shows
/// this beside the link to the rows, and reading every row of a large table to
/// find out how many there are would make the page cost what the data costs.
/// It goes through the same read path, so an RLS-enforced table counts the rows
/// the caller may see rather than the rows that exist.
pub async fn count_rows(
    catalog: &Catalog,
    table: &Table,
    context: Option<&CallerContext>,
) -> Result<i64> {
    let count = sc_expr::aggregate_expr(&sc_expr::AggFunc::Count, false, None, &table.name)?;
    let values = aggregate_values(
        catalog,
        table,
        vec![Projection::expr_as(count, ROW_COUNT_KEY)],
        None,
        context,
    )
    .await?;
    match values.get(ROW_COUNT_KEY) {
        Some(Value::Int(n)) => Ok(*n),
        // `count(*)` is `bigint` in Postgres and an integer everywhere else, so
        // anything but an int here means the driver mapped it to something this
        // does not know about rather than that the table is empty.
        other => Err(Error::msg(format!(
            "`count(*)` over `{}` came back as {other:?}",
            table.name
        ))),
    }
}

/// The `SELECT` one [`RowQuery`] renders to: every column plus the calculated
/// fields and the query's own extra projections, filtered, ordered and bounded.
fn read_select(catalog: &Catalog, table: &Table, query: &RowQuery) -> Result<Select> {
    let mut columns = vec![Projection::all()];
    columns.extend(calc_projections(catalog, table)?);
    columns.extend(query.extra.iter().cloned());
    let Some(partition) = &query.partition else {
        let mut select = Select::from(Source::table(table.name.clone())).columns(columns);
        if let Some(filter) = query.filter.clone() {
            select = select.filter(filter);
        }
        select.order = query.order.clone();
        select.limit = query.limit;
        select.offset = query.offset;
        return Ok(select);
    };

    // A per-group bound is not a `LIMIT`: the rows have to be *numbered* first,
    // inside the same read that the filter (and so the ownership predicate)
    // applies to, and the numbering compared afterwards. Hence the wrap.
    columns.push(Projection::expr_as(
        Expr::row_number(vec![Expr::col(partition.by.clone())], query.order.clone()),
        PARTITION_ROW_NUMBER,
    ));
    let mut inner = Select::from(Source::table(table.name.clone())).columns(columns);
    if let Some(filter) = query.filter.clone() {
        inner = inner.filter(filter);
    }
    let mut select = Select::from(Source::Subquery {
        query: Box::new(inner),
        alias: PARTITION_SOURCE.to_owned(),
    });
    let rn = || Expr::col(PARTITION_ROW_NUMBER);
    let skip = partition.offset.unwrap_or(0);
    let mut bound = (skip > 0).then(|| Expr::binary(BinOp::Gt, rn(), Expr::lit(skip as i64)));
    if let Some(take) = partition.limit {
        let last = skip.saturating_add(take);
        let within = Expr::binary(BinOp::Le, rn(), Expr::lit(last as i64));
        bound = Some(match bound {
            Some(b) => b.and(within),
            None => within,
        });
    }
    select.filter = bound;
    select.order = query.order.clone();
    select.limit = query.limit;
    select.offset = query.offset;
    Ok(select)
}

/// The text a row (or a caller looking one up) is grouped by on one column.
///
/// [`Value`] is not `Hash` — it carries floats and decimals — and a key column
/// is small, so grouping goes through its `Debug` rendering, which distinguishes
/// `Int(1)` from `Text("1")` and so cannot collapse two different keys into one
/// bucket. An absent column groups as `NULL`, which is where it belongs: a child
/// row whose key is null belongs to no parent.
pub(crate) fn group_key(value: Option<&Value>) -> String {
    format!("{:?}", value.unwrap_or(&Value::Null))
}

/// One fetched row as a map from column name to its value.
pub(crate) fn row_values(row: &Row) -> std::collections::BTreeMap<String, Value> {
    row.columns()
        .iter()
        .cloned()
        .zip(row.values().iter().cloned())
        .collect()
}

/// The rows of `table` matching `filter` as **query values** keyed by column,
/// including any non-stored calculated field.
///
/// The evaluation-side counterpart of [`list_rows_where`]: same rows, same RLS
/// routing, but as the `Value` map a formula is evaluated against rather than the
/// JSON wire shape. This is what an action's `where` predicate selects with — it
/// needs the values *typed* to bind them, and it needs the calculated fields
/// because a formula may read one.
pub async fn select_values(
    catalog: &Catalog,
    table: &Table,
    filter: Option<Expr>,
    context: Option<&CallerContext>,
) -> Result<Vec<std::collections::BTreeMap<String, Value>>> {
    let mut columns = vec![Projection::all()];
    columns.extend(calc_projections(catalog, table)?);
    let mut select = Select::from(Source::table(table.name.clone())).columns(columns);
    if let Some(filter) = filter {
        select = select.filter(filter);
    }
    let fetched = run_read(catalog, table, &select, context, false).await?;
    Ok(fetched.iter().map(row_values).collect())
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
    let row = row_to_json(&row);
    emit(catalog, table, WriteOp::Insert, &row, None, context).await;
    Ok(row)
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
    // The pre-image, for the event's `old_row` — read **only when something
    // listens** (§10.2's emit seam), so an update on a table with no update
    // trigger costs exactly what it always did. There is no transaction around
    // the pair: dispatch is after-commit by design (decision 1), and a row that
    // changed in between is a race the event reports rather than prevents.
    let old_row = match catalog.observes_writes(&table.name, WriteOp::Update) {
        true => read_row(catalog, table, &pk, id, context).await?,
        false => None,
    };
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
    let row = row_to_json(&row);
    emit(catalog, table, WriteOp::Update, &row, old_row, context).await;
    Ok(row)
}

/// Delete the row of `table` whose primary key is `id`, returning **the row as
/// it was**. Deleting a row that is not there is a
/// [`NotFound`](Error::NotFound), not a silent success.
///
/// The statement has to read the row back anyway — a delete event carries the
/// only copy of it anyone will ever get — so returning it costs nothing and is
/// the one moment it can be had. What a *caller* does with it is theirs to
/// decide: the REST projection answers `{"deleted": true}`, because that is its
/// wire contract, and the GraphQL one answers with the row, because that is
/// what `delete_X_by_pk: X` promised. Neither shape belongs here.
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
        // The whole row, not just the key: a delete event carries the row **as it
        // was**, which is the only copy of it anyone will ever get. No calc
        // projections — those are correlated subqueries, and correlating them
        // against a row being deleted in the same statement is a question with no
        // good answer.
        returning: vec![Projection::all()],
    };
    let rows = run_write(catalog, table, Statement::from(delete), context).await?;
    let Some(row) = rows.first() else {
        return Err(Error::not_found(format!("no row with {pk} = {id}")));
    };
    let row = row_to_json(row);
    emit(catalog, table, WriteOp::Delete, &row, None, context).await;
    Ok(row)
}

/// Raise the event one committed write is (§10.2), if anything is listening.
///
/// Two properties this function exists to hold, both of them in the TODO's words:
///
/// - **A write nobody observes pays nothing.** The `observes_writes` lookup comes
///   first, so the row is not even cloned for a table with no trigger on it.
/// - **A failing trigger does not fail the request or lose the write.** The write
///   has already committed by the time this runs, so an error here is *reported*
///   and the row still goes back to the caller. The dispatcher reports each
///   trigger's own failure; what reaches here is dispatch itself failing.
async fn emit(
    catalog: &Catalog,
    table: &Table,
    op: WriteOp,
    row: &Json,
    old_row: Option<Json>,
    caller: Option<&CallerContext>,
) {
    if !catalog.observes_writes(&table.name, op) {
        return;
    }
    let write = TableWrite {
        table,
        op,
        row: row.clone(),
        old_row,
        caller,
    };
    if let Err(e) = catalog.emit_write(write).await {
        eprintln!(
            "saltcorn: dispatching the {op} event for `{}`: {}",
            table.name,
            sc_error::format_chain(&e)
        );
    }
}

/// One row by primary key, as the JSON an event carries — the pre-image an
/// update's `old_row` needs. `None` when no such row (or none the caller may
/// see, which for an event is the same thing: the update will not match either).
async fn read_row(
    catalog: &Catalog,
    table: &Table,
    pk: &str,
    id: &str,
    context: Option<&CallerContext>,
) -> Result<Option<Json>> {
    let filter = pk_filter(table, pk, id)?;
    let rows = list_rows_where(catalog, table, Some(filter), context).await?;
    Ok(rows.as_array().and_then(|rows| rows.first()).cloned())
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
    let rows = run_read(catalog, table, &select, context, false).await?;
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
fn calc_projections(catalog: &Catalog, table: &Table) -> Result<Vec<Projection>> {
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

/// A row the database **returned** — an insert's or a delete's `RETURNING` —
/// back as the typed values a reader works in.
///
/// The inverse of [`row_to_json`], and deliberately not [`column_value`]: these
/// values came out of the column, so there is nothing to validate them against.
/// Holding a returned row to the field's attribute rules would refuse a row the
/// database already holds — a `max_length` tightened after the row was written
/// is the admin's problem to fix, not a reason a delete cannot say what it
/// removed.
///
/// A column the table does not declare, and one whose text will not parse as
/// its declared type, is carried as the JSON it arrived as rather than dropped:
/// a value nobody asked about must not silently disappear on the way back.
pub(crate) fn json_row_values(
    table: &Table,
    row: &Json,
) -> std::collections::BTreeMap<String, Value> {
    let mut values = std::collections::BTreeMap::new();
    let Json::Object(obj) = row else {
        return values;
    };
    for (name, json) in obj {
        let typed = table
            .field(name)
            .map(|field| storage_type(&field.base.type_))
            .and_then(|basic| sc_types::json_to_value(&basic, json).ok())
            .unwrap_or_else(|| match json {
                Json::Null => Value::Null,
                other => Value::Json(other.clone()),
            });
        values.insert(name.clone(), typed);
    }
    values
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

/// Whether this statement runs inside a caller-context transaction: **the table
/// decides**, not the caller — unless the statement itself reaches a table that
/// does.
///
/// It used to be "whenever a context was given", which worked only while the
/// context existed for RLS alone. Now the caller travels with every write (an
/// event has to say who caused it), so a `Some` on an ordinary table must not
/// silently wrap it in a transaction and a `SET LOCAL` no policy will ever read.
/// A context is still *required* to reach the policies: without one they see
/// `NULL` and deny, which is the fail-closed shape §7.3 depends on.
///
/// `reaches_rls` is the second half of that rule, and it is a property of the
/// *statement*: a read of an ordinary table that projects a correlated subquery
/// over an RLS-protected one runs that table's policies, so it needs the GUCs
/// too (see [`RowQuery::in_caller_context`]).
fn in_context<'a>(
    table: &Table,
    context: Option<&'a CallerContext>,
    reaches_rls: bool,
) -> Option<&'a CallerContext> {
    context.filter(|_| table.rls_enabled || reaches_rls)
}

/// Run a `SELECT`, collecting its rows — through an RLS caller-context
/// transaction on an RLS table (§7.3) or one whose subqueries reach one, else on
/// a pooled connection via the table's provider.
pub(crate) async fn run_read(
    catalog: &Catalog,
    table: &Table,
    select: &Select,
    context: Option<&CallerContext>,
    reaches_rls: bool,
) -> Result<Vec<Row>> {
    match in_context(table, context, reaches_rls) {
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
/// caller-context transaction on an RLS table, else via the provider.
async fn run_write(
    catalog: &Catalog,
    table: &Table,
    statement: Statement,
    context: Option<&CallerContext>,
) -> Result<Vec<Row>> {
    match in_context(table, context, false) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use sc_catalog::DbId;
    use sc_db::PhysicalTable;

    fn table(rls_enabled: bool) -> Table {
        let mut table = Table::from_physical(
            DbId::primary(),
            &PhysicalTable {
                name: "books".into(),
                schema: None,
                columns: Vec::new(),
                primary_key: vec!["id".into()],
                foreign_keys: Vec::new(),
            },
        );
        table.rls_enabled = rls_enabled;
        table
    }

    /// The rule the caller context changed meaning under: it is *who is writing*
    /// (every write has one, so an event can say who caused it), and whether the
    /// statement runs in a GUC transaction is the **table's** business.
    ///
    /// Worth its own test because both halves are silent failures. Routing an
    /// ordinary write through a policy transaction costs a transaction and a
    /// `SET LOCAL` no policy will ever read; *not* routing an RLS one leaves the
    /// GUCs unset, which every generated policy reads as no access — the
    /// fail-closed shape §7.3 depends on.
    #[test]
    fn the_table_decides_the_caller_context_transaction_not_the_caller() {
        let caller = CallerContext::anonymous(1);
        assert!(in_context(&table(true), Some(&caller), false).is_some());
        assert!(in_context(&table(false), Some(&caller), false).is_none());
        assert!(in_context(&table(true), None, false).is_none());
    }

    /// …and the one thing that is *not* the table's business: a statement whose
    /// own subqueries reach a protected table.
    ///
    /// A GraphQL read of an ordinary `departments` that projects
    /// `employees_aggregate { count }` over an RLS-protected `employees` runs
    /// the employees' policies inside the departments' statement. Without the
    /// GUCs those policies see no caller and grant nothing, and the aggregate
    /// comes back `0` — a number that looks like an answer. So the *statement*
    /// gets to ask for the transaction the table did not need.
    #[test]
    fn a_statement_reaching_a_protected_table_asks_for_the_transaction_itself() {
        let caller = CallerContext::anonymous(1);
        assert!(in_context(&table(false), Some(&caller), true).is_some());
        // Still fail-closed on the other half: no context is no transaction,
        // whatever the statement reaches.
        assert!(in_context(&table(false), None, true).is_none());
    }
}
