//! Tables in a code body: the host behind `db` (§10.1, the milestone "Tables in
//! code").
//!
//! `sc-expr` runs the JavaScript and knows nothing about tables — its
//! [`CodeHost`] seam is one JSON request in, one JSON value out, which is what
//! makes it the seam §15's other guest languages implement rather than a
//! JavaScript feature. [`TableHost`] is the implementation of that seam for *this*
//! server's tables, and it lives here because everything it needs already does:
//! the catalog, the formula translator, the shared filter vocabulary, §7.3's
//! ownership rule and the row layer whose writes are events.
//!
//! # What crosses
//!
//! One [`Plan`] per terminal — `db.books.where(…).limit(10).rows()` is one plan
//! and one round trip. The guest builds it; **nothing in it is trusted**. The
//! table is resolved through the catalog, every column against that table, every
//! Ⱶ-path through [`ownership::join_guard`](crate::ownership), every formula
//! through the one translator, and anything else is refused naming it: a plan
//! carries names and values, and the statement is built here.
//!
//! # The one exception, named
//!
//! `db.sql("select …", [args])` sends a [`SqlPlan`] instead, and its text is the
//! statement. It exists because the row layer's read does not express everything
//! — a window function, a recursive CTE, an `ON CONFLICT` — and a code body that
//! has to leave the server to ask is not an escape hatch. It is the **same**
//! admission §13.4's custom SQL queries are, on the same grounds and with the
//! same paragraph of consequences: a `run_js_code` body is server-side
//! configuration written by an administrator, so the rule on
//! [`Statement::Raw`](sc_query::Statement::Raw) — raw SQL is authored, never
//! assembled from what a caller sent — holds by construction. The body's own
//! values reach it as **binds** and nowhere else.
//!
//! What that costs is exactly the list of things the paragraph above is: no
//! ownership formula filters it, no rich type coerces what it returns, no
//! `File`-field rule governs it, and a write inside one raises **no table
//! event** — so a trigger will not see it, and the row layer's `db.books.insert`
//! is still the way to write a row that other triggers are meant to notice. What
//! does still hold is the caller-context transaction (an RLS-protected table's
//! policies decide), the row cap and the call budget.
//!
//! # Authority
//!
//! A code body's reads and writes are the **admin's** by default (§5), carrying
//! the event's user so an RLS policy that reads `user` still sees who caused it —
//! the rule `rows_scope.rs` already states for `insert_row`/`update_rows`: a
//! trigger is server-side configuration, and an audit row the caller may not
//! insert is the archetype of what a trigger exists to write.
//!
//! `asUser()` sets one field of the plan and **delegates to the event's caller**
//! instead. Every operation then goes through [`crate::ownership`]'s `*_as`
//! functions at the event's own role and user — §7.3's rule, the same functions
//! the agent tools go through, with no second implementation of "meets the floor
//! OR the formula grants it" here to be subtly wrong. So a delegated read is
//! narrowed to the rows the ownership formula grants (in the `WHERE` where it
//! can be, row by row where it cannot), a delegated update is checked on the row
//! as it is *and* on the row as it would become, a withheld row is the same
//! **not found** an absent row gets, and an RLS table is read and written inside
//! a caller-context transaction where the database's own policies decide.
//!
//! Delegation means less for `db.sql()` than it does for the chain, and the
//! difference is stated where the operation is: it runs the statement at the
//! caller's role and user, which is what row-level security reads — and nothing
//! else, because an ownership formula is applied by a row layer raw SQL does not
//! go through.
//!
//! Events differ in whom they have to delegate to, and that difference is
//! honoured rather than hidden: a table event or a directly-run trigger carries
//! the user who caused it, while a scheduled or startup trigger carries nobody
//! and therefore reads as the **public** role. Which is why `asAdmin()` is the
//! default.
//!
//! # Bounds
//!
//! Two of the three are here rather than in the guest, because a bound the guest
//! could edit is not a bound: the **row cap** (a read is materialised into the
//! isolate, so an unbounded `.rows()` on a large table is an out-of-memory) and
//! the **call budget** (an accidental N+1 must not hammer the database quietly).
//! The third — the wall clock — is the runtime's, since only it can stop a body
//! that never asks for anything.
//!
//! # Streaming
//!
//! `.iter()` is the answer to the row cap rather than an exception to it. It
//! sends one plan per **batch**, each a read of its own resumed from where the
//! last stopped, so the cap holds on every one of them and what a body may walk
//! is bounded by the call budget instead — which is the honest bound, because
//! what a stream spends is round trips and not memory.
//!
//! Resuming is a **keyset** on the read's own ordering, built in [`plan`]: the
//! primary key is appended to it so the order is total, null placement is stated
//! so the predicate and the `ORDER BY` cannot disagree, and the cursor is the
//! sort values of the last row read — projected into the same `SELECT` under
//! reserved aliases, because a `.select()` narrows what comes back and a Ⱶ-path
//! is no column of the table at all. Nothing in a cursor is trusted: it is
//! coerced against the column it sorts on exactly as a `.where()` literal is.
//!
//! Delegation is where a stream can be refused, and it is §5's asymmetry again:
//! a batch is bounded by the database, so the caller's read rule has to reach
//! inside the statement. RLS policies do and a translatable ownership formula
//! does; a formula only the evaluator can decide does not, and a batch of it
//! would fetch the whole table, hand back what survived and do it again — so it
//! is refused naming `.rows()`, which can decide row by row.

