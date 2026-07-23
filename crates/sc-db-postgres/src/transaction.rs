//! [`PgTransaction`] — a Postgres transaction on a pooled connection.
//!
//! Rather than borrow a `tokio_postgres::Transaction` from the pooled client
//! (which would make this a self-referential struct), the transaction owns its
//! connection and drives `BEGIN`/`COMMIT`/`ROLLBACK` directly. Because
//! [`commit`](sc_db::Transaction::commit) and
//! [`rollback`](sc_db::Transaction::rollback) consume the boxed handle, a
//! transaction is finalised exactly once; the owned connection is taken out at
//! that point (leaving `None`), so a `Drop` after an explicit finish is a no-op.
//! A handle dropped *without* finishing rolls back so the connection returns to
//! the pool clean rather than mid-transaction.

use async_trait::async_trait;
use deadpool_postgres::Object;
use sc_db::{RowStream, SchemaChange, Transaction};
use sc_error::{Error, Result};
use sc_query::Statement;

use crate::dialect::PgDialect;

/// A transaction bound to one pooled Postgres connection.
pub(crate) struct PgTransaction {
    /// The connection, held until the transaction is committed, rolled back, or
    /// dropped. `None` after any of those.
    client: Option<Object>,
    dialect: PgDialect,
}

impl PgTransaction {
    /// Open a transaction on `client` by issuing `BEGIN`.
    pub(crate) async fn begin(client: Object, dialect: PgDialect) -> Result<Self> {
        client
            .batch_execute("BEGIN")
            .await
            .map_err(|e| Error::database(format!("begin transaction: {e}")))?;
        Ok(PgTransaction {
            client: Some(client),
            dialect,
        })
    }

    /// Borrow the live connection, or error if the transaction is already
    /// finished.
    fn client(&self) -> Result<&Object> {
        self.client
            .as_ref()
            .ok_or_else(|| Error::database("transaction already finished"))
    }

    /// Take the connection to finalise the transaction, or error if it is
    /// already finished.
    fn take_client(&mut self) -> Result<Object> {
        self.client
            .take()
            .ok_or_else(|| Error::database("transaction already finished"))
    }
}

#[async_trait]
impl Transaction for PgTransaction {
    async fn query(&mut self, stmt: &Statement) -> Result<RowStream> {
        let client = self.client()?;
        crate::exec::run_query(client, &self.dialect, stmt).await
    }

    async fn set_local(&mut self, name: &str, value: &str) -> Result<()> {
        let client = self.client()?;
        // `set_config(name, value, is_local=true)` is the function form of
        // `SET LOCAL`, and unlike the `SET` statement it takes the setting
        // *value* as a bind — so it is parameterised, never interpolated. The
        // setting *name* is a fixed constant from our own code (`sc.role`,
        // `sc.user`), not user input.
        client
            .query("SELECT set_config($1, $2, true)", &[&name, &value])
            .await
            .map_err(|e| {
                Error::database(format!(
                    "set local `{name}`: {}",
                    sc_error::format_chain(&e)
                ))
            })?;
        Ok(())
    }

    async fn apply_schema(&mut self, change: &SchemaChange) -> Result<()> {
        let client = self.client()?;
        crate::exec::run_ddl(client, &self.dialect, change).await
    }

    async fn batch(&mut self, sql: &str) -> Result<()> {
        let client = self.client()?;
        client.batch_execute(sql).await.map_err(|e| {
            Error::database(format!(
                "batch failed: {}\n  sql: {sql}",
                sc_error::format_chain(&e)
            ))
        })?;
        Ok(())
    }

    async fn commit(self: Box<Self>) -> Result<()> {
        let mut this = self;
        let client = this.take_client()?;
        client
            .batch_execute("COMMIT")
            .await
            .map_err(|e| Error::database(format!("commit transaction: {e}")))?;
        Ok(())
    }

    async fn rollback(self: Box<Self>) -> Result<()> {
        let mut this = self;
        let client = this.take_client()?;
        client
            .batch_execute("ROLLBACK")
            .await
            .map_err(|e| Error::database(format!("rollback transaction: {e}")))?;
        Ok(())
    }
}

impl Drop for PgTransaction {
    fn drop(&mut self) {
        // `commit`/`rollback` already took the connection; nothing to do.
        let Some(client) = self.client.take() else {
            return;
        };
        // Abandoned without an explicit finish: roll back so the pooled
        // connection is not returned mid-transaction. `Drop` is sync, so run the
        // rollback on the runtime if we are on one, else on a short-lived thread.
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    let _ = client.batch_execute("ROLLBACK").await;
                });
            }
            Err(_) => {
                std::thread::spawn(move || {
                    if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        rt.block_on(async move {
                            let _ = client.batch_execute("ROLLBACK").await;
                        });
                    }
                });
            }
        }
    }
}
