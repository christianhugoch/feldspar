//! Library half of the `saltcorn` binary (layer 10).
//!
//! The binary ([`main`](../main/index.html)) stays thin; the reusable pieces —
//! parsing the database connection ([`DbConfig`]) and standing up a connected
//! [`Catalog`] ([`connect_catalog`]) — live here so integration tests can drive
//! the same boot path the CLI uses.

pub mod db;

use std::sync::Arc;

use sc_catalog::Catalog;
use sc_db::DatabaseDriver;
use sc_error::{Context, Result};

pub use db::DbConfig;

/// Connect to the primary database described by `db`, initialise the
/// [`Catalog`] from its live schema, and ensure the `users` table exists.
///
/// This is the whole "bring the data layer up" step of `saltcorn serve`. The
/// first real connection happens inside [`Catalog::init`] (introspection), so a
/// database that is unreachable or misconfigured fails here — with the redacted
/// [`DbConfig::target`] in the message rather than a silent, half-booted server.
pub async fn connect_catalog(db: &DbConfig) -> Result<Arc<Catalog>> {
    let driver = db
        .connect()
        .await
        .with_context(|| format!("connecting to database {}", db.target()))?;
    let catalog = Catalog::init(Arc::new(driver) as Arc<dyn DatabaseDriver>)
        .await
        .with_context(|| format!("reading the schema of database {}", db.target()))?;
    let catalog = Arc::new(catalog);
    sc_auth::bootstrap(&catalog)
        .await
        .context("ensuring the users table exists")?;
    Ok(catalog)
}
