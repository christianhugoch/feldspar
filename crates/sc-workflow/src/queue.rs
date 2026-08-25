//! The queue: which runs want the engine, and who is working on them (§10.3,
//! decision 5).
//!
//! The design says the engine is driven by "a durable queue on the bus", and
//! `sc-bus` does not exist. Building one to hold a queue would be building the
//! wrong thing first: **a durable queue's authority has to be the database
//! anyway**, or a crashed node loses the runs it was holding. So the runnable set
//! is a query —
//!
//! ```text
//! kind = 'workflow'
//!   AND state IN ('running', 'waiting')
//!   AND wake_at IS NOT NULL AND wake_at <= now
//!   AND (lease_until IS NULL OR lease_until < now)
//! ```
//!
//! — and claiming a run is a conditional `UPDATE` on that same predicate, which
//! is correct for two nodes as well as for one: both may read the same row, but
//! the first `UPDATE` puts a lease in the *future* and the second's `WHERE` no
//! longer matches, so it claims nothing. Nothing here needs a transaction, a lock
//! or a queue table.
//!
//! ## The seam, and what it is for
//!
//! [`WorkQueue`] is the shape `sc-bus` will implement later: "claim what is due"
//! and "**wake me when something might be**". Only the second is interesting —
//! today it is a sleep, tomorrow it is a `NOTIFY` or a Redis subscription, and
//! **nothing above the seam knows which it is talking to**. The polling
//! implementation is what ships, and it is twenty lines.
//!
//! ## Recovery is not a special case
//!
//! A lease that has run out is a crashed node's run, and the next poll picks it
//! up because an expired lease reads exactly like no lease at all. There is no
//! crash detector, no heartbeat table and no recovery pass — which is why
//! recovery is tested by writing a row with a stale lease rather than by killing
//! a process.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sc_agent::run_store::{
    COL_CLAIMED_BY, COL_ID, COL_KIND, COL_LEASE_UNTIL, COL_STATE, COL_WAKE_AT, RUNS_TABLE,
    run_from_row,
};
use sc_agent::{Run, RunKind, RunState, load_run};
use sc_catalog::Catalog;
use sc_db::Row;
use sc_error::Result;
use sc_query::{
    Assignment, BinOp, Expr, InSet, OrderBy, Select, Source, Statement, UnOp, Update, Value,
};

/// How many runs one poll claims when the caller does not say.
pub const DEFAULT_BATCH: usize = 8;

/// Where the engine gets runnable runs from, and how it says it is working on
/// one.
///
/// Object-safe, because which implementation is behind it is a property of the
/// deployment rather than of the engine: the polling one below is what a
/// single-node server runs, and a bus-backed one is the same three methods with a
/// cheaper [`wake`](WorkQueue::wake).
#[async_trait]
pub trait WorkQueue: Send + Sync {
    /// Claim up to `limit` runs that want the engine at `now`, leasing each until
    /// `until`, and answer the ones actually claimed.
    ///
    /// A run that another node claimed between the read and the write is simply
    /// not in the answer. That is not an error and must not be reported as one:
    /// it is two nodes doing their job.
    async fn claim(
        &self,
        now: DateTime<Utc>,
        until: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<Run>>;

    /// Extend the lease on a run this node is working on, so a step that takes
    /// longer than one lease is not picked up by somebody else half way through.
    async fn renew(&self, run: &Run, until: DateTime<Utc>) -> Result<()>;

    /// Wait until it is worth asking again.
    ///
    /// The whole reason the seam exists: a `NOTIFY` or a subscription replaces
    /// this and nothing else changes. Returning early is always allowed — the
    /// caller polls, it does not trust this to be precise.
    async fn wake(&self);

    /// This node's identity, for the operator reading a run that is not moving.
    /// Never authority — the lease is.
    fn node(&self) -> &str;
}

/// The queue that ships: the runs table, polled.
pub struct DatabaseQueue {
    catalog: Arc<Catalog>,
    node: String,
    poll: std::time::Duration,
}

impl DatabaseQueue {
    /// A queue over `catalog`, claiming as `node`, polling every `poll`.
    pub fn new(catalog: Arc<Catalog>, node: impl Into<String>, poll: std::time::Duration) -> Self {
        DatabaseQueue {
            catalog,
            node: node.into(),
            poll,
        }
    }

