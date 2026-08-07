//! The [`DatabaseDriver`] trait — one connected database — and the
//! [`Transaction`] handle it hands out.
//!
//! A driver is instantiated once per connected database and **must** be written
//! in Rust (unlike most extension points, this one is not exposed to guest code
//! — technical design §5, §15). The catalog holds drivers behind
//! `Arc<dyn DatabaseDriver>`, so the trait is object-safe.

use async_trait::async_trait;
use sc_error::{Error, Result};
use sc_query::{SqlDialect, Statement};

use crate::capabilities::DbCapabilities;
use crate::row::RowStream;
use crate::schema::{DescribedColumn, PhysicalTable, SchemaChange};

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

    /// Prepare `sql` — with its bind parameters typed by `param_types`, named as
    /// this backend names its types — and report the **result columns** the
    /// backend says it will produce, without running it.
    ///
    /// This is how a custom SQL query (§13.4) gets its result type: the database
    /// is the thing that knows what `SELECT sum(price), author FROM …` returns,
    /// so it is asked, rather than an administrator being made to declare a
    /// shape that goes stale the first time anyone edits the SQL. It doubles as
    /// validation — a statement that will not prepare comes back as the
    /// backend's own error, so a broken query is refused while its author is
    /// still looking at it.
    ///
    /// Preparing must have **no effect**: it is a plan, not an execution.
    ///
    /// The default errors rather than returning no columns: a backend that
    /// cannot describe a statement cannot type one either, and an empty answer
    /// would read as "this query returns nothing".
    async fn describe(&self, sql: &str, param_types: &[String]) -> Result<Vec<DescribedColumn>> {
        let _ = (sql, param_types);
        Err(Error::database(
            "this database backend cannot describe a statement, so a custom SQL \
             query cannot be typed against it",
        ))
    }

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

    /// Set a **transaction-local** configuration parameter (`SET LOCAL`), so it
    /// is scoped to this transaction and reverts on commit/rollback — it never
    /// leaks to the next user of a pooled connection.
    ///
    /// This is how the authorization layer (§7.3) hands the current caller's
    /// role and identity to row-level-security policies: the policies read the
    /// parameter with `current_setting(name, true)`, and a transaction that
    /// forgets to set it sees the parameter as `NULL` — which the policies are
    /// generated to treat as "no access", so a forgotten context **fails
    /// closed**. `value` is bound, never interpolated; `name` is a fixed
    /// constant chosen by the caller, not user input.
    ///
    /// The default implementation errors: a backend without transaction-local
    /// settings cannot support this authorization mode, and saying so beats a
    /// silent no-op that would make policies see the wrong caller.
    async fn set_local(&mut self, name: &str, value: &str) -> Result<()> {
        let _ = (name, value);
        Err(Error::database(
            "this database backend does not support transaction-local settings (SET LOCAL)",
        ))
    }

    /// Apply a schema change within the transaction.
    async fn apply_schema(&mut self, change: &SchemaChange) -> Result<()>;

    /// Run a raw, multi-statement SQL script with no bind parameters, in order,
    /// within the transaction.
    ///
    /// The escape hatch for backend-specific DDL that the structured
    /// [`SchemaChange`] does not model — row-level-security policies (§7.3),
    /// whose `CREATE POLICY` carries an arbitrary boolean expression. The SQL is
    /// generated by trusted code (formula translation, with literals rendered
    /// through the dialect's own quoting), never assembled from raw user input.
    /// The default errors, so a backend that has not opted in cannot silently
    /// skip the DDL.
    async fn batch(&mut self, sql: &str) -> Result<()> {
        let _ = sql;
        Err(Error::database(
            "this database backend does not support raw SQL batches",
        ))
    }

    /// Commit the transaction, making its effects durable.
    async fn commit(self: Box<Self>) -> Result<()>;

    /// Roll the transaction back, discarding its effects.
    async fn rollback(self: Box<Self>) -> Result<()>;
}
