//! The **plan**: what one terminal of a code body's `db` chain sends, and how it
//! becomes a statement.
//!
//! A plan is a plain JSON object (§7 of the milestone) — `op`, `table`,
//! `authority`, `where`, `select`, `order`, `limit`, `offset` — and that is the
//! whole seam. The fluent surface that produces it is JavaScript living in
//! `sc-expr`'s prelude; a Python adapter (§15) will produce the same objects, and
//! neither of them decides anything: **every** name in a plan is resolved here,
//! against the catalog, and anything else is refused naming it.
//!
//! Two spellings of a filter (§3), one predicate: the object DSL every other
//! surface already speaks — lowered by [`crate::filter`], the same walk an agent's
//! `where` argument goes through — or a formula string, parsed by
//! [`Formula::parse`] and translated by the one symbolic translator. A projection
//! is the same choice: a field name, a Ⱶ-path, or an `{ alias, formula }` object.
//! There is one expression language and this is it, which is why a formula the
//! translator refuses is an error naming it and saying to compute it in the code
//! body — where the author already has JavaScript.
//!
//! Nothing here reaches SQL as text. A column name comes from the catalog, a
//! join path from [`join_path_expr`], an operator from the shared vocabulary, and
//! every literal a body wrote becomes an `Expr::Lit` the query layer
//! parameterises on render.

use std::cell::Cell;
use std::collections::BTreeMap;

use sc_catalog::{Catalog, DataFieldKind, Table};
use sc_error::{Error, Result};
use sc_expr::{
    AggFunc, CalcFields, Env, Formula, JOIN, Operation, TranslateError, UserEnv, aggregate_expr,
    join_path_expr, translate, translate_value,
};
use sc_query::{Expr, OrderBy, Projection, Value};
use serde::Deserialize;
use serde_json::Value as Json;

use crate::convert::value_to_json;
use crate::filter::{self, FilterKey, JoinedColumn};
use crate::ownership::{self, AggregateGuard, JoinAccess};
use crate::rows::{self, RowQuery};

use super::HostLimits;

/// The key that spells the **formula** form of a `where` (§3).
///
/// A **column of this name wins**, as it does against the `and`/`or`/`not`
/// combinators and for the same reason: an application really may have a column
/// called `formula`, and a filter on it must keep meaning what it says. A table
/// that has one simply cannot use the string spelling — it still has the object
/// DSL, which can say everything the plan needs.
const FORMULA: &str = "formula";

// ---------------------------------------------------------------------------
// The wire shape
// ---------------------------------------------------------------------------

/// One terminal of a `db` chain, as it crosses from the guest.
///
/// `deny_unknown_fields` on purpose: a plan carrying something this server does
/// not understand is a guest and a host that disagree about the seam, and
/// answering it anyway would answer a question nobody asked.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    /// Which operation. `select` and `aggregate` read; the rest write.
    pub op: Op,
    /// The table, resolved against the catalog and nowhere else.
    pub table: String,
    /// Whose authority this runs under (§5). Admin unless the body delegated.
    #[serde(default)]
    pub authority: Authority,
    /// The filter: the object DSL, or `{ "formula": "…" }`.
    #[serde(default, rename = "where")]
    pub filter: Option<Json>,
    /// The projections. Empty is the whole row.
    #[serde(default)]
    pub select: Vec<Selection>,
    /// `ORDER BY` keys, in precedence order.
    #[serde(default)]
    pub order: Vec<OrderKey>,
    /// Grouping keys (phase 6).
    #[serde(default)]
    pub group: Vec<String>,
    /// The bound the body asked for.
    #[serde(default)]
    pub limit: Option<u64>,
    /// How many rows to skip first.
    #[serde(default)]
    pub offset: Option<u64>,
    /// The primary key `.get(pk)` named.
    #[serde(default)]
    pub pk: Option<Json>,
    /// What an `aggregate` asks for, each aliased by the key it rides back under.
    #[serde(default)]
    pub aggregate: Vec<AggSpec>,
    /// An insert's row(s), or an update's assignments (phase 3).
    #[serde(default)]
    pub values: Option<Json>,
}