    /// The runs that want the engine at `now` and that nothing is working on.
    ///
    /// Ordered by `wake_at`, so the run that has been waiting longest goes first
    /// and a busy engine cannot starve one run behind a stream of newer ones.
    pub async fn due(&self, now: DateTime<Utc>, limit: usize) -> Result<Vec<Run>> {
        let mut select = Select::from(Source::table(RUNS_TABLE)).filter(runnable(now));
        select.order = vec![OrderBy::asc(Expr::col(COL_WAKE_AT))];
        select.limit = Some(limit as u64);
        let rows: Vec<Row> = self
            .catalog
            .primary()
            .query(&Statement::from(select))
            .await?
            .try_collect()
            .await?;
        rows.iter().map(run_from_row).collect()
    }
}

#[async_trait]
impl WorkQueue for DatabaseQueue {
    async fn claim(
        &self,
        now: DateTime<Utc>,
        until: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<Run>> {
        let mut claimed = Vec::new();
        for run in self.due(now, limit).await? {
            // The compare-and-set: the same predicate that made the run look
            // claimable, re-checked by the database at the moment of the write.
            // A second node that got there first has put a lease in the future,
            // so this matches nothing and claims nothing.
            let update = Update::new(
                RUNS_TABLE,
                vec![
                    Assignment::new(COL_LEASE_UNTIL, Expr::lit(until)),
                    Assignment::new(COL_CLAIMED_BY, Expr::lit(self.node.clone())),
                ],
            )
            .filter(Expr::col(COL_ID).eq(Expr::lit(run.id.0)).and(unleased(now)));
            self.catalog
                .primary()
                .query(&Statement::from(update))
                .await?
                .try_collect()
                .await?;
            // Who actually holds it. Read back rather than assumed, because the
            // update above is allowed to have matched nothing — and because a
            // backend that cannot report affected rows must still give a
            // truthful answer here.
            //
            // The test is "this node holds it, and the lease is live", **not**
            // "the lease is the instant we wrote": a timestamp goes to the
            // database at nanosecond precision and comes back at the column's,
            // and a claim that depended on those agreeing would claim nothing on
            // Postgres and everything on a backend that happened to round the
            // other way.
            let Some(fresh) = load_run(&self.catalog, run.id).await? else {
                continue;
            };
            if fresh.claimed_by.as_deref() == Some(self.node.as_str())
                && fresh.lease_until.is_some_and(|until| until > now)
            {
                claimed.push(fresh);
            }
        }
        Ok(claimed)
    }

    async fn renew(&self, run: &Run, until: DateTime<Utc>) -> Result<()> {
        let update = Update::new(
            RUNS_TABLE,
            vec![Assignment::new(COL_LEASE_UNTIL, Expr::lit(until))],
        )
        .filter(
            Expr::col(COL_ID)
                .eq(Expr::lit(run.id.0))
                .and(Expr::col(COL_CLAIMED_BY).eq(Expr::lit(self.node.clone()))),
        );
        self.catalog
            .primary()
            .query(&Statement::from(update))
            .await?
            .try_collect()
            .await?;
        Ok(())
    }

    async fn wake(&self) {
        tokio::time::sleep(self.poll).await;
    }

    fn node(&self) -> &str {
        &self.node
    }
}

/// The runnable set (see the module docs): a live workflow run whose time has
/// come and that nothing is working on.
fn runnable(now: DateTime<Utc>) -> Expr {
    Expr::col(COL_KIND)
        .eq(Expr::lit(RunKind::Workflow.as_str()))
        .and(Expr::In {
            e: Box::new(Expr::col(COL_STATE)),
            set: InSet::List(vec![
                Expr::lit(RunState::Running.as_str()),
                Expr::lit(RunState::Waiting.as_str()),
            ]),
        })
        // A NULL `wake_at` is a run waiting on a **person**: live for a week and
        // runnable at no point in it, which is why "is it live" and "is it
        // runnable now" are two questions.
        .and(Expr::unary(UnOp::IsNotNull, Expr::col(COL_WAKE_AT)))
        .and(Expr::binary(
            BinOp::Le,
            Expr::col(COL_WAKE_AT),
            Expr::Lit(Value::Timestamp(now)),
        ))
        .and(unleased(now))
}

/// Nothing is working on this run: no lease, or one that has run out — which is
/// a crashed node's, and is the whole of the recovery path.
fn unleased(now: DateTime<Utc>) -> Expr {
    Expr::unary(UnOp::IsNull, Expr::col(COL_LEASE_UNTIL)).or(Expr::binary(
        BinOp::Lt,
        Expr::col(COL_LEASE_UNTIL),
        Expr::Lit(Value::Timestamp(now)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // What the queue does needs a database, and is pinned in
    // `tests/engine_runs.rs`. What can be asserted here is the predicate's
    // shape, which is the part a reader has to take on trust otherwise.
    #[test]
    fn the_runnable_set_excludes_a_run_waiting_on_a_person() {
        let sql = format!("{:?}", runnable(Utc::now()));
        // A null `wake_at` is excluded, and a live lease is.
        assert!(sql.contains("IsNotNull"), "{sql}");
        assert!(sql.contains("IsNull"), "{sql}");
        assert!(sql.contains("wake_at"), "{sql}");
        assert!(sql.contains("lease_until"), "{sql}");
    }
}
