//! [`TableProvider`]: presenting a data source as a queryable table.
//!
//! A provider interprets the universal query language ([`Select`], plus
//! [`Statement`] writes) and returns rows (technical design §8.3). There are two
//! implementations:
//!
//! - [`DriverTableProvider`] — the trivial one, which forwards straight to the
//!   [`DatabaseDriver`]. Every database table is served by one.
//! - [`ProvidedTableProvider`] — a table whose rows come from a **module's**
//!   table provider: `@saltcorn/rss`'s `RSS feed`, `@saltcorn/proxmox`'s cluster
//!   listings. The module answers a list of JSON objects and
//!   [`inmem`](crate::inmem) applies the `Select` to them.
//!
//! Materialisation (`None | Snapshot | Synced`) remains deferred, so the trait
//! carries only the read/write surface.
//!
//! ## The seam
//!
//! `sc-catalog` is layer 4 and a module host is layer 6, so the dependency is
//! inverted exactly as `sc-expr`'s `ModuleFnHost` is: [`TableProviderHost`] is
//! declared here, implemented in `sc-module`, and installed on the
//! [`Catalog`](crate::Catalog) by `sc-server` at boot and after every module
//! change. A process with no modules has none, and a provided table there is a
//! table with no fields and a reason — never a panic and never a silently empty
//! one.

use std::sync::Arc;

use async_trait::async_trait;
use sc_db::{DatabaseDriver, Row, RowStream};
use sc_error::{Error, Result};
use sc_query::{Select, Statement};
use sc_types::FormField;
use serde_json::Value as Json;

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

// --- provided tables ----------------------------------------------------------

/// One table provider a module supplies, as the "new table" screen offers it.
#[derive(Debug, Clone, PartialEq)]
pub struct TableProviderKind {
    /// The package that supplies it — `@saltcorn/rss`.
    pub module: String,
    /// Its own name within that package — `RSS feed`.
    pub provider: String,
    /// The settings it asks for, translated from the provider's own v1
    /// `configuration_workflow`. The same [`FormField`] vocabulary a file
    /// store's backend and an LLM provider use, so the admin UI renders it with
    /// no code that knows what a table provider is.
    pub config_spec: Vec<FormField>,
}

/// What supplies table providers: the seam `sc-module` implements and
/// `sc-server` installs on the catalog.
///
/// Three questions, and the split is the same one `ModuleFnHost` makes:
/// enumerating is synchronous because a form renders it in one expression, and
/// the two that reach a module are async because they reach a module.
#[async_trait]
pub trait TableProviderHost: Send + Sync {
    /// Every provider every loaded module supplies.
    fn providers(&self) -> Vec<TableProviderKind>;

    /// The fields `provider` presents for this configuration.
    ///
    /// Asked on every catalog reload rather than stored, which is v1's
    /// arrangement and the right one: the columns are the *module's* answer, so
    /// an upgraded package that presents a new column presents it, and nothing
    /// Saltcorn wrote down can disagree with the code that serves the rows.
    async fn fields(&self, module: &str, provider: &str, config: &Json) -> Result<Vec<DataField>>;

    /// Its rows, as JSON objects, for one v1 `where`/`options` pair.
    ///
    /// The pair is a **hint**: a provider may honour it (`@saltcorn/postgres-
    /// tables` turns it into SQL) or ignore it entirely (`@saltcorn/rss` answers
    /// the whole feed), and the caller applies the query to the answer either
    /// way.
    ///
    /// `table` is the Saltcorn table being read. v1 hands its provider the table
    /// row as `get_table`'s second argument and a provider reads its name off
    /// it, so the name travels; nothing else of a `Table` crosses the seam,
    /// because nothing else of it means anything on the far side.
    async fn rows(
        &self,
        module: &str,
        provider: &str,
        table: &str,
        config: &Json,
        filter: &Json,
        options: &Json,
    ) -> Result<Vec<Json>>;
}

/// A table served by a module's table provider.
///
/// Read-only in this milestone. v1's `get_table` may also answer `insertRow`,
/// `updateRow` and `deleteRows` — `@saltcorn/postgres-tables` does, behind a
/// config flag — and [`write`](ProvidedTableProvider::write) says so rather than
/// failing with something about a driver.
pub struct ProvidedTableProvider {
    host: Arc<dyn TableProviderHost>,
    table: String,
    module: String,
    provider: String,
    config: Json,
    fields: Vec<DataField>,
}

impl ProvidedTableProvider {
    /// A provider over `host` for one configured table.
    pub fn new(
        host: Arc<dyn TableProviderHost>,
        table: impl Into<String>,
        module: impl Into<String>,
        provider: impl Into<String>,
        config: Json,
        fields: Vec<DataField>,
    ) -> ProvidedTableProvider {
        ProvidedTableProvider {
            host,
            table: table.into(),
            module: module.into(),
            provider: provider.into(),
            config,
            fields,
        }
    }
}

#[async_trait]
impl TableProvider for ProvidedTableProvider {
    fn fields(&self) -> Vec<DataField> {
        self.fields.clone()
    }

    async fn query(&self, select: &Select) -> Result<RowStream> {
        // The hint first, so a provider that can do the work in its own backend
        // is given the chance; then the query itself, over whatever came back.
        let (filter, options) = crate::inmem::pushdown(select);
        let json = self
            .host
            .rows(
                &self.module,
                &self.provider,
                &self.table,
                &self.config,
                &filter,
                &options,
            )
            .await?;
        let rows: Vec<crate::inmem::ValueRow> = json
            .iter()
            .map(|row| crate::inmem::value_row(&self.fields, row))
            .collect();
        let out: Vec<Row> = crate::inmem::run_select_over(select, &self.table, rows)?;
        Ok(RowStream::from_rows(out))
    }

    async fn write(&self, change: &Statement) -> Result<RowStream> {
        let _ = change;
        Err(Error::invalid(format!(
            "`{}` is served by the table provider `{}` of `{}`, and this version of Saltcorn \
             reads a provided table but does not write one: a writable provider (v1\'s \
             `insertRow`/`updateRow`/`deleteRows`) is not implemented yet",
            self.table, self.provider, self.module
        )))
    }
}