/// The five operations a plan may be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// Rows.
    Select,
    /// One row of aggregate values.
    Aggregate,
    /// Write new rows.
    Insert,
    /// Change matched rows.
    Update,
    /// Remove matched rows.
    Delete,
}

/// Whose authority a plan runs under (§5): the trigger's own by default,
/// the event's caller when the body said `asUser()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    /// The trigger's: `ROLE_ADMIN`, carrying the event's user. The default,
    /// because a trigger is server-side configuration and the audit row a caller
    /// may not insert is the archetype of what a trigger exists to write.
    #[default]
    Admin,
    /// The event's caller, through `sc_api::ownership`'s `*_as` functions.
    User,
}

/// One projection: a name (a column, a calculated field or a Ⱶ-path), or a
/// formula under an alias.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Selection {
    /// `"title"`, `"customerⱵemail"`.
    Name(String),
    /// `{ alias: "spend", formula: "ordersↃcustomer.sum(o => o.total)" }`.
    Formula {
        /// The key the value rides back under.
        alias: String,
        /// The expression, in the one formula language.
        formula: String,
    },
}

/// One `ORDER BY` key.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderKey {
    /// A column, a calculated field, a Ⱶ-path or a formula.
    pub field: String,
    /// Which way. Ascending unless the body said otherwise.
    #[serde(default)]
    pub dir: Dir,
}

/// A sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dir {
    /// Ascending — what `.orderBy(field)` with no direction means.
    #[default]
    Asc,
    /// Descending.
    Desc,
}

/// One aggregate value a plan asks for.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggSpec {
    /// The key it rides back under (`"value"` for the scalar terminals).
    pub alias: String,
    /// `count`, `sum`, `avg`, `min` or `max`.
    #[serde(rename = "fn")]
    pub func: String,
    /// The field or formula aggregated; absent for `count`.
    #[serde(default)]
    pub arg: Option<String>,
}

// ---------------------------------------------------------------------------
// A validated read
// ---------------------------------------------------------------------------

/// A `select` plan, resolved: the read the row layer runs, and the shape its
/// values are answered in.
pub(crate) struct Read {
    /// The table being read.
    pub(crate) table: Table,
    /// The read itself.
    pub(crate) query: RowQuery,
    /// The keys the answer carries, or `None` for the whole row.
    keys: Option<Vec<String>>,
}

impl Read {
    /// One read row as the **REST wire shape** — `value_to_json`, so a Decimal is
    /// exact and a Date is ISO, and a row means the same thing in
    /// `db.books.rows()` as it does over HTTP.
    pub(crate) fn row(&self, values: &BTreeMap<String, Value>) -> Json {
        let Some(keys) = &self.keys else {
            return ownership::table_row_json(&self.table, values);
        };
        let mut map = serde_json::Map::with_capacity(keys.len());
        for key in keys {
            map.insert(
                key.clone(),
                values.get(key).map_or(Json::Null, value_to_json),
            );
        }
        Json::Object(map)
    }
}

/// An `aggregate` plan, resolved.
pub(crate) struct Aggregate {
    /// The table being aggregated.
    pub(crate) table: Table,
    /// One projection per requested value, aliased by its key.
    pub(crate) projections: Vec<Projection>,
    /// Which rows it ranges over.
    pub(crate) filter: Option<Expr>,
}

