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
use sc_types::{RichTypeRef, TypeRef};

use crate::field::{Attrs, DataField, DataFieldKind, DbId, FieldId, TableId};
use crate::field_meta::FieldMeta;
use crate::table_meta::{TableMeta, TableMetaId};

/// A way in which a stored `_sc_fields` overlay row did **not** cleanly apply to
/// the introspected field it names (design §3.2).
///
/// The merge never fails and never silently downgrades: a row that cannot be
/// honoured leaves the field as introspection produced it — still usable — and
/// records why here, for the admin UI to surface. The two reasons are a dangling
/// row (the column, or its table, is gone) and a rich type that does not fit the
/// column's SQL type. Each is exactly what a hand-edited database or a restored
/// dump can produce, and each is the admin's to fix, not a bug to crash on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldMergeIssue {
    /// The table the overlay row named.
    pub table: String,
    /// The field the overlay row named.
    pub field: String,
    /// A human-readable explanation of why the row did not apply.
    pub message: String,
}

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

    /// Apply a field overlay row to the matching field of this table (design
    /// §3.2), returning any way in which it did not cleanly apply.
    ///
    /// **Same precedence rule as [`apply_overlay`](Table::apply_overlay), one
    /// level down:** the database is the authority on what it knows (the column's
    /// SQL type, its nullability, whether a foreign key stands behind it), and the
    /// overlay is the authority on what *it* knows (a label, a rich type, a field
    /// kind's extra parameters, attributes). Where the two would contradict, the
    /// database wins and the contradiction is *reported*, never resolved by
    /// downgrading the column or by crashing — the table stays usable.
    ///
    /// The rules that make that concrete:
    ///
    /// - A **rich type** applies only if its `sql_types()` includes the column's
    ///   actual SQL type. `text` configured as `Integer` is reported and the
    ///   column stays its basic type. An unregistered type name is likewise
    ///   reported (it may belong to a plugin not loaded here).
    /// - A **`Key`** overlay's target is the database's whenever the database has
    ///   one: on a column introspection made a key from a foreign key, the
    ///   overlay may add only its `summary_field` and can never repoint the
    ///   reference. On a column with *no* foreign key, the overlay supplies the
    ///   whole reference — the case the database cannot enforce, such as a key
    ///   onto a field in another database (a future capability), where the
    ///   overlay is the only place the target can come from.
    /// - A **`File`** overlay applies to any column (its storage is `text`); it
    ///   is a reference the database does not model, so there is nothing to
    ///   contradict.
    /// - `label` and `attributes` are the overlay's to set and are applied even
    ///   when a type or kind issue is also reported — the field stays usable and
    ///   the admin is told what did not take.
    ///
    /// A **system table never takes a field overlay**, matching
    /// [`apply_overlay`](Table::apply_overlay): `save_field_meta` refuses to write
    /// one, so this only fires on a row inserted behind the API, and the answer is
    /// to ignore it.
    pub fn apply_field_overlay(&mut self, meta: &FieldMeta) -> Vec<FieldMergeIssue> {
        if self.is_system() {
            return Vec::new();
        }
        let mut issues = Vec::new();
        let Some(idx) = self
            .fields
            .iter()
            .position(|f| f.base.name == meta.field_name)
        else {
            issues.push(self.field_issue(meta, "refers to a column that does not exist"));
            return issues;
        };

        // Overlay-owned, always applied: a label (empty means "none given") and
        // the field's own attributes.
        if !meta.label.is_empty() {
            self.fields[idx].base.label = meta.label.clone();
        }
        self.fields[idx].base.attributes = meta.attributes.clone();

        // A rich type, if it fits the column's SQL type.
        if let Some(type_name) = &meta.type_name {
            let column_sql = self.fields[idx].base.type_.sql_type().to_owned();
            match RichTypeRef::resolve(type_name) {
                Ok(rich) if rich.rich_type().sql_types().contains(&column_sql.as_str()) => {
                    self.fields[idx].base.type_ = TypeRef::Rich(rich);
                }
                Ok(_) => issues.push(self.field_issue(
                    meta,
                    &format!(
                        "is configured as rich type `{type_name}`, which does not apply to a \
                         `{column_sql}` column"
                    ),
                )),
                Err(_) => issues.push(self.field_issue(
                    meta,
                    &format!("names rich type `{type_name}`, which is not registered"),
                )),
            }
        }

        // A field kind. `File` applies freely; `Plain` changes nothing; `Key`
        // depends on whether the database enforces the reference.
        match &meta.kind {
            DataFieldKind::Plain => {}
            DataFieldKind::File { .. } => self.fields[idx].kind = meta.kind.clone(),
            DataFieldKind::Key { summary_field, .. } => {
                // Read the database's target first, so the assignment below does
                // not borrow the field it writes.
                let db_target = match &self.fields[idx].kind {
                    DataFieldKind::Key {
                        target_table,
                        target_field,
                        ..
                    } => Some((target_table.clone(), target_field.clone())),
                    _ => None,
                };
                self.fields[idx].kind = match db_target {
                    // The database enforces this foreign key, so it is the
                    // authority on the target: the overlay may add only the
                    // summary field, never repoint the reference.
                    Some((target_table, target_field)) => DataFieldKind::Key {
                        target_table,
                        target_field,
                        summary_field: summary_field.clone(),
                    },
                    // No foreign key behind the column: the overlay supplies the
                    // whole reference. This is the case the database *cannot*
                    // enforce — a key onto a field in another database (a future
                    // capability) — so the overlay is the only place its target
                    // can come from.
                    None => meta.kind.clone(),
                };
            }
        }
        issues
    }

    /// Build a [`FieldMergeIssue`] naming this table and the overlay's field.
    fn field_issue(&self, meta: &FieldMeta, what: &str) -> FieldMergeIssue {
        FieldMergeIssue {
            table: self.name.clone(),
            field: meta.field_name.clone(),
            message: format!("field `{}.{}` {what}", self.name, meta.field_name),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::FileStoreId;
    use sc_types::BasicType;

    /// A two-column `books` table: a `text` `title` and a `text` `cover`.
    fn books() -> Table {
        Table {
            id: TableId("books".into()),
            name: "books".into(),
            database: DbId::primary(),
            source: TableSource::Database,
            fields: vec![
                DataField::plain("title", TypeRef::Basic(BasicType::Text)),
                DataField::plain("cover", TypeRef::Basic(BasicType::Text)),
            ],
            primary_key: vec![],
            label: "books".into(),
            description: String::new(),
            access: AccessRules::default(),
            attributes: Attrs::new(),
            overlay: None,
        }
    }

    #[test]
    fn a_fitting_rich_type_and_attributes_apply() {
        let mut table = books();
        let mut meta = FieldMeta::new("books", "title")
            .label("Title")
            .rich_type("string");
        meta.attributes.insert("max_length".into(), 200.into());

        let issues = table.apply_field_overlay(&meta);
        assert!(issues.is_empty(), "{issues:?}");

        let field = table.field("title").unwrap();
        assert_eq!(field.base.label, "Title");
        assert_eq!(field.base.type_.name(), "string");
        assert_eq!(field.base.attributes.get("max_length"), Some(&200.into()));
    }

    #[test]
    fn a_rich_type_that_does_not_fit_the_column_is_reported_and_the_column_stays_basic() {
        // `text` configured as `Integer` — what a hand-edited database produces.
        let mut table = books();
        let meta = FieldMeta::new("books", "title").rich_type("integer");

        let issues = table.apply_field_overlay(&meta);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].message.contains("integer"), "{:?}", issues[0]);
        assert!(issues[0].message.contains("text"), "{:?}", issues[0]);
        // The field is still usable, still its basic type.
        assert_eq!(
            table.field("title").unwrap().base.type_,
            TypeRef::Basic(BasicType::Text)
        );
    }

    #[test]
    fn an_unregistered_rich_type_is_reported() {
        let mut table = books();
        let meta = FieldMeta::new("books", "title").rich_type("colour");
        let issues = table.apply_field_overlay(&meta);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].message.contains("colour"), "{:?}", issues[0]);
        assert!(issues[0].message.contains("not registered"), "{:?}", issues[0]);
    }

    #[test]
    fn a_file_kind_applies_to_any_column() {
        let mut table = books();
        let meta = FieldMeta::new("books", "cover").kind(DataFieldKind::File {
            store: FileStoreId("uploads".into()),
            folder: Some("covers".into()),
            mime_allow: vec![],
        });
        let issues = table.apply_field_overlay(&meta);
        assert!(issues.is_empty(), "{issues:?}");
        assert!(matches!(
            table.field("cover").unwrap().kind,
            DataFieldKind::File { .. }
        ));
    }

    #[test]
    fn a_key_overlay_on_a_plain_column_supplies_the_whole_reference() {
        // No foreign key behind the column: the overlay is the only place a
        // target can come from — the case the database cannot enforce (e.g. a key
        // into another database). It applies rather than being rejected.
        let mut table = books();
        let key = DataFieldKind::Key {
            target_table: TableId("authors".into()),
            target_field: FieldId("id".into()),
            summary_field: Some(FieldId("name".into())),
        };
        let meta = FieldMeta::new("books", "title").kind(key.clone());
        let issues = table.apply_field_overlay(&meta);
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(table.field("title").unwrap().kind, key);
    }

    #[test]
    fn a_key_overlay_adds_only_the_summary_field_atop_a_real_foreign_key() {
        // The column already is a key from introspection; the overlay keeps the
        // database's target and adds only the summary field.
        let mut table = books();
        table.fields[0].kind = DataFieldKind::Key {
            target_table: TableId("authors".into()),
            target_field: FieldId("id".into()),
            summary_field: None,
        };
        let meta = FieldMeta::new("books", "title").kind(DataFieldKind::Key {
            // A different target here must be ignored — the database is authority.
            target_table: TableId("wrong".into()),
            target_field: FieldId("wrong".into()),
            summary_field: Some(FieldId("name".into())),
        });

        let issues = table.apply_field_overlay(&meta);
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(
            table.field("title").unwrap().kind,
            DataFieldKind::Key {
                target_table: TableId("authors".into()),
                target_field: FieldId("id".into()),
                summary_field: Some(FieldId("name".into())),
            }
        );
    }

    #[test]
    fn an_overlay_for_a_missing_column_is_reported() {
        let mut table = books();
        let meta = FieldMeta::new("books", "ghost").rich_type("string");
        let issues = table.apply_field_overlay(&meta);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].message.contains("does not exist"), "{:?}", issues[0]);
    }

    #[test]
    fn a_system_table_ignores_a_field_overlay() {
        let mut table = books();
        table.name = "_sc_secret".into();
        let meta = FieldMeta::new("_sc_secret", "title").rich_type("string");
        assert!(table.apply_field_overlay(&meta).is_empty());
        assert_eq!(
            table.field("title").unwrap().base.type_,
            TypeRef::Basic(BasicType::Text)
        );
    }
}
