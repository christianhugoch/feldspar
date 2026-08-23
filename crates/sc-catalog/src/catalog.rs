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
    /// The **secondary** databases an admin has connected (§9's Connections
    /// screen), keyed by the connection's name — which is also the [`DbId`]
    /// stamped onto every table they contribute.
    ///
    /// They are held here, beside the primary, because that is what makes their
    /// tables ordinary: [`reload`](Catalog::reload) introspects them into the
    /// same cache, so a foreign table is found by
    /// [`get`](Catalog::get) and served by [`provider`](Catalog::provider) with
    /// nothing above this crate knowing which database it came from — except
    /// where it should, which is the badge in the admin UI and the refusal to
    /// run DDL against it.
    databases: RwLock<HashMap<String, Arc<dyn DatabaseDriver>>>,
    /// Why a *stored* connection is **not** in [`databases`], keyed by name —
    /// the same arrangement, for the same reason, as
    /// [`file_store_errors`](Catalog::file_store_errors).
    database_errors: RwLock<HashMap<String, String>>,
    /// Tables a connection offered that the catalog did **not** adopt, keyed by
    /// connection name, because a table of that name was already there.
    ///
    /// The catalog keys tables by name, so two databases offering `orders` can
    /// only produce one `orders`. The primary always wins and the loser is
    /// **named** rather than dropped in silence: an admin who connects a
    /// database and cannot find one of its tables has to be able to read why,
    /// and "it clashed with a table you already had" is the whole answer.
    ///
    /// Rebuilt from scratch on every [`reload`](Catalog::reload), like
    /// [`field_overlay_issues`](Catalog::field_overlay_issues).
    shadowed_tables: RwLock<HashMap<String, Vec<String>>>,
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
    /// Who observes row writes (§10.2's emit seam), installed once at boot with
    /// `sc-action`'s trigger dispatcher — `None` in a process that has none (a
    /// build tool, a test), where every write is simply unobserved.
    ///
    /// The catalog holds it because it is what the row layer (layer 8) and
    /// `sc-action` (layer 6) both already have: the write cannot name the
    /// dispatcher without inverting the layering, and this is the inversion.
    table_events: RwLock<Option<Arc<dyn crate::events::TableEvents>>>,
    /// The `_sc_fields` overlay rows that did not cleanly merge on the last
    /// [`reload`](Self::reload) (design §3.2) — a dangling row, a rich type that
    /// does not fit its column, a `Key` with no foreign key behind it. Rebuilt
    /// every reload from scratch, so it always reflects the current schema and
    /// overlay; surfaced to the admin UI by [`field_overlay_issues`](Self::field_overlay_issues).
    field_overlay_issues: RwLock<Vec<FieldMergeIssue>>,
    /// Who observes **schema** changes (Phase 7's seam), installed once at boot
    /// with `sc-server`'s mount registry — `None` in a process that has none.
    ///
    /// Held here for the reason [`table_events`](Catalog::set_table_events) is:
    /// the schema editor (layer 8) and the mount registry (layer 10) cannot name
    /// each other, and the catalog is what they both already hold.
    schema_observer: RwLock<Option<Arc<dyn crate::observer::SchemaObserver>>>,
    /// The **module functions** a formula may hoist and a code body may call
    /// (TODO "Modules in-process", §4a) — `None` in a process with no modules
    /// installed, or none at all.
    ///
    /// Held here for the reason [`table_events`](Catalog::set_table_events) is,
    /// and it is the same inversion. What implements it is `sc-module`'s worker
    /// pool; what needs it is [`prefetch_bindings`](crate::prefetch_bindings)
    /// — which resolves a formula's hoisted calls and is called from three
    /// crates, none of which can name `sc-module` — and `run_js_code`, in a
    /// fourth. The catalog is what all four already hold.
    module_functions: RwLock<Option<Arc<dyn sc_expr::ModuleFnHost>>>,
    /// Where this deployment's applications are reachable in a browser
    /// (`crate::origin`), set once at boot by the process that knows — the
    /// server from its command line, a command-line build from its
    /// `saltcorn.toml` environment. `None` where nobody said, which is a normal
    /// state: a server with no base domain serves no applications.
    public_origin: RwLock<Option<crate::PublicOrigin>>,
}