/// Resolve a `select` plan against the catalog.
pub(crate) fn read(cat: &Catalog, plan: &Plan, limits: &HostLimits, role: u8) -> Result<Read> {
    let table = cat.require(&plan.table)?;
    let low = Lowering::new(cat, &table, role)?;
    let mut filter = low.filter(plan.filter.as_ref())?;
    if let Some(pk) = &plan.pk {
        let expr = pk_expr(&table, pk)?;
        filter = Some(match filter {
            Some(existing) => existing.and(expr),
            None => expr,
        });
    }

    let mut query = RowQuery::new().where_(filter);
    let mut keys = Vec::with_capacity(plan.select.len());
    let mut extra = Vec::new();
    for selection in &plan.select {
        match selection {
            Selection::Name(name) => {
                keys.push(name.clone());
                // A column of the table (or a calculated field) is already in the
                // `SELECT` the row layer builds; only a path or an expression is
                // a projection this read has to add.
                if let Some(expr) = low.projected(name)? {
                    extra.push(Projection::expr_as(expr, name.clone()));
                }
            }
            Selection::Formula { alias, formula } => {
                keys.push(alias.clone());
                extra.push(Projection::expr_as(
                    low.formula_value(formula)?,
                    alias.clone(),
                ));
            }
        }
    }
    if !extra.is_empty() {
        query = query.projecting(extra);
    }

    let mut order = Vec::with_capacity(plan.order.len());
    for key in &plan.order {
        let expr = low.value_expr(&key.field)?;
        order.push(match key.dir {
            Dir::Asc => OrderBy::asc(expr),
            Dir::Desc => OrderBy::desc(expr),
        });
    }
    query = query.order_by(order);

    refuse_grouping(plan)?;
    query = query.limit(bounded_limit(plan, limits, &table)?);
    if let Some(offset) = plan.offset {
        query = query.offset(offset);
    }
    let query = query.requiring_caller_context(low.in_caller_context.get());
    Ok(Read {
        table,
        query,
        keys: (!plan.select.is_empty()).then_some(keys),
    })
}

/// Resolve an `aggregate` plan against the catalog.
pub(crate) fn aggregate(cat: &Catalog, plan: &Plan, role: u8) -> Result<Aggregate> {
    let table = cat.require(&plan.table)?;
    let low = Lowering::new(cat, &table, role)?;
    let filter = low.filter(plan.filter.as_ref())?;
    refuse_grouping(plan)?;
    if plan.aggregate.is_empty() {
        return Err(Error::invalid(format!(
            "this aggregate names no value to compute over `{}`",
            table.name
        )));
    }
    let mut projections = Vec::with_capacity(plan.aggregate.len());
    for spec in &plan.aggregate {
        let func = agg_func(&spec.func, &table)?;
        let value = match &spec.arg {
            Some(arg) => Some(low.value_expr(arg)?),
            None if matches!(func, AggFunc::Count) => None,
            None => {
                return Err(Error::invalid(format!(
                    "`{}` over `{}` needs a field or a formula to compute over",
                    spec.func, table.name
                )));
            }
        };
        projections.push(Projection::expr_as(
            aggregate_expr(&func, false, value, &table.name)?,
            spec.alias.clone(),
        ));
    }
    // An aggregate is one `SELECT` with no room for a per-row decision, so a
    // projection that would need the caller's GUCs set cannot be honoured here
    // the way a row read's can. Refused rather than answered with a number the
    // policies never saw.
    if low.in_caller_context.get() && !table.rls_enabled {
        return Err(Error::invalid(format!(
            "this aggregate over `{}` reaches a table with row-level security, which an \
             aggregate cannot carry the caller into — read the rows and aggregate them in \
             your code body instead",
            table.name
        )));
    }
    Ok(Aggregate {
        table,
        projections,
        filter,
    })
}

