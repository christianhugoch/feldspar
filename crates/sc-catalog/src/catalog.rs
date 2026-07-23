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
use crate::field_meta::{FIELD_META_TABLE, list_field_meta};
use crate::provider::{DriverTableProvider, TableProvider};
use crate::table::{FieldMergeIssue, Table};
use crate::table_meta::{TABLE_META_TABLE, list_table_meta};

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
    /// Why a stored file store is **not** in [`file_stores`], keyed by name.
    ///
    /// A store that failed to connect must not vanish silently: its definition is
    /// still there, the admin still needs to see it in the list, and the reason —
    /// "directory /srv/docs does not exist" — is the only thing that tells them
    /// what to fix. Absence of a name here means either "connected" or "never
    /// attempted"; the definition list plus [`file_store`](Self::file_store)
    /// distinguishes those.
    file_store_errors: RwLock<HashMap<String, String>>,
    /// The `_sc_fields` overlay rows that did not cleanly merge on the last
    /// [`reload`](Self::reload) (design §3.2) — a dangling row, a rich type that
    /// does not fit its column, a `Key` with no foreign key behind it. Rebuilt
    /// every reload from scratch, so it always reflects the current schema and
    /// overlay; surfaced to the admin UI by [`field_overlay_issues`](Self::field_overlay_issues).
    field_overlay_issues: RwLock<Vec<FieldMergeIssue>>,
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
            file_store_errors: RwLock::new(HashMap::new()),
            field_overlay_issues: RwLock::new(Vec::new()),
        };
        catalog.reload().await?;
        Ok(catalog)
    }

    /// The primary database driver.
    pub fn primary(&self) -> &Arc<dyn DatabaseDriver> {
        &self.primary
    }

    /// Re-introspect the primary database and rebuild the table cache, applying
    /// the `_sc_tables` overlay on top. Called after every schema change the
    /// catalog applies, and after every overlay change, so the cache never
    /// drifts from either source.
    ///
    /// **Introspection is still what makes a table exist.** The overlay only
    /// adds to a table already found, so a database with no `_sc_tables` table
    /// — or one whose rows describe tables that are not there — behaves exactly
    /// as it did before the overlay existed. That is the zero-setup promise of
    /// §9 in one line of code: the loop below can only ever modify entries the
    /// introspection loop above it created.
    pub async fn reload(&self) -> Result<()> {
        let physicals = self.primary.introspect().await?;
        let mut map = HashMap::with_capacity(physicals.len());
        for physical in &physicals {
            let table = Table::from_physical(self.primary_db.clone(), physical);
            map.insert(table.id.clone(), table);
        }

        // Only query the overlay when the database has one. Asking first is not
        // defensiveness — it is required: `bootstrap_table_meta` creates the
        // table *through* `create_table`, which reloads, so this runs at least
        // once on a database where `_sc_tables` genuinely does not exist yet.
        // Selecting from it there would make bootstrapping impossible.
        if map.contains_key(&TableId(TABLE_META_TABLE.to_owned())) {
            for meta in list_table_meta(self).await? {
                // An overlay for a table that is not here is not an error and is
                // not dropped; see `orphan_table_meta` for why it is kept.
                if let Some(table) = map.get_mut(&TableId(meta.table_name.clone())) {
                    table.apply_overlay(&meta);
                }
            }
        }

        // Then the `_sc_fields` overlay, onto the fields the table now has. Same
        // "only when the table exists" guard and same bootstrapping reason as
        // above. The merge reports rather than fails, so its issues are collected
        // here and stored beside the cache.
        let mut field_issues = Vec::new();
        if map.contains_key(&TableId(FIELD_META_TABLE.to_owned())) {
            for meta in list_field_meta(self).await? {
                match map.get_mut(&TableId(meta.table_name.clone())) {
                    Some(table) => field_issues.extend(table.apply_field_overlay(&meta)),
                    // A field overlay whose whole table is gone is a dangling row
                    // too — kept (like an orphan table overlay) and reported.
                    None => field_issues.push(FieldMergeIssue {
                        table: meta.table_name.clone(),
                        field: meta.field_name.clone(),
                        message: format!(
                            "field overlay names table `{}`, which is not in the catalog",
                            meta.table_name
                        ),
                    }),
                }
            }
        }

        // Ownership formulas were *parsed* by `apply_overlay`; validation needs
        // the whole schema (a Ⱶ-path crosses tables), so it runs here, after
        // every table has merged. A formula that fails validation is cleared —
        // it **grants nothing** (fail closed) — and the reason is left on the
        // table for the admin UI, exactly like a field-overlay issue: reported,
        // never fatal, the table stays usable at its `min_role`s.
        let shape = schema_shape_of(&map);
        let mut ownership_errors: Vec<(TableId, String)> = Vec::new();
        for (id, table) in &map {
            if let Some(formula) = &table.ownership
                && let Err(e) = formula.validate(&shape, &table.name)
            {
                ownership_errors.push((id.clone(), e.to_string()));
            }
        }
        for (id, message) in ownership_errors {
            if let Some(table) = map.get_mut(&id) {
                table.ownership = None;
                table.ownership_error = Some(message);
            }
        }

        // The DB I/O is done; take the locks only to swap in the new snapshots so
        // they are never held across an await.
        let mut guard = self
            .cache
            .write()
            .map_err(|_| Error::msg("catalog cache lock poisoned"))?;
        *guard = map;
        drop(guard);
        let mut issues = self
            .field_overlay_issues
            .write()
            .map_err(|_| Error::msg("catalog field-overlay issue lock poisoned"))?;
        *issues = field_issues;
        Ok(())
    }

    /// The `_sc_fields` overlay rows that did not cleanly merge on the last
    /// [`reload`](Self::reload), for the admin UI to surface (design §3.2). Empty
    /// when every stored field overlay applied cleanly, which is the ordinary
    /// case.
    pub fn field_overlay_issues(&self) -> Result<Vec<FieldMergeIssue>> {
        let guard = self
            .field_overlay_issues
            .read()
            .map_err(|_| Error::msg("catalog field-overlay issue lock poisoned"))?;
        Ok(guard.clone())
    }

    /// The catalog described as an `sc_expr` [`SchemaShape`] — what formula
    /// validation and translation (§7.3) see: every table's fields, each Key
    /// field's target, and the user object's fields. This is the projection
    /// that keeps `sc-expr` below the catalog in the dependency graph: the
    /// catalog describes tables *to* it, never the other way around.
    pub fn schema_shape(&self) -> Result<sc_expr::SchemaShape> {
        let guard = self
            .cache
            .read()
            .map_err(|_| Error::msg("catalog cache lock poisoned"))?;
        Ok(schema_shape_of(&guard))
    }

    /// Each user field mapped to its SQL type — what the GUC translation
    /// (`UserEnv::Guc`) casts a `user.x` extraction to when generating RLS
    /// policies, and what the save-time RLS check uses. The password hash is
    /// excluded for the same reason [`schema_shape`](Self::schema_shape)
    /// excludes it: not formula business.
    pub fn user_field_types(&self) -> Result<std::collections::BTreeMap<String, String>> {
        let mut map = std::collections::BTreeMap::new();
        if let Some(users) = self.get(USERS_TABLE)? {
            for field in &users.fields {
                if field.base.name != USERS_PASSWORD_COLUMN {
                    map.insert(
                        field.base.name.clone(),
                        field.base.type_.sql_type().to_owned(),
                    );
                }
            }
        }
        Ok(map)
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
    /// is replaced (re-connecting the same name re-points it), which is what an
    /// edit to a store's path does.
    ///
    /// Connecting clears any recorded [`file_store_error`](Self::file_store_error)
    /// for that name: the store is demonstrably working now, so a stale reason it
    /// once failed would be shown to the admin as if it were current.
    pub fn connect_file_store(&self, store: Arc<dyn FileStore>) -> Result<()> {
        let name = store.name().to_owned();
        let mut guard = self
            .file_stores
            .write()
            .map_err(|_| Error::msg("catalog file-store registry lock poisoned"))?;
        guard.insert(name.clone(), store);
        drop(guard);
        self.clear_file_store_error(&name)
    }

    /// Disconnect the file store named `name`, returning whether one was
    /// connected. Also clears any recorded connection error for it.
    ///
    /// The counterpart to [`connect_file_store`](Self::connect_file_store), and
    /// what a deleted or renamed store needs: without this a store's definition
    /// could be removed while its handle kept serving, so the file manager would
    /// happily browse a store the admin had just deleted. Re-pointing an existing
    /// store does *not* need this — connecting the same name replaces it.
    ///
    /// This removes the handle only. Nothing on disk is touched; see
    /// [`delete_file_store`](crate::delete_file_store) for why that separation
    /// matters.
    pub fn disconnect_file_store(&self, name: &str) -> Result<bool> {
        let mut guard = self
            .file_stores
            .write()
            .map_err(|_| Error::msg("catalog file-store registry lock poisoned"))?;
        let existed = guard.remove(name).is_some();
        drop(guard);
        self.clear_file_store_error(name)?;
        Ok(existed)
    }

    /// Record why the store named `name` could not be connected, so the admin UI
    /// can show a store that is defined but not usable, with the reason.
    pub fn record_file_store_error(&self, name: &str, error: impl Into<String>) -> Result<()> {
        let mut guard = self
            .file_store_errors
            .write()
            .map_err(|_| Error::msg("catalog file-store error registry lock poisoned"))?;
        guard.insert(name.to_owned(), error.into());
        Ok(())
    }

    /// Why the store named `name` is not connected, if it failed to connect.
    pub fn file_store_error(&self, name: &str) -> Result<Option<String>> {
        let guard = self
            .file_store_errors
            .read()
            .map_err(|_| Error::msg("catalog file-store error registry lock poisoned"))?;
        Ok(guard.get(name).cloned())
    }

    /// Forget any recorded connection error for `name`.
    fn clear_file_store_error(&self, name: &str) -> Result<()> {
        let mut guard = self
            .file_store_errors
            .write()
            .map_err(|_| Error::msg("catalog file-store error registry lock poisoned"))?;
        guard.remove(name);
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

/// The user table's name and its password column. These mirror `sc-auth`'s
/// constants — that crate sits *above* this one in the layering, so the names
/// are restated here rather than imported. Both are design constants (§7.1:
/// users live in a table called `users`; the hash column is not deletable), not
/// configuration.
const USERS_TABLE: &str = "users";
const USERS_PASSWORD_COLUMN: &str = "password_hash";

/// Project a table cache into the [`sc_expr::SchemaShape`] formula validation
/// and translation consume: field names, Key targets, and the user object's
/// fields — everything the user table has except the password hash, which a
/// formula has no business reading (`sc-auth` never exposes it on a `User`
/// either, so a formula naming it would bind nothing and always deny).
fn schema_shape_of(map: &HashMap<TableId, Table>) -> sc_expr::SchemaShape {
    let mut shape = sc_expr::SchemaShape::new();
    for table in map.values() {
        let mut table_shape = sc_expr::TableShape::new();
        for field in &table.fields {
            table_shape = match &field.kind {
                crate::field::DataFieldKind::Key {
                    target_table,
                    target_field,
                    ..
                } => table_shape.key_field(&field.base.name, &target_table.0, &target_field.0),
                _ => table_shape.field(&field.base.name),
            };
        }
        shape = shape.table(&table.name, table_shape);
    }
    if let Some(users) = map.get(&TableId(USERS_TABLE.to_owned())) {
        shape = shape.user_fields(
            users
                .fields
                .iter()
                .map(|f| f.base.name.as_str())
                .filter(|name| *name != USERS_PASSWORD_COLUMN),
        );
    }
    shape
}
