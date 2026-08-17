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
//! through the one translator, and anything else is refused naming it. There is
//! no raw-SQL escape hatch, and no way to add one from the guest: a plan carries
//! names and values, and the statement is built here.
//!
//! # Authority
//!
//! A code body's reads and writes are the **admin's** by default (§5), carrying
//! the event's user so an RLS policy that reads `user` still sees who caused it —
//! the rule `rows_scope.rs` already states for `insert_row`/`update_rows`: a
//! trigger is server-side configuration, and an audit row the caller may not
//! insert is the archetype of what a trigger exists to write. `asUser()` sets one
//! field of the plan and delegates instead; that path arrives in phase 4.
//!
//! # Bounds
//!
//! Two of the three are here rather than in the guest, because a bound the guest
//! could edit is not a bound: the **row cap** (a read is materialised into the
//! isolate, so an unbounded `.rows()` on a large table is an out-of-memory) and
//! the **call budget** (an accidental N+1 must not hammer the database quietly).
//! The third — the wall clock — is the runtime's, since only it can stop a body
//! that never asks for anything.

mod plan;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use sc_auth::ROLE_ADMIN;
use sc_catalog::{CallerContext, Catalog};
use sc_error::{Error, Result};
use sc_expr::{CodeHost, DEFAULT_MAX_HOST_CALLS};
use serde_json::Value as Json;

use crate::convert::value_to_json;
use crate::rows;

pub use plan::{AggSpec, Authority, Dir, Op, OrderKey, Plan, Selection};

/// How many rows one read may return before it is refused.
///
/// A read is materialised into the isolate: an unbounded `.rows()` over a large
/// table is an out-of-memory, not a slow query, and the error says to add a
/// `.limit()` because that is the fix.
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
pub struct TableHost {
    /// The catalog every name is resolved through.
    catalog: Arc<Catalog>,
    /// The event's caller, as the event carries it. Attached to every statement:
    /// on an RLS table it is what the policies read, and off one it is what the
    /// **event** this run raises reports as its cause.
    user: Option<Json>,
    /// The triggers that led here, including the one running. A write this host
    /// makes carries it, so `Event::firing`'s cascade bound applies to a code
    /// body exactly as it does to an action (phase 3).
    chain: Vec<String>,
    /// The bounds.
    limits: HostLimits,
    /// How many calls this run has made.
    calls: AtomicU32,
}

impl TableHost {
    /// A handle over `catalog`, with the default bounds and no caller.
    pub fn new(catalog: Arc<Catalog>) -> TableHost {
        TableHost {
            catalog,
            user: None,
            chain: Vec::new(),
            limits: HostLimits::default(),
            calls: AtomicU32::new(0),
        }
    }

    /// Attach the event's caller.
    #[must_use]
    pub fn caused_by(mut self, user: Option<Json>) -> TableHost {
        self.user = user;
        self
    }

    /// Attach the trigger chain that led here.
    #[must_use]
    pub fn chained(mut self, chain: Vec<String>) -> TableHost {
        self.chain = chain;
        self
    }

    /// Set the bounds, in place of [`HostLimits::default`].
    #[must_use]
    pub fn with_limits(mut self, limits: HostLimits) -> TableHost {
        self.limits = limits;
        self
    }

    /// Answer one plan.
    async fn run(&self, plan: &Plan) -> Result<Json> {
        match plan.authority {
            Authority::Admin => {}
            // Phase 4. Said as what it is: a body that delegates today would
            // otherwise silently get the admin's rows, which is the one wrong
            // answer this whole milestone is careful not to give.
            Authority::User => {
                return Err(Error::invalid(
                    "`asUser()` is not available yet: this code body's database access runs \
                     as the trigger, not as the event's caller",
                ));
            }
        }
        match plan.op {
            Op::Select => self.select(plan).await,
            Op::Aggregate => self.aggregate(plan).await,
            Op::Insert | Op::Update | Op::Delete => Err(Error::invalid(
                "writing tables from a code body is not available yet — this code body may \
                 read",
            )),
        }
    }

    /// A `select`: the rows, as the REST wire shape.
    async fn select(&self, plan: &Plan) -> Result<Json> {
        let read = plan::read(&self.catalog, plan, &self.limits, ROLE_ADMIN)?;
        let context = self.caller();
        let values =
            rows::list_row_values(&self.catalog, &read.table, &read.query, Some(&context)).await?;
        // The cap is enforced on what came back rather than by trimming it: a
        // body handed 1000 of 4000 rows would go on to compute a wrong answer out
        // of a right-looking one, and never know.
        if values.len() as u64 > self.limits.max_rows {
            return Err(Error::invalid(format!(
                "reading `{}` returned more than the {} rows a code body may hold at once; \
                 add a `.limit()` or narrow the `.where()`",
                read.table.name, self.limits.max_rows
            )));
        }
        Ok(Json::Array(values.iter().map(|v| read.row(v)).collect()))
    }

    /// An `aggregate`: one object, keyed by the aliases the plan asked for.
    async fn aggregate(&self, plan: &Plan) -> Result<Json> {
        let agg = plan::aggregate(&self.catalog, plan, ROLE_ADMIN)?;
        let context = self.caller();
        let values = rows::aggregate_values(
            &self.catalog,
            &agg.table,
            agg.projections,
            agg.filter,
            Some(&context),
        )
        .await?;
        let mut out = serde_json::Map::with_capacity(plan.aggregate.len());
        for spec in &plan.aggregate {
            out.insert(
                spec.alias.clone(),
                values.get(&spec.alias).map_or(Json::Null, value_to_json),
            );
        }
        Ok(Json::Object(out))
    }

    /// The caller every statement of this run carries: **admin, in the event's
    /// user's name**, with the trigger chain that led here.
    fn caller(&self) -> CallerContext {
        CallerContext::new(ROLE_ADMIN, self.user.clone()).chained(self.chain.clone())
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

#[async_trait]
impl CodeHost for TableHost {
    async fn call(&self, request: Json) -> Result<Json> {
        self.spend_call()?;
        let plan: Plan = serde_json::from_value(request).map_err(|e| {
            Error::invalid(format!(
                "this database request is not one the server understands: {e}"
            ))
        })?;
        self.run(&plan).await
    }
}

#[cfg(test)]
mod tests;