/// One step of a transactional schema batch ([`Catalog::apply_schema_batch`]).
///
/// Two shapes, because the DDL a schema change needs comes in two: the
/// structured [`SchemaChange`]s the driver renders, and the raw SQL that models
/// what `SchemaChange` deliberately does not — the row-level-security policies,
/// whose `CREATE POLICY` carries an arbitrary boolean expression (§7.3). Both go
/// through the same [`Transaction`](sc_db::Transaction), in the order given.
#[derive(Debug, Clone)]
pub enum SchemaStep {
    /// A structured create/drop table or add/drop column.
    Change(SchemaChange),
    /// Raw DDL generated by trusted code — policies, and nothing else so far.
    Sql(String),
}

impl Catalog {
    /// Build a catalog from a primary driver, loading its tables from
    /// introspection.
    pub async fn init(primary: Arc<dyn DatabaseDriver>) -> Result<Catalog> {
        let catalog = Catalog {
            primary,
            primary_db: DbId::primary(),
            databases: RwLock::new(HashMap::new()),
            database_errors: RwLock::new(HashMap::new()),
            shadowed_tables: RwLock::new(HashMap::new()),
            cache: RwLock::new(HashMap::new()),
            file_stores: RwLock::new(HashMap::new()),
            file_store_errors: RwLock::new(HashMap::new()),
            field_overlay_issues: RwLock::new(Vec::new()),
            schema_observer: RwLock::new(None),
            module_functions: RwLock::new(None),
            public_origin: RwLock::new(None),
            table_events: RwLock::new(None),
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

        // Then every secondary connection, on the same terms: introspection is
        // what makes a table exist here too, so a connected database needs no
        // registration step and a disconnected one contributes nothing.
        //
        // **The primary wins every name.** It hosts `users` and the `_sc_*`
        // tables, and a foreign table quietly taking one of those names would
        // repoint authentication at somebody else's database. So the insert is
        // conditional and the losers are recorded (see `shadowed_tables`).
        // Connections are read out of the lock first: introspection awaits, and
        // the guard must not be held across it.
        let mut shadowed: HashMap<String, Vec<String>> = HashMap::new();
        for (name, driver) in self.connected_databases()? {
            let foreign = match driver.introspect().await {
                Ok(tables) => {
                    self.clear_database_error(&name)?;
                    tables
                }
                // A database that was reachable when it was connected and is not
                // now is exactly the file-store case: the connection stays
                // defined, its tables drop out of the catalog, and the reason is
                // recorded for the admin to read. Failing the whole reload would
                // take the primary database's tables down with it.
                Err(e) => {
                    self.record_database_error(&name, e.to_string())?;
                    continue;
                }
            };
            for physical in &foreign {
                let table = Table::from_physical(DbId(name.clone()), physical);
                if map.contains_key(&table.id) {
                    shadowed
                        .entry(name.clone())
                        .or_default()
                        .push(table.name.clone());
                    continue;
                }
                map.insert(table.id.clone(), table);
            }
        }
        for names in shadowed.values_mut() {
            names.sort();
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

        // Calculated fields (Phase 8) are validated and dependency-ordered here,
        // after every field overlay has merged and before ownership validation —
        // an ownership formula may reference a calc field, so the calc fields
        // must have settled first. Invalid ones are dropped (fail closed) and
        // reported like any other field-overlay issue. The shape passed in still
        // contains them, so a calc field reading another resolves.
        let calc_shape = self.with_module_functions(schema_shape_of(&map));
        field_issues.extend(crate::calc::merge_calc_fields(&mut map, &calc_shape));

        // Ownership formulas were *parsed* by `apply_overlay`; validation needs
        // the whole schema (a Ⱶ-path crosses tables), so it runs here, after
        // every table has merged. A formula that fails validation is cleared —
        // it **grants nothing** (fail closed) — and the reason is left on the
        // table for the admin UI, exactly like a field-overlay issue: reported,
        // never fatal, the table stays usable at its `min_role`s.
        let shape = self.with_module_functions(schema_shape_of(&map));
        let mut ownership_errors: Vec<(TableId, String)> = Vec::new();
        for (id, table) in &map {
            let Some(formula) = &table.ownership else {
                continue;
            };
            match formula.validate(&shape, &table.name) {
                Err(e) => ownership_errors.push((id.clone(), e.to_string())),
                // An ownership formula may not call a module function (§4b),
                // and the check is here as well as on save because a module can
                // be *installed* after a formula was stored: the same source
                // that validated yesterday would start calling a module today.
                // Cleared, like any other invalid rule, so it grants nothing.
                Ok(analysis) => {
                    if let Some(call) = analysis.first_module_call() {
                        ownership_errors.push((
                            id.clone(),
                            format!(
                                "an ownership formula may not call the module function `{}`: a                                  rule that decides who may read a row must fail closed, so a                                  module that is down would deny every read of this table",
                                call.function
                            ),
                        ));
                    }
                }
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
        drop(issues);
        let mut guard = self
            .shadowed_tables
            .write()
            .map_err(|_| Error::msg("catalog shadowed-table lock poisoned"))?;
        *guard = shadowed;
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
        Ok(self.with_module_functions(schema_shape_of(&guard)))
    }

    /// The module functions installed on this catalog, as `(function, module)`
    /// pairs — what a [`SchemaShape`](sc_expr::SchemaShape) declares so a
    /// formula may call one (§4b). Empty on a server with no modules.
    pub fn module_function_names(&self) -> Vec<(String, String)> {
        self.module_functions().map_or_else(Vec::new, |host| {
            host.functions()
                .into_iter()
                .map(|function| (function.name, function.module))
                .collect()
        })
    }

    /// `shape`, with this catalog's module functions declared on it.
    fn with_module_functions(&self, shape: sc_expr::SchemaShape) -> sc_expr::SchemaShape {
        let mut shape = shape;
        for (name, module) in self.module_function_names() {
            shape = shape.module_function(name, module);
        }
        shape
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
        self.create_table_inner(name.into(), fields, false).await
    }

    /// Create a table whose rows are **not worth a WAL record**: Postgres's
    /// `UNLOGGED`, where the primary database supports it (design §7.2).
    ///
    /// Identical to [`create_table`](Self::create_table) in every other respect,
    /// and identical in *every* respect on a backend that does not advertise
    /// [`unlogged_tables`](sc_db::DbCapabilities::unlogged_tables) — the flag
    /// buys write throughput, never semantics, so falling back to an ordinary
    /// table is correct rather than a failure. The trade it makes is real and
    /// belongs to the caller: an unclean shutdown truncates the table, and its
    /// contents never reach a physical standby.
    pub async fn create_unlogged_table(
        &self,
        name: impl Into<String>,
        fields: &[DataField],
    ) -> Result<Table> {
        let unlogged = self.primary.capabilities().unlogged_tables;
        self.create_table_inner(name.into(), fields, unlogged).await
    }

    /// Create a table **in a named database** — the primary, or one of the
    /// connections an admin has added — then reload and return it.
    ///
    /// The database is named rather than inferred, because at this moment there
    /// is nothing to infer it from: the table does not exist yet, so there is no
    /// row in the cache carrying a [`DbId`]. It is the one schema operation that
    /// has to be told, and every later one — add a field, drop a column, drop the
    /// table — reads the answer back off the table it created.
    pub async fn create_table_in(
        &self,
        database: &DbId,
        name: impl Into<String>,
        fields: &[DataField],
    ) -> Result<Table> {
        self.create_table_in_inner(database, name.into(), fields, false)
            .await
    }

    /// The shared body of the two create-table entry points.
    async fn create_table_inner(
        &self,
        name: String,
        fields: &[DataField],
        unlogged: bool,
    ) -> Result<Table> {
        self.create_table_in_inner(&self.primary_db.clone(), name, fields, unlogged)
            .await
    }

    /// The shared body of every create-table entry point, database and all.
    async fn create_table_in_inner(
        &self,
        database: &DbId,
        name: String,
        fields: &[DataField],
        unlogged: bool,
    ) -> Result<Table> {
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
        self.driver_named(database)?
            .apply_schema(&SchemaChange::CreateTable {
                name: name.clone(),
                columns,
                primary_key,
                unlogged,
            })
            .await?;
        self.reload().await?;
        self.require(&name)
    }

    /// The driver that DDL naming `table` must be sent to: the driver of the
    /// database that hosts it.
    ///
    /// The whole reason schema changes are routed rather than sent to
    /// [`primary`](Catalog::primary): a table an admin created on a connection
    /// is theirs to alter and to drop, and the same `ALTER TABLE` sent to the
    /// primary would either fail confusingly or — if a table of that name
    /// existed there too — alter the wrong one, in the wrong database, with no
    /// error to say so.
    ///
    /// A table that is **not in the catalog** routes to the primary, and that is
    /// deliberate rather than a fallback: the callers that create one
    /// legitimately name a table before it exists, and a create names its own
    /// database (see [`create_table_in`](Catalog::create_table_in)) rather than
    /// asking here.
    fn driver_for_table(&self, name: &str) -> Result<Arc<dyn DatabaseDriver>> {
        match self.get(name)? {
            Some(table) => self.driver_for(&table),
            None => Ok(self.primary.clone()),
        }
    }

    /// Add a field to an existing table, then reload the cache and return the
    /// updated [`Table`].
    pub async fn create_field(&self, table: &str, field: &DataField) -> Result<Table> {
        self.driver_for_table(table)?
            .apply_schema(&SchemaChange::AddColumn {
                table: table.to_owned(),
                column: field.to_column_def(),
            })
            .await?;
        self.reload().await?;
        self.require(table)
    }

    /// Drop a table and the overlay rows that describe it, then reload.
    ///
    /// **The overlay goes with the table.** §1.1 keeps an overlay whose table has
    /// vanished on purpose — a restore or an external migration must not lose a
    /// table's access rules — but that rule reads a *deliberate* drop as an
    /// accident. A row left behind by a drop Saltcorn itself performed would be
    /// indistinguishable from a kept orphan, so the two must be told apart at the
    /// one moment anything can: here.
    ///
    /// Refusing what the database would refuse — a table another table's `Key`
    /// references — is the caller's, not this: see
    /// [`SchemaProjection::referencing_fields`](crate::SchemaProjection::referencing_fields)
    /// and `sc_api::schema_edit`, which refuse **by name** before any DDL is
    /// issued. A foreign-key violation out of Postgres is not something an agent
    /// or an admin can act on.
    pub async fn drop_table(&self, name: &str) -> Result<()> {
        // The driver is resolved *before* the overlay is forgotten: after it,
        // `_sc_tables` no longer says which database the table is in, and the
        // cache lookup this needs would be gone with it.
        let driver = self.driver_for_table(name)?;
        self.forget_table_meta(name).await?;
        driver
            .apply_schema(&SchemaChange::DropTable {
                name: name.to_owned(),
                if_exists: false,
            })
            .await?;
        self.reload().await
    }

    /// Drop a column and its `_sc_fields` overlay row, then reload.
    ///
    /// A **calculated** field has no column, so this deletes only the overlay
    /// that introduces it — dropping the column that is not there would be an
    /// error naming a column the admin never created.
    pub async fn drop_field(&self, table: &str, field: &str) -> Result<Table> {
        let driver = self.driver_for_table(table)?;
        let is_calc = self
            .get(table)?
            .and_then(|t| t.field(field).map(DataField::is_calc))
            .unwrap_or(false);
        self.forget_field_meta(table, field).await?;
        if !is_calc {
            driver
                .apply_schema(&SchemaChange::DropColumn {
                    table: table.to_owned(),
                    column: field.to_owned(),
                    if_exists: false,
                })
                .await?;
        }
        self.reload().await?;
        self.require(table)
    }

    /// Delete the `_sc_tables` row for `name` and every `_sc_fields` row for its
    /// fields, without touching the table. The overlay half of
    /// [`drop_table`](Self::drop_table), split out so a batch can do it after its
    /// own DDL has committed.
    pub async fn forget_table_meta(&self, name: &str) -> Result<()> {
        if self.get(FIELD_META_TABLE)?.is_some() {
            for meta in crate::field_meta::list_field_meta_for_table(self, name).await? {
                crate::field_meta::delete_field_meta_row(self, meta.id).await?;
            }
        }
        if self.get(TABLE_META_TABLE)?.is_some()
            && let Some(meta) = crate::table_meta::load_table_meta_by_name(self, name).await?
        {
            crate::table_meta::delete_table_meta_row(self, meta.id).await?;
        }
        Ok(())
    }

    /// Delete the `_sc_fields` row for one field, without touching the column.
    pub async fn forget_field_meta(&self, table: &str, field: &str) -> Result<()> {
        if self.get(FIELD_META_TABLE)?.is_some()
            && let Some(meta) =
                crate::field_meta::load_field_meta_by_field(self, table, field).await?
        {
            crate::field_meta::delete_field_meta_row(self, meta.id).await?;
        }
        Ok(())
    }

    /// Apply a whole list of schema steps in **one** transaction, committing only
    /// if every one of them succeeded (Phase 7).
    ///
    /// This is what makes a batch of schema operations atomic: a refused
    /// operation rolls the transaction back, so a half-built schema is never a
    /// state anybody has to clean up by hand. `Sql` steps carry the DDL
    /// [`SchemaChange`] does not model — the row-level-security policies — which
    /// is why they join the same transaction rather than needing one of their own.
    ///
    /// **The catalog is not reloaded here**: a batch reloads once, when it is
    /// done, rather than once per operation.
    pub async fn apply_schema_batch(&self, steps: &[SchemaStep]) -> Result<()> {
        self.apply_schema_batch_in(&self.primary_db.clone(), steps)
            .await
    }

    /// [`apply_schema_batch`](Self::apply_schema_batch) against a **named**
    /// database.
    ///
    /// One batch, one database, and that is a constraint rather than an
    /// oversight: a batch is one transaction, and a transaction cannot span two
    /// Postgres servers. A caller with operations for two databases issues two
    /// batches and knows that the second failing does not undo the first —
    /// `sc_api::schema_edit` refuses such a batch outright rather than pretending
    /// otherwise.
    pub async fn apply_schema_batch_in(&self, database: &DbId, steps: &[SchemaStep]) -> Result<()> {
        let mut tx = self.driver_named(database)?.begin().await?;
        for step in steps {
            let outcome = match step {
                SchemaStep::Change(change) => tx.apply_schema(change).await,
                SchemaStep::Sql(sql) => tx.batch(sql).await,
            };
            if let Err(e) = outcome {
                let _ = tx.rollback().await;
                return Err(e);
            }
        }
        tx.commit().await
    }

    /// Install the listener for schema changes — `sc-server`'s mount registry,
    /// once, at boot (Phase 7's schema seam).
    ///
    /// Replaces any previous one, for the reason
    /// [`set_table_events`](Self::set_table_events) does.
    pub fn set_schema_observer(&self, observer: Arc<dyn crate::observer::SchemaObserver>) {
        if let Ok(mut guard) = self.schema_observer.write() {
            *guard = Some(observer);
        }
    }

    /// Tell the installed observer, if any, that the schema moved.
    ///
    /// Called **after** the DDL committed and the cache reloaded. An `Err` is the
    /// *reaction* failing, never the change; the caller reports it beside the
    /// result rather than pretending the schema stayed put.
    pub fn notify_schema_changed(&self, change: &crate::observer::SchemaChanged) -> Result<()> {
        let observer = {
            let guard = self
                .schema_observer
                .read()
                .map_err(|_| Error::msg("catalog schema-observer lock poisoned"))?;
            guard.clone()
        };
        match observer {
            Some(observer) => observer.schema_changed(self, change),
            None => Ok(()),
        }
    }

    /// Record where this deployment's applications are reachable in a browser
    /// (see [`crate::origin`]), replacing anything set before.
    ///
    /// Called once at boot by the process that knows: `saltcorn serve` from its
    /// `--base-domain`/`--bind`, a command-line build from the `saltcorn.toml`
    /// environment it connected with. It is not database state and is not
    /// persisted — it is a fact about *this process's* view of the deployment,
    /// held here because the project generator that needs it already has a
    /// catalog and nothing else it could ask.
    pub fn set_public_origin(&self, origin: crate::PublicOrigin) {
        if let Ok(mut guard) = self.public_origin.write() {
            *guard = Some(origin);
        }
    }

    /// Where applications are reachable, if this process was told.
    ///
    /// `None` is a normal answer — no base domain means no application is
    /// addressable — and every caller is expected to have something sensible to
    /// say instead of a hostname it made up.
    pub fn public_origin(&self) -> Option<crate::PublicOrigin> {
        self.public_origin
            .read()
            .ok()
            .and_then(|guard| guard.clone())
    }

    /// Ensure a system metadata table exists with (at least) `fields`, creating
    /// it if absent and **additively reconciling** it if it is already there.
    ///
    /// This is the one-time bootstrap every `_sc_*` table performs
    /// (`_sc_applications`, `_sc_triggers`, `_sc_file_stores`), factored here so
    /// there is one answer to "what happens when a release adds a column".
    ///
    /// The design bans a migration framework for now, and without one an existing
    /// database would simply lack the new column — every read of that table would
    /// then fail on a database that was working yesterday. So a declared column
    /// the table does not have is **created** ([`create_field`](Self::create_field)),
    /// which is the subset of migration that is always safe: nothing is dropped,
    /// nothing is renamed, and no data is rewritten. A column that exists is left
    /// exactly as it is — this never re-types or re-constrains one, because that
    /// *is* a migration and needs a framework that can decide what to do with the
    /// rows already there.
    ///
    /// The corollary a caller must respect: **a field added to an existing
    /// table's declaration cannot be `required`**. The rows already stored have no
    /// value for it, so `NOT NULL` would be rejected by the database (or, worse,
    /// accepted with an invented default). Such a column is nullable, and its
    /// reader treats `NULL` as the empty value.
    pub async fn bootstrap_table(&self, name: &str, fields: &[DataField]) -> Result<Table> {
        let Some(existing) = self.get(name)? else {
            return self.create_table(name, fields).await;
        };
        let mut table = existing;
        for field in fields {
            if table.field(&field.base.name).is_none() {
                table = self.create_field(name, field).await?;
            }
        }
        Ok(table)
    }

    /// A provider that serves the given table's rows: the trivial
    /// [`DriverTableProvider`] over the driver of the database that **hosts**
    /// it.
    ///
    /// It returns a `Result` for one reason, and it is the reason the whole
    /// multi-database arrangement is safe: a table stamped with a connection
    /// that is no longer connected has no driver, and the honest answer is to
    /// say so. Falling back to the primary would run somebody's query against
    /// the wrong database — the same table name, different rows — which is a
    /// failure nobody would see until the data was wrong.
    pub fn provider(&self, table: &Table) -> Result<Arc<dyn TableProvider>> {
        Ok(Arc::new(DriverTableProvider::new(
            self.driver_for(table)?,
            table.fields.clone(),
        )))
    }

    /// The driver of the database hosting `table` — the primary, or the
    /// secondary connection whose name the table's [`DbId`] carries.
    pub fn driver_for(&self, table: &Table) -> Result<Arc<dyn DatabaseDriver>> {
        if table.database == self.primary_db {
            return Ok(self.primary.clone());
        }
        self.database(&table.database.0)?.ok_or_else(|| {
            Error::not_found(format!(
                "table `{}` is served by database connection `{}`, which is not connected",
                table.name, table.database.0
            ))
        })
    }

    /// The driver of the database with this id: the primary, or a connected
    /// secondary.
    pub fn driver_named(&self, database: &DbId) -> Result<Arc<dyn DatabaseDriver>> {
        if *database == self.primary_db {
            return Ok(self.primary.clone());
        }
        self.database(&database.0)?.ok_or_else(|| {
            Error::not_found(format!(
                "database connection `{}` is not connected",
                database.0
            ))
        })
    }

    /// Register a secondary database under `name`, making its tables eligible
    /// for the next [`reload`](Catalog::reload).
    ///
    /// Replaces any driver already connected under that name — re-connecting a
    /// name re-points it, which is what editing a connection's host does — and
    /// clears any recorded [`database_error`](Catalog::database_error), since
    /// the connection demonstrably works now.
    ///
    /// `primary` is refused rather than shadowed: it is the [`DbId`] every table
    /// of the primary database carries, and a second holder of that name would
    /// make `driver_for` ambiguous in the one direction that matters.
    pub fn connect_database(&self, name: &str, driver: Arc<dyn DatabaseDriver>) -> Result<()> {
        if name == self.primary_db.0 {
            return Err(Error::invalid(format!(
                "`{name}` is the name of the primary database and cannot name a connection"
            )));
        }
        let mut guard = self
            .databases
            .write()
            .map_err(|_| Error::msg("catalog database registry lock poisoned"))?;
        guard.insert(name.to_owned(), driver);
        drop(guard);
        self.clear_database_error(name)
    }

    /// Disconnect the secondary database named `name`, returning whether one was
    /// connected. Also clears any recorded connection error for it.
    ///
    /// The caller reloads afterwards: this removes the driver, and it is the
    /// reload that removes its tables from the cache.
    pub fn disconnect_database(&self, name: &str) -> Result<bool> {
        let mut guard = self
            .databases
            .write()
            .map_err(|_| Error::msg("catalog database registry lock poisoned"))?;
        let existed = guard.remove(name).is_some();
        drop(guard);
        self.clear_database_error(name)?;
        Ok(existed)
    }

    /// The connected secondary database with the given name, if any.
    pub fn database(&self, name: &str) -> Result<Option<Arc<dyn DatabaseDriver>>> {
        let guard = self
            .databases
            .read()
            .map_err(|_| Error::msg("catalog database registry lock poisoned"))?;
        Ok(guard.get(name).cloned())
    }

    /// The names of every connected secondary database, sorted.
    pub fn database_names(&self) -> Result<Vec<String>> {
        let guard = self
            .databases
            .read()
            .map_err(|_| Error::msg("catalog database registry lock poisoned"))?;
        let mut names: Vec<String> = guard.keys().cloned().collect();
        drop(guard);
        names.sort();
        Ok(names)
    }

    /// Every connected secondary database as `(name, driver)`, cloned out of the
    /// lock so a caller may await between them.
    fn connected_databases(&self) -> Result<Vec<(String, Arc<dyn DatabaseDriver>)>> {
        let guard = self
            .databases
            .read()
            .map_err(|_| Error::msg("catalog database registry lock poisoned"))?;
        let mut pairs: Vec<(String, Arc<dyn DatabaseDriver>)> = guard
            .iter()
            .map(|(name, driver)| (name.clone(), driver.clone()))
            .collect();
        drop(guard);
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(pairs)
    }

    /// Record why the connection named `name` is not usable, so the admin UI can
    /// show a connection that is defined but not connected, with the reason.
    pub fn record_database_error(&self, name: &str, error: impl Into<String>) -> Result<()> {
        let mut guard = self
            .database_errors
            .write()
            .map_err(|_| Error::msg("catalog database error registry lock poisoned"))?;
        guard.insert(name.to_owned(), error.into());
        Ok(())
    }

    /// Why the connection named `name` is not usable, if it is not.
    pub fn database_error(&self, name: &str) -> Result<Option<String>> {
        let guard = self
            .database_errors
            .read()
            .map_err(|_| Error::msg("catalog database error registry lock poisoned"))?;
        Ok(guard.get(name).cloned())
    }

    /// Forget any recorded connection error for `name`.
    fn clear_database_error(&self, name: &str) -> Result<()> {
        let mut guard = self
            .database_errors
            .write()
            .map_err(|_| Error::msg("catalog database error registry lock poisoned"))?;
        guard.remove(name);
        Ok(())
    }

    /// The tables the connection named `name` offered and the catalog did not
    /// adopt, because something already held the name (see
    /// [`shadowed_tables`](Catalog::shadowed_tables)). Sorted; empty is the
    /// ordinary case.
    pub fn shadowed_tables(&self, name: &str) -> Result<Vec<String>> {
        let guard = self
            .shadowed_tables
            .read()
            .map_err(|_| Error::msg("catalog shadowed-table lock poisoned"))?;
        Ok(guard.get(name).cloned().unwrap_or_default())
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

    /// Install the listener for row writes — `sc-action`'s trigger dispatcher,
    /// once, at boot (§10.2's emit seam).
    ///
    /// Replaces any previous one rather than refusing: a process installs exactly
    /// one, and a test that installs a second means the second.
    pub fn set_table_events(&self, events: Arc<dyn crate::events::TableEvents>) -> Result<()> {
        let mut guard = self
            .table_events
            .write()
            .map_err(|_| Error::msg("catalog table-events lock poisoned"))?;
        *guard = Some(events);
        Ok(())
    }

    /// Install the module functions — `sc-server`'s `ModuleServices`, at boot
    /// and again after every module change (install, configure, delete, Reload).
    ///
    /// Replaces any previous one rather than refusing, for
    /// [`set_table_events`](Catalog::set_table_events)' reason: the module set
    /// is rebuilt whole on every change, so the second install *is* the answer.
    pub fn set_module_functions(&self, functions: Arc<dyn sc_expr::ModuleFnHost>) -> Result<()> {
        let mut guard = self
            .module_functions
            .write()
            .map_err(|_| Error::msg("catalog module-functions lock poisoned"))?;
        *guard = Some(functions);
        Ok(())
    }

    /// The installed module functions, cloned out of the lock — `None` where
    /// nobody installed any, which is what makes `modfn` unbound in a code body
    /// and a module function in a formula an unknown identifier.
    ///
    /// A poisoned lock reads as "there are none": this is asked on the read and
    /// write paths, where the honest answer to "is the module registry broken"
    /// is a formula that fails naming its call rather than every query failing.
    pub fn module_functions(&self) -> Option<Arc<dyn sc_expr::ModuleFnHost>> {
        self.module_functions.read().ok()?.clone()
    }

    /// Whether anything listens for `op` on `table` — the question the row layer
    /// asks **before** doing any work for the feature, so a write nobody observes
    /// pays one lock and one lookup rather than a query.
    ///
    /// A poisoned lock reads as "nobody listens": this is asked on the write
    /// path, where the honest answer to "is the listener registry broken" is to
    /// let the write proceed unobserved rather than to fail it.
    pub fn observes_writes(&self, table: &str, op: crate::events::WriteOp) -> bool {
        match self.listener() {
            Ok(Some(events)) => events.observes(table, op),
            _ => false,
        }
    }

    /// Hand one committed write to the listener, if there is one.
    ///
    /// An `Err` is the *dispatch* failing, never the write — which has already
    /// happened by the time this is called (decision 1: after commit). The caller
    /// reports it and returns the row.
    pub async fn emit_write(&self, write: crate::events::TableWrite<'_>) -> Result<()> {
        match self.listener()? {
            Some(events) => events.emit(self, write).await,
            None => Ok(()),
        }
    }

    /// The installed listener, **cloned out of the lock**: dispatch runs actions,
    /// which write rows, which come back here — so the guard must not be held
    /// across the await.
    fn listener(&self) -> Result<Option<Arc<dyn crate::events::TableEvents>>> {
        let guard = self
            .table_events
            .read()
            .map_err(|_| Error::msg("catalog table-events lock poisoned"))?;
        Ok(guard.clone())
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
pub(crate) const USERS_TABLE: &str = "users";
pub(crate) const USERS_PASSWORD_COLUMN: &str = "password_hash";

/// Project a table cache into the [`sc_expr::SchemaShape`] formula validation
/// and translation consume.
fn schema_shape_of(map: &HashMap<TableId, Table>) -> sc_expr::SchemaShape {
    schema_shape_of_tables(map.values())
}

/// Project any set of tables into the [`sc_expr::SchemaShape`] formula
/// validation and translation consume: field names, Key targets, and the user
/// object's fields — everything the user table has except the password hash,
/// which a formula has no business reading (`sc-auth` never exposes it on a
/// `User` either, so a formula naming it would bind nothing and always deny).
///
/// Taken over an iterator rather than the cache, because a schema *edit*
/// validates against the schema it will leave behind rather than the one in the
/// cache (Phase 7) — see [`SchemaProjection`](crate::SchemaProjection).
pub(crate) fn schema_shape_of_tables<'a>(
    tables: impl Iterator<Item = &'a Table>,
) -> sc_expr::SchemaShape {
    let mut shape = sc_expr::SchemaShape::new();
    let mut users: Option<&Table> = None;
    for table in tables {
        if table.name == USERS_TABLE {
            users = Some(table);
        }
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
        // A single-column primary key lets aggregations over this table as a
        // child (Phase 7) count rows and break `maxBy`/`minBy` ties; a composite
        // or absent key leaves it unset, so those simply do not translate.
        if let [pk] = table.primary_key.as_slice() {
            table_shape = table_shape.primary_key(pk);
        }
        shape = shape.table(&table.name, table_shape);
    }
    if let Some(users) = users {
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
