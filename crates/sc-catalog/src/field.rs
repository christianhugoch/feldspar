//! Fields and the identifiers that tie them together.
//!
//! The technical design (§6.2) separates a **database** field ([`DataField`] —
//! a column in a table) from a **form** field, and shares their common shape in
//! [`BaseField`]. The MVP models `DataField` fully — including the `Key` and
//! `File` kinds, even though the admin UI only exercises `Plain` — so the data
//! model is right from the start (`FormField` and calculated fields are
//! post-MVP; see the crate root).
//!
//! Design note (a deliberate, documented deviation): §6.2 sketches these types
//! under `sc-types`, but the `Key` kind references a table and a field by id, and
//! ids are a catalog concept. Keeping the field types here — one layer up, beside
//! [`Table`](crate::Table) — avoids `sc-types` depending on catalog identifiers
//! while matching the Phase 4 TODO grouping.

use sc_db::{Column, ColumnDef};
use sc_types::TypeRef;

/// Type-specific attributes carried by a field or table, always a JSON object
/// (technical design §6, §9: "a sparse value goes into `attributes`").
pub type Attrs = serde_json::Map<String, serde_json::Value>;

/// Identifies a table within the catalog. For the MVP — which stores no metadata
/// beyond `information_schema` — a table's stable identity is simply its name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TableId(pub String);

/// Identifies a field within a table. As with [`TableId`], the MVP identity is
/// the field (column) name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FieldId(pub String);

/// Identifies a connected database. The MVP has a single **primary** database
/// (see [`DbId::primary`]); the newtype exists so multi-database support slots in
/// later without changing field signatures.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DbId(pub String);

/// Identifies a connected file store, referenced by [`DataFieldKind::File`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileStoreId(pub String);

impl DbId {
    /// The single primary database of the MVP: the one that additionally hosts
    /// the `users` and (post-MVP) `_sc_*` metadata tables.
    pub fn primary() -> DbId {
        DbId("primary".to_owned())
    }
}

/// Properties shared by every field: its identifier name, human label, type, and
/// type-specific attributes (technical design §6.2).
#[derive(Debug, Clone, PartialEq)]
pub struct BaseField {
    /// A valid identifier in SQL and every guest language.
    pub name: String,
    /// Human-facing string; defaults to the name.
    pub label: String,
    /// The field's type — basic in the MVP (rich types post-MVP).
    pub type_: TypeRef,
    /// Type-specific attributes (JSON object).
    pub attributes: Attrs,
}

impl BaseField {
    /// A base field with the given name and type; the label defaults to the name
    /// and there are no attributes.
    pub fn new(name: impl Into<String>, type_: TypeRef) -> BaseField {
        let name = name.into();
        BaseField {
            label: name.clone(),
            name,
            type_,
            attributes: Attrs::new(),
        }
    }
}

/// A column in a database table (technical design §6.2).
#[derive(Debug, Clone, PartialEq)]
pub struct DataField {
    /// The shared field properties.
    pub base: BaseField,
    /// Whether the column is `NOT NULL`.
    pub required: bool,
    /// Whether the column carries a `UNIQUE` constraint.
    pub unique: bool,
    /// Whether the column is part of the table's primary key (possibly
    /// composite).
    pub primary_key: bool,
    /// What the field references, if anything.
    pub kind: DataFieldKind,
}

/// What a [`DataField`] holds beyond a plain scalar (technical design §6.2).
#[derive(Debug, Clone, PartialEq)]
pub enum DataFieldKind {
    /// An ordinary scalar column.
    Plain,
    /// A foreign key: holds the value of a referenced field (not necessarily the
    /// target's primary key), with an optional summary field used as the default
    /// label when selecting.
    Key {
        /// The referenced table.
        target_table: TableId,
        /// The referenced field.
        target_field: FieldId,
        /// A field of the target used as a human label, if chosen.
        summary_field: Option<FieldId>,
    },
    /// A file reference: a relative path within a named file store, optionally
    /// restricted to a folder and/or MIME types.
    File {
        /// The file store the path is relative to.
        store: FileStoreId,
        /// A folder within the store the file must live under, if restricted.
        folder: Option<String>,
        /// Allowed MIME types (empty = any).
        mime_allow: Vec<String>,
    },
}

impl DataField {
    /// A plain, nullable, non-unique, non-key column of the given name and type —
    /// the base to tweak with the builder methods below.
    pub fn plain(name: impl Into<String>, type_: TypeRef) -> DataField {
        DataField {
            base: BaseField::new(name, type_),
            required: false,
            unique: false,
            primary_key: false,
            kind: DataFieldKind::Plain,
        }
    }

    /// Set the human label.
    pub fn label(mut self, label: impl Into<String>) -> DataField {
        self.base.label = label.into();
        self
    }

    /// Mark the column `NOT NULL`.
    pub fn required(mut self) -> DataField {
        self.required = true;
        self
    }

    /// Attach a `UNIQUE` constraint.
    pub fn unique(mut self) -> DataField {
        self.unique = true;
        self
    }

    /// Make the column part of the primary key.
    pub fn primary_key(mut self) -> DataField {
        self.primary_key = true;
        self
    }

    /// The column definition this field emits when the table or column is
    /// created via the catalog. Primary-key membership is applied at the table
    /// level (in the `CREATE TABLE`), not here.
    pub fn to_column_def(&self) -> ColumnDef {
        let mut col = ColumnDef::new(self.base.name.clone(), self.base.type_.sql_type());
        if self.required {
            col = col.not_null();
        }
        if self.unique {
            col = col.unique();
        }
        col
    }

    /// Build a data field from an introspected [`Column`]. Nullability comes from
    /// the column; uniqueness is not reported by MVP introspection and defaults
    /// to `false`. The caller supplies primary-key membership and the kind, both
    /// of which are table-level facts.
    pub fn from_column(col: &Column, primary_key: bool, kind: DataFieldKind) -> DataField {
        DataField {
            base: BaseField {
                name: col.name.clone(),
                label: col.name.clone(),
                type_: TypeRef::from_sql_type(&col.sql_type),
                attributes: Attrs::new(),
            },
            required: !col.nullable,
            unique: false,
            primary_key,
            kind,
        }
    }
}