mod files;
mod plan;
mod triggers;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use sc_auth::{ROLE_ADMIN, ROLE_PUBLIC, User};
use sc_catalog::{CallerContext, Catalog, Table};
use sc_error::{Error, Repr, Result};
use sc_expr::{CodeHost, DEFAULT_MAX_HOST_CALLS, JsEvaluator};
use serde_json::Value as Json;

use crate::convert::value_to_json;
use crate::ownership;
use crate::rows;

pub use files::{FileStoreHost, MAX_COPY_BYTES, MAX_FILE_BYTES};
pub use plan::{AggSpec, Authority, Dir, Op, OrderKey, Plan, Selection, SqlOp, SqlPlan};
pub use triggers::TriggerRunHost;

/// How many rows one read may return before it is refused.
///
/// A read is materialised into the isolate: an unbounded `.rows()` over a large
/// table is an out-of-memory, not a slow query, and the error says to add a
/// `.limit()` because that is the fix. A table that has to be walked whole is
/// walked with `.iter()`, whose every batch is bounded by this same cap.
pub const DEFAULT_MAX_ROWS: u64 = 1000;

/// What one run of a code body may spend against the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostLimits {
    /// The most rows one read may return.
    pub max_rows: u64,
    /// The most database calls one run may make.
    pub max_calls: u32,
}

impl Default for HostLimits {
    fn default() -> Self {
        HostLimits {
            max_rows: DEFAULT_MAX_ROWS,
            max_calls: DEFAULT_MAX_HOST_CALLS,
        }
    }
}

/// The `db` handle of one code-body run, on the host's side.
///
/// **One per run.** The call budget is counted on it, and the event's caller and
/// trigger chain ride on every statement it makes — so sharing one between runs
/// would share a budget between them and attribute one run's writes to another's
/// cascade.
pub struct TableHost<'a> {
    /// The catalog every name is resolved through.
    ///
    /// **Borrowed**: the catalog is what the row layer, an action's context and
    /// every request handler already hold a reference to, and threading an `Arc`
    /// down to a firing trigger would mean threading one through the write that
    /// fired it. `sc_expr::CodeCall` therefore borrows its host too, and the code
    /// runtime bridges the borrow across to its isolate thread.
    catalog: &'a Catalog,
    /// The role the event was served at — what a delegated operation is checked
    /// against, and public for an event with no caller at all.
    role: u8,
    /// The event's caller, as the event carries it. Attached to every statement:
    /// on an RLS table it is what the policies read, and off one it is what the
    /// **event** this run raises reports as its cause. Under `asUser()` it is
    /// also read back as an [`sc_auth::User`], which is what §7.3's rule takes.
    user: Option<Json>,
    /// The triggers that led here, including the one running. A write this host
    /// makes carries it, so `Event::firing`'s cascade bound applies to a code
    /// body exactly as it does to an action.
    chain: Vec<String>,
    /// The bounds.
    limits: HostLimits,
    /// The formula engine, for a delegated operation whose ownership formula the
    /// translator refuses and which therefore has to be decided row by row.
    ///
    /// A code thread blocks on the host call while this runs, on the *formula*
    /// isolate — which is exactly why the two runtimes are separate (decision 1):
    /// one isolate serving both would deadlock here, waiting for itself.
    evaluator: Option<Arc<dyn JsEvaluator>>,
    /// How many calls this run has made.
    calls: AtomicU32,
}