/// The row cap, applied to the plan's own bound.
///
/// A read is materialised into the isolate, so an unbounded `.rows()` on a large
/// table is an out-of-memory rather than a slow query. The bound the body asked
/// for is honoured up to the cap and **refused** above it — never silently
/// lowered, because a body that asked for 5000 rows and got 1000 would go on to
/// compute a wrong answer from a right-looking one. The read itself asks for one
/// row more than the cap so that an unbounded read can tell "exactly the cap"
/// from "more than we may return".
fn bounded_limit(plan: &Plan, limits: &HostLimits, table: &Table) -> Result<u64> {
    match plan.limit {
        Some(n) if n > limits.max_rows => Err(Error::invalid(format!(
            "a code body may read {} rows at once, and this asked `{}` for {n}; \
             narrow the `.where()` or lower the `.limit()`",
            limits.max_rows, table.name
        ))),
        Some(n) => Ok(n),
        None => Ok(limits.max_rows.saturating_add(1)),
    }
}

/// Grouped aggregation is phase 6; until then a `group` is refused rather than
/// quietly ignored, which would answer one row where the body expected many.
fn refuse_grouping(plan: &Plan) -> Result<()> {
    match plan.group.is_empty() {
        true => Ok(()),
        false => Err(Error::invalid(
            "`.groupBy()` is not available yet: aggregate in your code body, or \
             aggregate once per group",
        )),
    }
}

/// The aggregate a plan named, by the names the chain's terminals use.
fn agg_func(name: &str, table: &Table) -> Result<AggFunc> {
    match name {
        "count" => Ok(AggFunc::Count),
        "sum" => Ok(AggFunc::Sum),
        "avg" => Ok(AggFunc::Avg),
        "min" => Ok(AggFunc::Min),
        "max" => Ok(AggFunc::Max),
        other => Err(Error::invalid(format!(
            "`{other}` is not an aggregate over `{}` — they are count, sum, avg, min, max",
            table.name
        ))),
    }
}

/// `pk = <value>` for a `.get(pk)`, coerced against the key column.
fn pk_expr(table: &Table, pk: &Json) -> Result<Expr> {
    let key = rows::single_pk(table)?;
    let value = rows::column_value(table, &key, pk)?;
    Ok(Expr::col(key).eq(Expr::lit(value)))
}

// ---------------------------------------------------------------------------
// Resolving names
// ---------------------------------------------------------------------------

/// What every name in a plan is resolved through: the catalog, the schema shape
/// and this caller's role.
///
/// Its methods take `&self` and record their one side effect — that the
/// statement has to run inside a caller-context transaction, because something
/// it reaches has row-level security — in a [`Cell`], so a resolver can be
/// handed to the shared filter walk as a plain closure.
struct Lowering<'a> {
    catalog: &'a Catalog,
    /// The table being read. Owned (a `Table` is cheap next to a query) so that
    /// the caller may hand its own copy on to the read it is building.
    table: Table,
    /// Every field of the table: the code host has no allow-list, unlike an
    /// agent's tool, because a code body is the admin's own configuration.
    fields: Vec<String>,
    shape: sc_expr::SchemaShape,
    calc: CalcFields,
    user_env: UserEnv,
    role: u8,
    in_caller_context: Cell<bool>,
}

