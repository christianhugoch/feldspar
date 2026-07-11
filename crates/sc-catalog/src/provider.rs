//! [`TableProvider`]: presenting a data source as a queryable table.
//!
//! A provider interprets the universal query language ([`Select`], plus
//! [`Statement`] writes) and returns rows (technical design §8.3). A
//! database-backed table is served by the trivial [`DriverTableProvider`], which
//! forwards straight to the [`DatabaseDriver`]; non-trivial providers
//! (RSS/IMAP/search/…) are post-MVP. Materialisation (`None | Snapshot | Synced`)
//! is likewise deferred, so the MVP trait carries only the read/write surface.

use std::sync::Arc;

use async_trait::async_trait;
use sc_db::{DatabaseDriver, RowStream};
use sc_error::Result;
use sc_query::{Select, Statement};

use crate::field::DataField;

/// Presents a data source as a table: report its fields, run a `SELECT`, and (if
/// writable) apply an `INSERT`/`UPDATE`/`DELETE`.
#[async_trait]
pub trait TableProvider: Send + Sync {
    /// The fields this provider presents.
    fn fields(&self) -> Vec<DataField>;

    /// Run a `SELECT` and stream matching rows.
    async fn query(&self, select: &Select) -> Result<RowStream>;

    /// Apply a write statement (`INSERT`/`UPDATE`/`DELETE`), streaming back any
    /// `RETURNING` rows (empty when the statement returns nothing).
    async fn write(&self, change: &Statement) -> Result<RowStream>;
}

/// The trivial provider: a table backed directly by a [`DatabaseDriver`]. Every
/// database table is served by one of these.
pub struct DriverTableProvider {
    driver: Arc<dyn DatabaseDriver>,
    fields: Vec<DataField>,
}

impl DriverTableProvider {
    /// Wrap a driver and the fields of the table it serves.
    pub fn new(driver: Arc<dyn DatabaseDriver>, fields: Vec<DataField>) -> DriverTableProvider {
        DriverTableProvider { driver, fields }
    }
}

#[async_trait]
impl TableProvider for DriverTableProvider {
    fn fields(&self) -> Vec<DataField> {
        self.fields.clone()
    }

    async fn query(&self, select: &Select) -> Result<RowStream> {
        self.driver
            .query(&Statement::Select(Box::new(select.clone())))
            .await
    }

    async fn write(&self, change: &Statement) -> Result<RowStream> {
        self.driver.query(change).await
    }
}
