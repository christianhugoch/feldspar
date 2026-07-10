//! [`PgDriver`] — a pooled connection to one Postgres database that runs a
//! rendered [`Statement`] and streams the result back as a
//! [`RowStream`](sc_db::RowStream).
//!
//! This is the first working piece of the Postgres backend: connection and
//! pooling (via `deadpool-postgres`) and the query path. Introspection, schema
//! application, and transactions — and the full `DatabaseDriver` trait impl that
//! ties them together — arrive in the following Phase 2 items.

use std::sync::Arc;

use deadpool_postgres::{Manager, ManagerConfig, Object, Pool, RecyclingMethod};
use sc_db::{DbCapabilities, PhysicalTable, Row, RowStream, SchemaChange};
use sc_error::{Error, Result};
use sc_query::{SqlDialect, Statement};
use tokio_postgres::types::ToSql;
use tokio_postgres::{Config, NoTls};

use crate::dialect::PgDialect;
use crate::value::{PgParam, decode};

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

    /// Apply a single schema change (create/drop table, add/drop column) by
    /// rendering it to Postgres DDL and executing it. Creating a table emits
    /// exactly the columns given — no `id` column is invented (see [`crate::ddl`]).
    pub async fn apply_schema(&self, change: &SchemaChange) -> Result<()> {
        let sql = crate::ddl::render(&self.dialect, change)?;
        let client = self.client().await?;
        // DDL is issued over the simple-query protocol (no binds).
        client
            .batch_execute(&sql)
            .await
            .map_err(|e| Error::database(format!("apply_schema failed: {e}")))?;
        Ok(())
    }

    /// Check out a pooled connection.
    async fn client(&self) -> Result<Object> {
        self.pool
            .get()
            .await
            .map_err(|e| Error::database(format!("checkout connection: {e}")))
    }

    /// Render `stmt` to Postgres SQL, run it on a pooled connection, and return
    /// the rows.
    ///
    /// Any statement kind is accepted; a non-`RETURNING` mutation simply yields
    /// no rows. Result rows are materialised into the stream (buffered) for the
    /// MVP — the signature stays a `RowStream` so a future switch to true
    /// server-side streaming is invisible to callers.
    pub async fn query(&self, stmt: &Statement) -> Result<RowStream> {
        let (sql, binds) = self.dialect.render(stmt)?;
        let client = self.client().await?;

        // Bind values are wrapped so the whole ordered set can be passed as
        // `&[&(dyn ToSql + Sync)]`.
        let params: Vec<PgParam> = binds.iter().map(PgParam).collect();
        let param_refs: Vec<&(dyn ToSql + Sync)> =
            params.iter().map(|p| p as &(dyn ToSql + Sync)).collect();

        let pg_rows = client
            .query(&sql, &param_refs)
            .await
            .map_err(|e| Error::database(format!("query failed: {e}")))?;

        // All rows in a result share one column list; build it once.
        let columns: Arc<Vec<String>> = Arc::new(
            pg_rows
                .first()
                .map(|r| r.columns().iter().map(|c| c.name().to_string()).collect())
                .unwrap_or_default(),
        );

        let mut rows = Vec::with_capacity(pg_rows.len());
        for pg in &pg_rows {
            let mut values = Vec::with_capacity(pg.len());
            for i in 0..pg.len() {
                values.push(decode(pg, i)?);
            }
            rows.push(Row::new(columns.clone(), values)?);
        }
        Ok(RowStream::from_rows(rows))
    }
}
