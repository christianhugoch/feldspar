//! [`Table`]: a table as the catalog presents it.
//!
//! A `Table` is pure data — its id, name, database, fields, primary key, access
//! rules, and attributes — built either from introspection
//! ([`Table::from_physical`]) or from a create request. The *behaviour* of
//! serving rows lives in a [`TableProvider`](crate::TableProvider), which the
//! [`Catalog`](crate::Catalog) constructs on demand; [`TableSource`] records
//! which kind of provider backs the table so that identity stays serialisable
//! and comparable (technical design §8.2).

use std::collections::BTreeSet;

use sc_db::PhysicalTable;

use crate::field::{Attrs, DataField, DataFieldKind, DbId, FieldId, TableId};
use crate::table_meta::{TableMeta, TableMetaId};

/// Which kind of provider serves a table's rows (technical design §8.3). The MVP
/// only has database-backed tables; virtual providers (RSS, IMAP, search, …) add
/// variants here later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableSource {
    /// Backed directly by a table in the connected database `database`.
    Database,
}

/// Per-CRUD access control for a table (technical design §8.2). Roles run 1–100
/// with **lower = more privileged** (1 = admin, 100 = public), so a `min_role` is
/// the least-privileged role number still allowed: a user with `role <= min_role`
/// passes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessRules {
    /// Least-privileged role that may read rows.
    pub min_role_read: u8,
    /// Least-privileged role that may create/update/delete rows.
    pub min_role_write: u8,
}

impl Default for AccessRules {
    /// Admin-only, matching the MVP admin UI: only role 1 (admin) may read or
    /// write. An introspected table carries no overlay metadata, so this default
    /// applies until `_sc_tables` overlays arrive (post-MVP).
    fn default() -> Self {
        AccessRules {
            min_role_read: 1,
            min_role_write: 1,
        }
    }
}

/// A table as the catalog knows it (technical design §8.2).
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    /// Stable identity (the table name in the MVP).
    pub id: TableId,
    /// The (unqualified) table name.
    pub name: String,
    /// The database that hosts it.
    pub database: DbId,
    /// Which kind of provider serves its rows.
    pub source: TableSource,
    /// Its fields, in declaration order. A composite or absent primary key is
    /// allowed.
    pub fields: Vec<DataField>,
    /// Names of the primary-key columns, in key order (empty for none; more than
    /// one for a composite key).
    pub primary_key: Vec<String>,
    /// A human label. Defaults to the table's own name; an overlay row may
    /// replace it.
    pub label: String,
    /// A human description; empty unless an overlay row gives one.
    pub description: String,
    /// Per-CRUD access rules.
    pub access: AccessRules,
    /// Table-level attributes (JSON object).
    pub attributes: Attrs,
    /// The overlay row this table's configuration came from, if it has one.
    ///
    /// `None` is the ordinary case and the important one: it means "nobody has
    /// configured this table", which is exactly how a freshly connected database
    /// looks and must keep looking (§9). It is recorded rather than inferred so
    /// that saving a change updates the existing row instead of racing to create
    /// a second one for the same table.
    pub overlay: Option<TableMetaId>,
}

impl Table {
    /// Build a catalog table from an introspected [`PhysicalTable`].
    ///
    /// Primary-key membership is taken from the physical key; a column that is
    /// the sole local column of a foreign key becomes a
    /// [`Key`](DataFieldKind::Key) field (with no summary field, which is overlay
    /// metadata), and every other column is [`Plain`](DataFieldKind::Plain).
    pub fn from_physical(database: DbId, physical: &PhysicalTable) -> Table {
        let pk: BTreeSet<&str> = physical.primary_key.iter().map(String::as_str).collect();
        let fields = physical
            .columns
            .iter()
            .map(|col| {
                let is_pk = pk.contains(col.name.as_str());
                let kind = single_column_fk_target(physical, &col.name)
                    .map(|(table, field)| DataFieldKind::Key {
                        target_table: TableId(table),
                        target_field: FieldId(field),
                        summary_field: None,
                    })
                    .unwrap_or(DataFieldKind::Plain);
                DataField::from_column(col, is_pk, kind)
            })
            .collect();
        Table {
            id: TableId(physical.name.clone()),
            name: physical.name.clone(),
            database,
            source: TableSource::Database,
            fields,
            primary_key: physical.primary_key.clone(),
            label: physical.name.clone(),
            description: String::new(),
            access: AccessRules::default(),
            attributes: Attrs::new(),
            overlay: None,
        }
    }

    /// Apply an overlay row to this table (technical design §9, TODO §1.2).
    ///
    /// **The precedence rule, stated once:** the database is the authority on
    /// everything it knows — columns, types, nullability, keys, the primary key
    /// — and the overlay is the authority on everything *it* knows — access
    /// rules, label, description, attributes. The two sets do not intersect, so
    /// there is no contested value and no conflict semantics to get wrong. That
    /// is a constraint on what may ever be added to `_sc_tables`, not merely a
    /// description of what it holds today: a `nullable` column there would break
    /// this method's correctness, which is why `table_meta`'s column list is
    /// asserted in full by a test rather than spot-checked.
    ///
    /// A **system table never takes an overlay**. `save_table_meta` refuses to
    /// write one, so this only fires on a row inserted behind the API's back —
    /// and the answer there is to ignore it, because `_sc_*` tables are hidden
    /// from users (§9) and their access is not configurable by anyone.
    ///
    /// An empty label or description in the row means "none given", so the
    /// table's own name stays its label rather than becoming blank.
    pub fn apply_overlay(&mut self, meta: &TableMeta) {
        if self.is_system() {
            return;
        }
        if !meta.label.is_empty() {
            self.label = meta.label.clone();
        }
        if !meta.description.is_empty() {
            self.description = meta.description.clone();
        }
        self.access = meta.access.clone();
        self.attributes = meta.attributes.clone();
        self.overlay = Some(meta.id);
    }

    /// Whether this is a system table (`_sc_*`), hidden from users (technical
    /// design §9). No such tables exist in the MVP, but the check is defined so
    /// callers can filter consistently.
    pub fn is_system(&self) -> bool {
        self.name.starts_with("_sc_")
    }

    /// The field with the given name, if present.
    pub fn field(&self, name: &str) -> Option<&DataField> {
        self.fields.iter().find(|f| f.base.name == name)
    }
}

/// If `column` is the single local column of some foreign key on `physical`,
/// return the `(referenced_table, referenced_column)` it points at.
fn single_column_fk_target(physical: &PhysicalTable, column: &str) -> Option<(String, String)> {
    physical.foreign_keys.iter().find_map(|fk| {
        match (fk.columns.as_slice(), fk.referenced_columns.as_slice()) {
            ([local], [referenced]) if local == column => {
                Some((fk.referenced_table.clone(), referenced.clone()))
            }
            _ => None,
        }
    })
}
