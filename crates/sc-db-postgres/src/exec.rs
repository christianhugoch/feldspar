//! Statement and DDL execution against a `tokio_postgres` connection.
//!
//! Factored out of the driver so the exact same code path runs a statement
//! whether it is issued on a pooled connection ([`PgDriver`](crate::PgDriver))
//! or inside a transaction ([`PgTransaction`](crate::transaction::PgTransaction)).
//! Both hold a `deadpool` connection that derefs to a `tokio_postgres::Client`.

use std::sync::Arc;

use sc_db::{Row, RowStream, SchemaChange};
use sc_error::{Error, Result};
use sc_query::{SqlDialect, Statement};
use tokio_postgres::Client;
use tokio_postgres::types::ToSql;

use crate::dialect::PgDialect;
use crate::value::{PgParam, decode};

/// Render `stmt` to Postgres SQL, run it on `client`, and materialise the rows.
///
/// Any statement kind is accepted; a non-`RETURNING` mutation simply yields no
/// rows. Rows are buffered into the stream for the MVP — the `RowStream`
/// signature keeps a later switch to true server-side streaming invisible.
pub(crate) async fn run_query(
    client: &Client,
    dialect: &PgDialect,
    stmt: &Statement,
) -> Result<RowStream> {
    let (sql, binds) = dialect.render(stmt)?;

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

/// Render `change` to a single DDL statement and run it over the simple-query
/// protocol (DDL takes no binds).
pub(crate) async fn run_ddl(
    client: &Client,
    dialect: &PgDialect,
    change: &SchemaChange,
) -> Result<()> {
    let sql = crate::ddl::render(dialect, change)?;
    client
        .batch_execute(&sql)
        .await
        .map_err(|e| Error::database(format!("apply_schema failed: {e}")))?;
    Ok(())
}
