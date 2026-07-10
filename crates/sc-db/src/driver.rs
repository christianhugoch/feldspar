//! The [`DatabaseDriver`] trait — one connected database — and the
//! [`Transaction`] handle it hands out.
//!
//! A driver is instantiated once per connected database and **must** be written
//! in Rust (unlike most extension points, this one is not exposed to guest code
//! — technical design §5, §15). The catalog holds drivers behind
//! `Arc<dyn DatabaseDriver>`, so the trait is object-safe.

use async_trait::async_trait;
use sc_error::Result;
use sc_query::{SqlDialect, Statement};

use crate::capabilities::DbCapabilities;
use crate::row::RowStream;
use crate::schema::{PhysicalTable, SchemaChange};

/// A single connected database: introspect its schema, run queries, change its
/// schema, and open transactions (technical design §5).
///
/// The **primary** database is just the one driver that additionally hosts the
/// `_sc_*` metadata and `users` tables; the trait itself draws no distinction.
#[async_trait]
pub trait DatabaseDriver: Send + Sync {
    /// Read the live schema (via `information_schema` or the backend
    /// equivalent). There is no separate discovery step: every table a
    /// connection can see is returned here and is immediately usable.
    async fn introspect(&self) -> Result<Vec<PhysicalTable>>;

    /// Run a statement and stream back its rows. Literals in `stmt` are already
    /// parameterised by [`sc_query`] rendering, so nothing is interpolated here.
    async fn query(&self, stmt: &Statement) -> Result<RowStream>;

    /// Apply a single schema change (create/drop table, add/drop column).
    /// Creating a table adds no implicit primary-key column.
    async fn apply_schema(&self, change: &SchemaChange) -> Result<()>;

    /// Begin a transaction. Each metadata mutation (and, later, each workflow
    /// step) runs inside one; the returned handle is committed or rolled back
    /// exactly once.
    async fn begin(&self) -> Result<Box<dyn Transaction>>;

    /// What this backend supports. The authorization and message-bus layers
    /// branch on these flags rather than assuming a Postgres feature set.
    fn capabilities(&self) -> DbCapabilities;

    /// The SQL dialect used to render a [`Statement`] for this backend. A
    /// Postgres-dialect migration is translated to the driver's own dialect
    /// through this.
    fn dialect(&self) -> &dyn SqlDialect;
}

/// An in-progress transaction on a [`DatabaseDriver`].
///
/// Queries and schema changes issued through the handle are scoped to the
/// transaction; it is finished by [`commit`](Transaction::commit) or
/// [`rollback`](Transaction::rollback), both of which consume the handle so it
/// can be finalised only once. Dropping the handle without either rolls back.
#[async_trait]
pub trait Transaction: Send {
    /// Run a statement within the transaction.
    async fn query(&mut self, stmt: &Statement) -> Result<RowStream>;

    /// Apply a schema change within the transaction.
    async fn apply_schema(&mut self, change: &SchemaChange) -> Result<()>;

    /// Commit the transaction, making its effects durable.
    async fn commit(self: Box<Self>) -> Result<()>;

    /// Roll the transaction back, discarding its effects.
    async fn rollback(self: Box<Self>) -> Result<()>;
}
