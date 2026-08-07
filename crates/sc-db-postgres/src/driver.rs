//! [`PgDriver`] — a pooled connection to one Postgres database, and its
//! [`DatabaseDriver`] implementation.
//!
//! The driver owns a `deadpool-postgres` pool and renders statements with
//! [`PgDialect`]. Query and DDL execution live in [`crate::exec`] (shared with
//! transactions); introspection in [`crate::introspect`]; DDL rendering in
//! [`crate::ddl`]; transactions in [`crate::transaction`]. This module wires
//! them together and exposes them both as inherent methods (convenient, and what
//! the crate's tests use) and through the `DatabaseDriver` trait (for the
//! catalog's `Arc<dyn DatabaseDriver>`).

use async_trait::async_trait;
use deadpool_postgres::{Manager, ManagerConfig, Object, Pool, RecyclingMethod};
use sc_db::{
    DatabaseDriver, DbCapabilities, DescribedColumn, PhysicalTable, RowStream, SchemaChange,
    Transaction,
};
use sc_error::{Error, Result};
use sc_query::{SqlDialect, Statement};
use tokio_postgres::{Config, NoTls};

use crate::dialect::PgDialect;

/// A connection pool to one Postgres database, plus its SQL dialect.
///
/// Cheap to clone conceptually (the underlying `Pool` is an `Arc`), though it is
/// normally held once behind an `Arc<dyn DatabaseDriver>` by the catalog.
pub struct PgDriver {
    pool: Pool,
    dialect: PgDialect,
}

impl PgDriver {
    /// Wrap an already-built connection pool.
    pub fn from_pool(pool: Pool) -> Self {
        PgDriver {
            pool,
            dialect: PgDialect::new(),
        }
    }

    /// Build a pooled driver from a libpq/URL connection string, e.g.
    /// `postgres://user:pass@host:5432/dbname`.
    pub async fn connect(url: &str) -> Result<Self> {
        let config = url
            .parse::<Config>()
            .map_err(|e| Error::config(format!("invalid postgres connection string: {e}")))?;
        Self::from_config(&config)
    }

    /// Build a pooled driver from a parsed tokio-postgres [`Config`].
    pub fn from_config(config: &Config) -> Result<Self> {
        let mgr_config = ManagerConfig {
            recycling_method: RecyclingMethod::Fast,
        };
        let manager = Manager::from_config(config.clone(), NoTls, mgr_config);
        let pool = Pool::builder(manager)
            .max_size(8)
            .build()
            .map_err(|e| Error::database(format!("build pool: {e}")))?;
        Ok(Self::from_pool(pool))
    }

    /// The underlying connection pool.
    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    /// The Postgres SQL dialect used to render statements for this driver.
    pub fn dialect(&self) -> &PgDialect {
        &self.dialect
    }

    /// What this Postgres backend supports.
    pub fn capabilities(&self) -> DbCapabilities {
        DbCapabilities {
            row_level_security: true,
            composite_pk: true,
            listen_notify: true,
            returning: true,
        }
    }

    /// Read the live schema of every user table reachable through the
    /// connection (there is no discovery step — see [`crate::introspect`]).
    pub async fn introspect(&self) -> Result<Vec<PhysicalTable>> {
        let client = self.client().await?;
        crate::introspect::introspect(&client).await
    }

    /// Render `stmt` to Postgres SQL, run it on a pooled connection, and return
    /// the rows.
    pub async fn query(&self, stmt: &Statement) -> Result<RowStream> {
        let client = self.client().await?;
        crate::exec::run_query(&client, &self.dialect, stmt).await
    }

    /// Apply a single schema change (create/drop table, add/drop column).
    /// Creating a table emits exactly the columns given — no `id` column is
    /// invented (see [`crate::ddl`]).
    pub async fn apply_schema(&self, change: &SchemaChange) -> Result<()> {
        let client = self.client().await?;
        crate::exec::run_ddl(&client, &self.dialect, change).await
    }

    /// Prepare `sql` (parameters typed by `param_types`) and report the result
    /// columns Postgres says it will produce, without running it — how a custom
    /// SQL query is typed, and how one that will not prepare is refused at the
    /// keyboard (see [`crate::exec::describe`]).
    pub async fn describe(
        &self,
        sql: &str,
        param_types: &[String],
    ) -> Result<Vec<DescribedColumn>> {
        let client = self.client().await?;
        crate::exec::describe(&client, sql, param_types).await
    }

    /// Begin a transaction on a dedicated pooled connection. Metadata mutations
    /// run inside one; the returned handle is committed or rolled back exactly
    /// once (dropping it rolls back).
    pub async fn begin(&self) -> Result<Box<dyn Transaction>> {
        let client = self.client().await?;
        let tx = crate::transaction::PgTransaction::begin(client, self.dialect).await?;
        Ok(Box::new(tx))
    }

    /// Check out a pooled connection.
    async fn client(&self) -> Result<Object> {
        self.pool
            .get()
            .await
            .map_err(|e| Error::database(format!("checkout connection: {e}")))
    }
}

// The trait impl delegates to the inherent methods above. Inherent methods
// shadow trait methods in resolution, so the `PgDriver::method(self, …)` calls
// below refer to the inherent implementations (no recursion).
#[async_trait]
impl DatabaseDriver for PgDriver {
    async fn introspect(&self) -> Result<Vec<PhysicalTable>> {
        PgDriver::introspect(self).await
    }

    async fn query(&self, stmt: &Statement) -> Result<RowStream> {
        PgDriver::query(self, stmt).await
    }

    async fn apply_schema(&self, change: &SchemaChange) -> Result<()> {
        PgDriver::apply_schema(self, change).await
    }

    async fn describe(&self, sql: &str, param_types: &[String]) -> Result<Vec<DescribedColumn>> {
        PgDriver::describe(self, sql, param_types).await
    }

    async fn begin(&self) -> Result<Box<dyn Transaction>> {
        PgDriver::begin(self).await
    }

    fn capabilities(&self) -> DbCapabilities {
        PgDriver::capabilities(self)
    }

    fn dialect(&self) -> &dyn SqlDialect {
        &self.dialect
    }
}