impl<'a> Lowering<'a> {
    fn new(catalog: &'a Catalog, table: &Table, role: u8) -> Result<Lowering<'a>> {
        Ok(Lowering {
            catalog,
            fields: table.fields.iter().map(|f| f.base.name.clone()).collect(),
            shape: catalog.schema_shape()?,
            calc: table.calc_formulas(),
            table: table.clone(),
            // A formula in a plan reads the row and nothing else — see
            // `guard`, which refuses `user` by name rather than inlining a null.
            user_env: UserEnv::Inline(None),
            role,
            in_caller_context: Cell::new(false),
        })
    }

    /// The predicate a plan's `where` means — the object DSL and the formula
    /// spelling, which the shared walk reaches through one resolver so that the
    /// two can be **mixed**: repeated `.where()` calls arrive as
    /// `{ and: [ { … }, { formula: "…" } ] }`.
    fn filter(&self, where_: Option<&Json>) -> Result<Option<Expr>> {
        filter::where_resolved(&self.table, &self.fields, where_, &|key, condition| {
            self.filter_key(key, condition)
        })
    }

    /// A filter key the table does not declare: the formula spelling, or a
    /// Ⱶ-path as the column it compares.
    fn filter_key(&self, key: &str, condition: &Json) -> Result<Option<FilterKey>> {
        if key == FORMULA
            && let Some(source) = condition.as_str()
        {
            return Ok(Some(FilterKey::Predicate(self.formula_predicate(source)?)));
        }
        if !key.contains(JOIN) {
            return Ok(None);
        }
        let (target, column) = self.walk(key)?;
        Ok(Some(FilterKey::Joined(Box::new(JoinedColumn {
            expr: join_path_expr(&self.shape, &self.table.name, key).map_err(Error::from)?,
            column,
            table: target,
        }))))
    }

    /// The projection a selected **name** needs, or `None` when the row already
    /// carries it (a column of the table, or a calculated field the row layer
    /// projects itself).
    fn projected(&self, name: &str) -> Result<Option<Expr>> {
        match self.table.field(name) {
            Some(_) => Ok(None),
            None if is_plain_name(name) => Err(Error::invalid(format!(
                "`{}` has no field `{name}` to select",
                self.table.name
            ))),
            None => Ok(Some(self.value_expr(name)?)),
        }
    }

    /// One name in **value** position — a column, a calculated field, a Ⱶ-path or
    /// an expression. What `.orderBy(…)` and `.sum(…)` take, resolved the one
    /// way, so a formula means the same thing wherever a plan puts one.
    fn value_expr(&self, source: &str) -> Result<Expr> {
        match self.table.field(source) {
            Some(field) if !field.is_calc() => Ok(Expr::col(source)),
            // A calculated field has no column; its own expression is what the
            // database computes, inlined by the translator's calc environment.
            Some(_) => self.formula_value(source),
            None if is_plain_name(source) => Err(Error::invalid(format!(
                "`{}` has no field `{source}`",
                self.table.name
            ))),
            None => self.formula_value(source),
        }
    }

    /// A formula in value position: parsed, validated against the schema shape
    /// with this table's row as the bare scope, guarded, and translated.
    fn formula_value(&self, source: &str) -> Result<Expr> {
        let formula = self.parse(source)?;
        match translate_value(&formula, &self.env(), &self.shape, &self.table.name) {
            Ok(expr) => Ok(expr),
            Err(e) => Err(self.translation_failed(source, e)),
        }
    }

    /// A formula in predicate position — the string spelling of a `where`.
    fn formula_predicate(&self, source: &str) -> Result<Expr> {
        let formula = self.parse(source)?;
        match translate(
            &formula,
            Operation::Read,
            &self.env(),
            &self.shape,
            &self.table.name,
        ) {
            Ok(expr) => Ok(expr),
            Err(e) => Err(self.translation_failed(source, e)),
        }
    }

    /// Parse and validate one formula, and check what it reaches.
    fn parse(&self, source: &str) -> Result<Formula> {
        let formula = Formula::parse(source)
            .map_err(|e| Error::invalid(format!("`{source}` is not a formula: {e}")))?;
        let analysis = formula.validate(&self.shape, &self.table.name)?;
        // The scope is the row's own fields. `user`, `row`, `old` and `payload`
        // are bindings of the *code body*, which is JavaScript and can splice
        // whatever it likes into the plan — a formula that named one would be a
        // second, weaker way to say something the body already says better.
        if let Some(ambient) = analysis.ambient_outside(&[]) {
            return Err(Error::invalid(format!(
                "`{source}` reads `{}`, which a formula in a code body has no scope for: \
                 its scope is the row of `{}`. Read `{}` in the code body and pass the value \
                 into the filter instead",
                ambient.as_str(),
                self.table.name,
                ambient.as_str(),
            )));
        }
        for path in &analysis.join_paths {
            self.walk(&path.ident)?;
        }
        for use_ in &analysis.agg_uses {
            self.guard_child(&use_.child_table)?;
        }
        Ok(formula)
    }

    /// The environment a plan's formulas translate in.
    fn env(&self) -> Env<'_> {
        Env::new(&self.user_env).with_calc(&self.calc)
    }