impl<'a> TableHost<'a> {
    /// A handle over `catalog`, with the default bounds and no caller.
    pub fn new(catalog: &'a Catalog) -> TableHost<'a> {
        TableHost {
            catalog,
            role: ROLE_PUBLIC,
            user: None,
            chain: Vec::new(),
            limits: HostLimits::default(),
            evaluator: None,
            calls: AtomicU32::new(0),
        }
    }

    /// Attach the event's caller: the role it was served at and the user's own
    /// fields, exactly as `Event::caller` carries them.
    ///
    /// Both, rather than the user alone, because the two answer different
    /// questions: the role is what a delegated operation's floor is checked
    /// against, and the fields are what an ownership formula and an RLS policy
    /// read. An event with no caller keeps the public role, which is what
    /// `asUser()` in a scheduled trigger honestly means.
    #[must_use]
    pub fn caused_by(mut self, role: u8, user: Option<Json>) -> TableHost<'a> {
        self.role = role;
        self.user = user;
        self
    }

    /// Give delegated operations the formula engine, for an ownership formula the
    /// translator cannot lower.
    #[must_use]
    pub fn with_evaluator(mut self, evaluator: Option<Arc<dyn JsEvaluator>>) -> TableHost<'a> {
        self.evaluator = evaluator;
        self
    }

    /// Attach the trigger chain that led here.
    #[must_use]
    pub fn chained(mut self, chain: Vec<String>) -> TableHost<'a> {
        self.chain = chain;
        self
    }

    /// Set the bounds, in place of [`HostLimits::default`].
    #[must_use]
    pub fn with_limits(mut self, limits: HostLimits) -> TableHost<'a> {
        self.limits = limits;
        self
    }

    /// Answer one plan.
    async fn run(&self, plan: &Plan) -> Result<Json> {
        // Resolved once per plan, and before anything is looked up: the authority
        // decides the role every name in the plan is resolved *at*, so a
        // delegated read of a table the caller may not reach through a key is
        // refused by the same guard a REST embed is.
        let actor = self.actor(plan.authority)?;
        match plan.op {
            Op::Select => self.select(plan, &actor).await,
            Op::Aggregate => self.aggregate(plan, &actor).await,
            Op::Insert => self.insert(plan, &actor).await,
            Op::Update | Op::Delete => self.write(plan, &actor).await,
        }
    }

    /// Whose authority this plan runs under, as the value the operations dispatch
    /// on. Delegation reads the event's caller back as an [`sc_auth::User`] here,
    /// so a caller object that is not one is refused before any statement runs.
    fn actor(&self, authority: Authority) -> Result<Actor> {
        match authority {
            Authority::Admin => Ok(Actor::Admin(self.caller())),
            Authority::User => Ok(Actor::Caller {
                role: self.role,
                user: User::from_json(self.role, self.user.as_ref())?,
            }),
        }
    }

    /// A `select`: the rows, as the REST wire shape.
    ///
    /// One batch of an `.iter()` is the same read with two differences, both of
    /// them in [`plan::read`]: it sorts by a **total** order, and it answers
    /// `{ rows, cursor }` so the guest knows where the next batch starts and
    /// whether there is one.
    async fn select(&self, plan: &Plan, actor: &Actor) -> Result<Json> {
        let read = plan::read(self.catalog, plan, &self.limits, actor.role())?;
        if read.cursor.is_some()
            && let Actor::Caller { role, user } = actor
        {
            self.streamable(&read.table, *role, user.as_ref())?;
        }
        let values = match actor {
            Actor::Admin(context) => {
                rows::list_row_values(self.catalog, &read.table, &read.query, Some(context)).await?
            }
            // §7.3's rule, in the one place it lives: the formula narrows the
            // rows in the `WHERE` where it can and row by row where it cannot,
            // and the bound is applied after it either way.
            Actor::Caller { role, user } => {
                ownership::read_row_values_as(
                    self.catalog,
                    &read.table,
                    &read.query,
                    *role,
                    user.as_ref(),
                    self.evaluator.as_ref(),
                )
                .await?
            }
        };
        // The cap is enforced on what came back rather than by trimming it: a
        // body handed 1000 of 4000 rows would go on to compute a wrong answer out
        // of a right-looking one, and never know.
        self.within_cap(
            values.len(),
            &format!("reading `{}` returned", read.table.name),
            "add a `.limit()` or narrow the `.where()`",
        )?;
        let rows = Json::Array(values.iter().map(|v| read.row(v)).collect());
        let Some(cursor) = &read.cursor else {
            return Ok(rows);
        };
        let mut out = serde_json::Map::with_capacity(2);
        out.insert("rows".to_owned(), rows);
        out.insert("cursor".to_owned(), cursor.next(&values));
        Ok(Json::Object(out))
    }

    /// An `aggregate`: one object keyed by the aliases the plan asked for, or —
    /// when the plan grouped — one such object **per group**, with the group keys
    /// beside the values.
    ///
    /// The two are one path (§phase 6): `.count()` is
    /// `.aggregate({ value: "count()" })` with nothing to group by, so the filter,
    /// the caller context and §5's refusal are decided once rather than twice.
    /// Only the shape of the answer differs, and it differs because the questions
    /// do: a scalar terminal answers a value, and a grouped one answers rows.
    async fn aggregate(&self, plan: &Plan, actor: &Actor) -> Result<Json> {
        let agg = plan::aggregate(self.catalog, plan, &self.limits, actor.role())?;
        let values = match actor {
            Actor::Admin(context) => {
                rows::aggregate_grouped(
                    self.catalog,
                    &agg.table,
                    agg.projections,
                    &agg.query,
                    Some(context),
                )
                .await?
            }
            Actor::Caller { role, user } => {
                self.delegable_aggregate(&agg.table, *role, user.as_ref())?;
                ownership::aggregate_grouped_as(
                    self.catalog,
                    &agg.table,
                    agg.projections,
                    &agg.query,
                    *role,
                    user.as_ref(),
                )
                .await?
            }
        };
        let group = |values: &std::collections::BTreeMap<String, sc_query::Value>| {
            let mut out = serde_json::Map::with_capacity(agg.keys.len());
            for key in &agg.keys {
                out.insert(
                    key.clone(),
                    values.get(key).map_or(Json::Null, value_to_json),
                );
            }
            Json::Object(out)
        };
        if !agg.grouped {
            // A scalar aggregate is one row by construction; an empty answer is
            // the database saying something else, and reads back as nulls rather
            // than as invented zeroes.
            return Ok(group(&values.first().cloned().unwrap_or_default()));
        }
        // A grouped aggregate answers rows, so it is bounded like a read: a body
        // that grouped a million-row table by a free-text column would otherwise
        // materialise a million groups into the isolate.
        self.within_cap(
            values.len(),
            &format!("grouping `{}` produced", agg.table.name),
            "add a `.limit()` or narrow the `.where()`",
        )?;
        Ok(Json::Array(values.iter().map(group).collect()))
    }

    /// A `db.sql(…)`: the body's own SQL, and its rows as JSON objects keyed by
    /// the column names the database reported.
    ///
    /// **The one thing in `db` that is not resolved through the catalog**, and
    /// the module documentation's paragraph on it says what that costs. What is
    /// still true here is the part that does not depend on knowing the tables:
    /// the arguments are **binds**, so a value that spells SQL is a value; the
    /// row cap applies, because these rows are materialised into the isolate
    /// exactly as a `.rows()`'s are; the call is one of the run's budget; and the
    /// statement runs inside the same caller-context transaction every other
    /// operation does, so an RLS-protected table's policies still decide what it
    /// can see.
    ///
    /// What `asUser()` means here is therefore **narrower than it is anywhere
    /// else**, and narrow in a way worth saying out loud: it runs the statement
    /// at the caller's role and user, which is exactly what row-level security
    /// reads — and nothing more. An ownership *formula* is applied by the row
    /// layer, and raw SQL does not go through the row layer, so it does not
    /// filter this. A delegated `db.sql()` over a table that is owned by formula
    /// rather than by RLS sees the whole table; the body that wants §7.3's rule
    /// wants the chain, which is why the chain is the default surface and this is
    /// the escape hatch.
    async fn sql(&self, plan: &SqlPlan) -> Result<Json> {
        let statement = plan::statement(self.catalog, plan)?;
        let context = self.actor(plan.authority)?.context(&self.chain);
        let rows = sc_catalog::run_in_context(self.catalog, &context, &statement).await?;
        self.within_cap(
            rows.len(),
            "this `db.sql()` returned",
            "add a `LIMIT` to the statement",
        )?;
        Ok(crate::rest::custom::rows_to_json(&rows))
    }

    /// An `insert`: the written row, or an array of them for an array in.
    ///
    /// Through [`rows::create_row_ctx`], which is the whole point — a code body's
    /// write is coerced against its columns, validated, File-field-checked and
    /// **observed by triggers** exactly as a write through the API is. It carries
    /// this run's caller, so the event it raises says who caused it and how deep
    /// in a cascade it already is.
    ///
    /// Delegated, the same write goes through [`ownership::insert_row_as`] —
    /// §7.3's write rule, the proposed row checked against the ownership formula
    /// with `_insert` folded true — and then through the very same row layer, so a
    /// delegated write is an event exactly as an admin's is.
    async fn insert(&self, plan: &Plan, actor: &Actor) -> Result<Json> {
        let insertion = plan::insert(self.catalog, plan, &self.limits)?;
        let mut written = Vec::with_capacity(insertion.rows.len());
        for row in &insertion.rows {
            written.push(match actor {
                Actor::Admin(context) => {
                    rows::create_row_ctx(self.catalog, &insertion.table, row, Some(context)).await?
                }
                Actor::Caller { role, user } => {
                    ownership::insert_row_as(
                        self.catalog,
                        &insertion.table,
                        row,
                        *role,
                        user.as_ref(),
                        self.evaluator.as_ref(),
                        &self.chain,
                    )
                    .await?
                }
            });
        }
        Ok(match insertion.many {
            true => Json::Array(written),
            false => written.into_iter().next().unwrap_or(Json::Null),
        })
    }

    /// An `update` or a `delete`: `{ updated | deleted, ids }`, the shape the
    /// `update_rows` and `delete_rows` actions already answer.
    ///
    /// The matched rows are resolved **first** and then written one at a time
    /// through the row layer, exactly as those actions do — the events are the
    /// point (§4). No transaction spans the two (§6): a body that fails half way
    /// leaves the rows it already wrote, and their events have already gone out.
    ///
    /// Delegated, **the rows are resolved through the delegated read** and each
    /// one written through [`ownership::update_row_as`] /
    /// [`ownership::delete_row_as`]. Both halves matter: a row the caller cannot
    /// see must never be a row they can reach by predicate, and an update must be
    /// granted on the row as it is *and* on the row as it would become.
    async fn write(&self, plan: &Plan, actor: &Actor) -> Result<Json> {
        let write = plan::write(self.catalog, plan, &self.limits, actor.role())?;
        let table = &write.matched.table;
        let values = match actor {
            Actor::Admin(context) => {
                rows::list_row_values(self.catalog, table, &write.matched.query, Some(context))
                    .await?
            }
            Actor::Caller { role, user } => {
                ownership::read_row_values_as(
                    self.catalog,
                    table,
                    &write.matched.query,
                    *role,
                    user.as_ref(),
                    self.evaluator.as_ref(),
                )
                .await?
            }
        };
        let verb = match plan.op {
            Op::Update => "update",
            _ => "delete",
        };
        self.within_cap(
            values.len(),
            &format!("this {verb} of `{}` matched", table.name),
            "add a `.limit()` or narrow the `.where()`",
        )?;

        let mut ids = Vec::with_capacity(values.len());
        for row in &values {
            let (id, key) = rows::row_key(table, &write.pk, row)?;
            match (actor, &write.values) {
                (Actor::Admin(context), Some(assignments)) => {
                    rows::update_row_ctx(self.catalog, table, &id, assignments, Some(context))
                        .await?;
                }
                (Actor::Admin(context), None) => {
                    rows::delete_row_ctx(self.catalog, table, &id, Some(context)).await?;
                }
                (Actor::Caller { role, user }, Some(assignments)) => {
                    ownership::update_row_as(
                        self.catalog,
                        table,
                        &id,
                        assignments,
                        *role,
                        user.as_ref(),
                        self.evaluator.as_ref(),
                        &self.chain,
                    )
                    .await?;
                }
                (Actor::Caller { role, user }, None) => {
                    ownership::delete_row_as(
                        self.catalog,
                        table,
                        &id,
                        *role,
                        user.as_ref(),
                        self.evaluator.as_ref(),
                        &self.chain,
                    )
                    .await?;
                }
            }
            ids.push(key);
        }
        let count = Json::from(ids.len());
        let mut out = serde_json::Map::with_capacity(2);
        out.insert(
            match plan.op {
                Op::Update => "updated".to_owned(),
                _ => "deleted".to_owned(),
            },
            count,
        );
        out.insert("ids".to_owned(), Json::Array(ids));
        Ok(Json::Object(out))
    }

    /// The row cap, asserted on rows that are already in hand — a read's answer,
    /// the rows a bulk write matched, or what the body's own SQL returned.
    ///
    /// Both are refusals rather than truncations, and for the same reason: a body
    /// handed 1000 of 4000 rows computes a wrong answer out of a right-looking
    /// one, and a body that updated 1000 of 4000 rows would report having done
    /// what it was asked.
    fn within_cap(&self, rows: usize, clause: &str, fix: &str) -> Result<()> {
        if rows as u64 > self.limits.max_rows {
            return Err(Error::invalid(format!(
                "{clause} more than the {} rows a code body may take at once; {fix}",
                self.limits.max_rows
            )));
        }
        Ok(())
    }

    /// The caller every statement of this run carries: **admin, in the event's
    /// user's name**, with the trigger chain that led here.
    fn caller(&self) -> CallerContext {
        CallerContext::new(ROLE_ADMIN, self.user.clone()).chained(self.chain.clone())
    }

    /// §5's one asymmetry, said in the code body's own terms.
    ///
    /// A delegated aggregate over a table whose ownership formula the translator
    /// refuses cannot be answered: [`ownership::aggregate_guard`] will not fall
    /// back to the evaluator, because an aggregate over rows it cannot filter
    /// would silently count rows the caller may not see, and a plausible number
    /// hides that where a refusal cannot. The guard is asked here — ahead of
    /// [`ownership::aggregate_values_as`], which asks it again and would answer in
    /// its own words — for one reason: from here the message can name the way out,
    /// and in a code body there is one.
    ///
    /// A caller who may not read the table at all gets that answer unchanged: it
    /// is an `auth` refusal, the same one a read gets, and reading the rows
    /// instead would not help them.
    fn delegable_aggregate(&self, table: &Table, role: u8, user: Option<&User>) -> Result<()> {
        match ownership::aggregate_guard(self.catalog, table, &table.name, role, user) {
            Ok(_) => Ok(()),
            Err(e) => Err(match e.repr() {
                Repr::Invalid(message) => Error::invalid(format!(
                    "{message}. Read the rows with `.rows()` and aggregate them in your code \
                     body instead, which can decide row by row"
                )),
                _ => e,
            }),
        }
    }

    /// Whether a **delegated** read of `table` can be streamed — §5's asymmetry
    /// again, in the one other place it bites.
    ///
    /// Streaming a read means the database bounding it: each batch is a `LIMIT`
    /// over the rows the caller may see, resumed from where the last one
    /// stopped. That works when the ownership rule reaches the statement — RLS
    /// policies do, a translatable formula does — and cannot when the rule is a
    /// formula only the evaluator can decide, because then the bound is applied
    /// *after* the rows come back ([`ownership::read_row_values_as`] says so, and
    /// says why). A batch of 1000 would fetch the whole table, hand back
    /// whatever survived the formula, and do it again for the next batch: not
    /// streaming, and quadratic while pretending otherwise.
    ///
    /// [`ownership::aggregate_guard`] is exactly this question already asked —
    /// "can this caller's read rule be carried inside the statement?" — so it is
    /// the thing asked, rather than a second copy of the rule to be subtly
    /// wrong. Only the wording is this surface's own: an aggregate's way out is
    /// not a stream's.
    fn streamable(&self, table: &Table, role: u8, user: Option<&User>) -> Result<()> {
        match ownership::aggregate_guard(self.catalog, table, &table.name, role, user) {
            Ok(_) => Ok(()),
            Err(e) => Err(match e.repr() {
                Repr::Invalid(_) => Error::invalid(format!(
                    "`{}` cannot be streamed as this user: its ownership formula has to be \
                     decided row by row, so a batch of it cannot be bounded by the database. \
                     Read it with `.rows()` and a `.limit()`, which can decide row by row",
                    table.name
                )),
                _ => e,
            }),
        }
    }

    /// Spend one of the run's database calls.
    ///
    /// The runtime counts these too, and both counts are wanted: the guest's
    /// stops the loop before the plan is built, and this one is the number that
    /// is true — a host shared or a prelude tampered with cannot lower it.
    fn spend_call(&self) -> Result<()> {
        if self.calls.fetch_add(1, Ordering::SeqCst) >= self.limits.max_calls {
            return Err(Error::invalid(format!(
                "this code made more than {} database calls in one run; the bound exists so \
                 an accidental loop cannot hammer the database",
                self.limits.max_calls
            )));
        }
        Ok(())
    }
}

