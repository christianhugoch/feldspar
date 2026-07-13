//! The [`Catalog`]: the hub that owns the connected database and an in-memory
//! cache of its tables.
//!
//! For the MVP the catalog is initialised straight from a
//! [`DatabaseDriver`]'s introspection — **no stored metadata beyond
//! `information_schema`** (technical design §5, §8, §9). As soon as a database is
//! connected every table is usable; there is no discovery or registration step.
//! The cache is rebuilt from introspection on [`init`](Catalog::init) and after
//! every schema mutation the catalog makes, so it always reflects the live
//! schema. Cross-process cache invalidation over a message bus is post-MVP
//! (single process for the MVP), so a simple `RwLock` guards the cache here.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use sc_db::{DatabaseDriver, SchemaChange};
use sc_error::{Error, Result};
use sc_files::FileStore;

use crate::field::{DataField, DbId, TableId};
use crate::provider::{DriverTableProvider, TableProvider};
use crate::table::Table;

/// The catalog: the connected primary database plus a cache of its tables
/// (technical design §8.1). Row data for users, workflow runs, and files is
/// deliberately not cached; for the MVP the cache holds tables only. Connected
/// **file stores** (§14.1) are registered here too — not their contents, just
/// the named store handles — so the server can resolve a store by name.
pub struct Catalog {
    /// The primary (and, for the MVP, only) database driver.
    primary: Arc<dyn DatabaseDriver>,
    /// The primary database's id, stamped onto every [`Table`] it hosts.
    primary_db: DbId,
    /// Cache of tables keyed by id, rebuilt from introspection.
    cache: RwLock<HashMap<TableId, Table>>,
    /// Connected file stores, keyed by their [`name`](FileStore::name). Only the
    /// store handles live here (files keep no database row, design §9); the bytes
    /// and per-file metadata stay in the store itself.
    file_stores: RwLock<HashMap<String, Arc<dyn FileStore>>>,
}

impl Catalog {
    /// Build a catalog from a primary driver, loading its tables from
    /// introspection.
    pub async fn init(primary: Arc<dyn DatabaseDriver>) -> Result<Catalog> {
        let catalog = Catalog {
            primary,
            primary_db: DbId::primary(),
            cache: RwLock::new(HashMap::new()),
            file_stores: RwLock::new(HashMap::new()),
        };
        catalog.reload().await?;
        Ok(catalog)
    }

    /// The primary database driver.
    pub fn primary(&self) -> &Arc<dyn DatabaseDriver> {
        &self.primary
    }

    /// Re-introspect the primary database and rebuild the table cache. Called
    /// after every schema change the catalog applies so the cache never drifts
    /// from the live schema.
    pub async fn reload(&self) -> Result<()> {
        let physicals = self.primary.introspect().await?;
        let mut map = HashMap::with_capacity(physicals.len());
        for physical in &physicals {
            let table = Table::from_physical(self.primary_db.clone(), physical);
            map.insert(table.id.clone(), table);
        }
        // The DB I/O is done; take the lock only to swap in the new snapshot so
        // it is never held across an await.
        let mut guard = self
            .cache
            .write()
            .map_err(|_| Error::msg("catalog cache lock poisoned"))?;
        *guard = map;
        Ok(())
    }

    /// The cached table with the given name, if present.
    pub fn get(&self, name: &str) -> Result<Option<Table>> {
        let guard = self
            .cache
            .read()
            .map_err(|_| Error::msg("catalog cache lock poisoned"))?;
        Ok(guard.get(&TableId(name.to_owned())).cloned())
    }

    /// The cached table with the given name, or a [`NotFound`](Error::NotFound)
    /// error.
    pub fn require(&self, name: &str) -> Result<Table> {
        self.get(name)?
            .ok_or_else(|| Error::not_found(format!("table `{name}` is not in the catalog")))
    }

    /// All cached tables, sorted by name. Includes system (`_sc_*`) tables; use
    /// [`Table::is_system`] to filter.
    pub fn tables(&self) -> Result<Vec<Table>> {
        let guard = self
            .cache
            .read()
            .map_err(|_| Error::msg("catalog cache lock poisoned"))?;
        let mut tables: Vec<Table> = guard.values().cloned().collect();
        tables.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(tables)
    }

    /// Create a table with the given fields, then reload the cache and return the
    /// resulting [`Table`]. Primary-key columns are those fields marked
    /// [`primary_key`](DataField::primary_key); no `id` column is invented.
    pub async fn create_table(
        &self,
        name: impl Into<String>,
        fields: &[DataField],
    ) -> Result<Table> {
        let name = name.into();
        if fields.is_empty() {
            return Err(Error::invalid(format!(
                "cannot create table `{name}` with no fields"
            )));
        }
        let columns = fields.iter().map(DataField::to_column_def).collect();
        let primary_key = fields
            .iter()
            .filter(|f| f.primary_key)
            .map(|f| f.base.name.clone())
            .collect();
        self.primary
            .apply_schema(&SchemaChange::CreateTable {
                name: name.clone(),
                columns,
                primary_key,
            })
            .await?;
        self.reload().await?;
        self.require(&name)
    }

    /// Add a field to an existing table, then reload the cache and return the
    /// updated [`Table`].
    pub async fn create_field(&self, table: &str, field: &DataField) -> Result<Table> {
        self.primary
            .apply_schema(&SchemaChange::AddColumn {
                table: table.to_owned(),
                column: field.to_column_def(),
            })
            .await?;
        self.reload().await?;
        self.require(table)
    }

    /// A provider that serves the given table's rows. For a database-backed table
    /// this is the trivial [`DriverTableProvider`] over the primary driver.
    pub fn provider(&self, table: &Table) -> Arc<dyn TableProvider> {
        Arc::new(DriverTableProvider::new(
            self.primary.clone(),
            table.fields.clone(),
        ))
    }

    /// Connect a named file store, making it resolvable by
    /// [`file_store`](Self::file_store). A store whose name is already connected
    /// is replaced (re-connecting the same name re-points it).
    pub fn connect_file_store(&self, store: Arc<dyn FileStore>) -> Result<()> {
        let mut guard = self
            .file_stores
            .write()
            .map_err(|_| Error::msg("catalog file-store registry lock poisoned"))?;
        guard.insert(store.name().to_owned(), store);
        Ok(())
    }

    /// The connected file store with the given name, if any.
    pub fn file_store(&self, name: &str) -> Result<Option<Arc<dyn FileStore>>> {
        let guard = self
            .file_stores
            .read()
            .map_err(|_| Error::msg("catalog file-store registry lock poisoned"))?;
        Ok(guard.get(name).cloned())
    }

    /// The connected file store with the given name, or a
    /// [`NotFound`](Error::NotFound) error.
    pub fn require_file_store(&self, name: &str) -> Result<Arc<dyn FileStore>> {
        self.file_store(name)?
            .ok_or_else(|| Error::not_found(format!("file store `{name}` is not connected")))
    }

    /// The names of every connected file store, sorted.
    pub fn file_store_names(&self) -> Result<Vec<String>> {
        let guard = self
            .file_stores
            .read()
            .map_err(|_| Error::msg("catalog file-store registry lock poisoned"))?;
        let mut names: Vec<String> = guard.keys().cloned().collect();
        names.sort();
        Ok(names)
    }
}