    /// A translation failure, said in terms of the plan that caused it.
    ///
    /// The untranslatable case is the interesting one: there is exactly one
    /// expression language, and the part of it the database cannot do is the part
    /// the *code body* is for. So the message names the formula and says where to
    /// put it, which costs the author nothing.
    fn translation_failed(&self, source: &str, e: TranslateError) -> Error {
        match e {
            TranslateError::Untranslatable(what) => Error::invalid(format!(
                "`{source}` cannot be computed by the database ({what}) — compute it in your \
                 code body instead, which is JavaScript and can"
            )),
            TranslateError::Error(e) => e,
        }
    }

    /// Follow a Ⱶ-path from this table, checking each hop the way a read of the
    /// target table would be checked. Answers the final table and column, which
    /// is what a filter literal is coerced against.
    ///
    /// [`ownership::join_guard`] is the check, and it is the same one the REST
    /// `select` embeds and the GraphQL key fields go through: a joined row is
    /// *read*, so the target's read rule holds, and a caller whose access comes
    /// from an ownership formula is refused by name rather than handed a withheld
    /// row one column at a time.
    fn walk(&self, ident: &str) -> Result<(Table, String)> {
        let mut table = self.table.clone();
        let mut segments = ident.split(JOIN).peekable();
        while let Some(segment) = segments.next() {
            let field = table.field(segment).ok_or_else(|| {
                Error::invalid(format!(
                    "`{}` has no field `{segment}` (in `{ident}`)",
                    table.name
                ))
            })?;
            if segments.peek().is_none() {
                return Ok((table.clone(), segment.to_owned()));
            }
            let DataFieldKind::Key { target_table, .. } = &field.kind else {
                return Err(Error::invalid(format!(
                    "`{}`.`{segment}` is not a key to another table, so `{ident}` joins nothing",
                    table.name
                )));
            };
            let target = self.catalog.require(&target_table.0)?;
            match ownership::join_guard(&target, self.role)? {
                JoinAccess::Unrestricted => {}
                JoinAccess::InContext => self.in_caller_context.set(true),
            }
            table = target;
        }
        // Unreachable: a path with no Ⱶ has one segment, which returns above.
        Err(Error::invalid(format!("`{ident}` is not a join path")))
    }

    /// Whether a Ↄ-aggregation over `child` may be computed for this caller —
    /// [`ownership::aggregate_guard`]'s question, asked for the same reason: the
    /// aggregate is a correlated subquery with no room for a per-row decision.
    fn guard_child(&self, child: &str) -> Result<()> {
        let child = self.catalog.require(child)?;
        match ownership::aggregate_guard(self.catalog, &child, &child.name, self.role, None)? {
            AggregateGuard::InContext => self.in_caller_context.set(true),
            AggregateGuard::Predicate(None) => {}
            AggregateGuard::Predicate(Some(_)) => {
                return Err(Error::invalid(format!(
                    "`{}` cannot be aggregated for you here: your access to it comes from its \
                     ownership formula, and the correlated subquery this becomes has no room \
                     for that decision — read `{}` directly instead",
                    child.name, child.name
                )));
            }
        }
        Ok(())
    }
}

/// Whether a name is an ordinary identifier — no Ⱶ, no Ↄ, no operators — and so
/// something that should have been a field of the table rather than an
/// expression to parse.
///
/// The distinction is only about the **message**: `nope` is a typo and gets
/// "`books` has no field `nope`", while `qty * price` is an expression and gets
/// whatever the formula language says about it.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}