/// Whose authority one plan runs under, resolved (§5).
///
/// Two shapes and only two, which is what makes every operation's dispatch a
/// two-armed match rather than a flag threaded through it: the **trigger's own**
/// authority, already built as the caller context every statement of an
/// undelegated run carries, and the **event's caller**, as the role and user
/// §7.3's rule takes.
enum Actor {
    /// Admin, in the event's user's name, with the chain that led here.
    Admin(CallerContext),
    /// The event's caller. `user` is `None` for an event that carries nobody — a
    /// scheduled or startup trigger — which then reads as the public role.
    Caller {
        /// The role every floor is checked against.
        role: u8,
        /// The user an ownership formula and an RLS policy read.
        user: Option<User>,
    },
}

impl Actor {
    /// The role every name in the plan is resolved at — a Ⱶ-path's hops, a
    /// Ↄ-aggregation's child table — as well as every row rule.
    fn role(&self) -> u8 {
        match self {
            Actor::Admin(_) => ROLE_ADMIN,
            Actor::Caller { role, .. } => *role,
        }
    }

    /// The caller context a statement of this authority runs under — the GUCs an
    /// RLS policy reads, and the chain a write's event carries.
    ///
    /// The chain-and-row-layer operations never ask: `Admin` already **is** the
    /// context they were built from, and a delegated one is built by
    /// `ownership`'s own `*_as` functions. `db.sql()` asks, because it is the one
    /// operation that reaches the database without either of those in front of it.
    fn context(&self, chain: &[String]) -> CallerContext {
        match self {
            Actor::Admin(context) => context.clone(),
            Actor::Caller { role, user } => {
                ownership::caller_context_at(*role, user.as_ref()).chained(chain.to_vec())
            }
        }
    }
}

#[async_trait]
impl CodeHost for TableHost<'_> {
    async fn call(&self, request: Json) -> Result<Json> {
        self.spend_call()?;
        let refuse = |e: serde_json::Error| {
            Error::invalid(format!(
                "this database request is not one the server understands: {e}"
            ))
        };
        // Two shapes cross this seam, and the `op` says which before either is
        // read: a plan over a table, and the body's own SQL — which shares
        // nothing with a plan but the authority. Told apart here rather than by
        // an untagged enum, so a malformed request is refused in the words of the
        // shape it was trying to be.
        if request.get("op").and_then(Json::as_str) == Some("sql") {
            let plan: SqlPlan = serde_json::from_value(request).map_err(refuse)?;
            return self.sql(&plan).await;
        }
        let plan: Plan = serde_json::from_value(request).map_err(refuse)?;
        self.run(&plan).await
    }
}

#[cfg(test)]
mod tests;
