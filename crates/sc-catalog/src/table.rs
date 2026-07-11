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
    /// Per-CRUD access rules.
    pub access: AccessRules,
    /// Table-level attributes (JSON object).
    pub attributes: Attrs,
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
            access: AccessRules::default(),
            attributes: Attrs::new(),
        }
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
