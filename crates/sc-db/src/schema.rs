//! The physical schema as the database sees it, plus the schema-change requests
//! that mutate it.
//!
//! Two directions:
//!
//! - [`PhysicalTable`] and friends are the *result* of
//!   [`introspect`](crate::DatabaseDriver::introspect): the live shape of a
//!   table read from `information_schema`. Per the goals there is no table
//!   discovery step — as soon as a database is connected, every table is usable,
//!   so introspection is the source of truth and stored metadata is only an
//!   optional overlay (§5, §9).
//! - [`SchemaChange`] is the *input* to
//!   [`apply_schema`](crate::DatabaseDriver::apply_schema): a single create/drop
//!   table or add/drop column. Notably, creating a table does **not** invent an
//!   `id` column — key fields are declared explicitly like any other (§5).
//!
//! Both model composite primary keys and foreign keys to non-primary-key
//! columns, which the goals require, so keys are always `Vec<String>` and never
//! "the id column".

use serde::{Deserialize, Serialize};

/// A table exactly as it exists in the database, as returned by introspection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalTable {
    /// The table name (unqualified).
    pub name: String,
    /// The containing schema namespace (e.g. `public`), when the backend has
    /// one. `None` means the driver's default schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Columns in declaration order.
    pub columns: Vec<Column>,
    /// Names of the columns forming the primary key, in key order. Empty when
    /// the table has no primary key; more than one entry is a composite key.
    #[serde(default)]
    pub primary_key: Vec<String>,
    /// Foreign-key constraints declared on this table.
    #[serde(default)]
    pub foreign_keys: Vec<ForeignKey>,
}

/// One column of a [`PhysicalTable`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    /// The column name.
    pub name: String,
    /// The backend's own type name (e.g. `text`, `int8`, `timestamptz`). Mapping
    /// this to a rich/basic Saltcorn type is the type layer's job (§6), not the
    /// driver's.
    pub sql_type: String,
    /// Whether the column accepts `NULL`.
    pub nullable: bool,
    /// The column default rendered as backend SQL, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

/// A foreign-key constraint. Local `columns` reference `referenced_columns` of
/// `referenced_table`, positionally. The referenced columns need not be a
/// primary key (the goals require FKs to non-PK columns).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignKey {
    /// Local columns constrained by this key, in order.
    pub columns: Vec<String>,
    /// The referenced table's (unqualified) name.
    pub referenced_table: String,
    /// The referenced columns, positionally matching `columns`.
    pub referenced_columns: Vec<String>,
}

/// A single schema mutation applied by
/// [`apply_schema`](crate::DatabaseDriver::apply_schema).
///
/// Kept deliberately minimal for the MVP: create/drop a table and add/drop a
/// column. Indexes, constraints, and alterations arrive as the design demands
/// them. Creating a table adds **no** implicit primary-key column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "op")]
pub enum SchemaChange {
    /// Create a new table with the given columns and optional primary key.
    CreateTable {
        /// The new table's name.
        name: String,
        /// Column definitions, in the order they should be declared.
        columns: Vec<ColumnDef>,
        /// Names of columns forming the primary key (empty for none; more than
        /// one for a composite key). Each must appear in `columns`.
        #[serde(default)]
        primary_key: Vec<String>,
    },
    /// Drop an existing table.
    DropTable {
        /// The table to drop.
        name: String,
        /// Suppress an error when the table does not exist.
        #[serde(default)]
        if_exists: bool,
    },
    /// Add a column to an existing table.
    AddColumn {
        /// The table to alter.
        table: String,
        /// The column to add.
        column: ColumnDef,
    },
    /// Drop a column from an existing table.
    DropColumn {
        /// The table to alter.
        table: String,
        /// The column to drop.
        column: String,
        /// Suppress an error when the column does not exist.
        #[serde(default)]
        if_exists: bool,
    },
}

/// The definition of a column to create, used inside a [`SchemaChange`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnDef {
    /// The column name.
    pub name: String,
    /// The backend SQL type for the column (e.g. `text`, `uuid`, `int8`).
    pub sql_type: String,
    /// Whether the column accepts `NULL`.
    pub nullable: bool,
    /// A column default rendered as backend SQL, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Whether a `UNIQUE` constraint should be attached to the column.
    #[serde(default)]
    pub unique: bool,
}

impl ColumnDef {
    /// A nullable column of the given name and SQL type, with no default and no
    /// unique constraint — the base to tweak fields on.
    pub fn new(name: impl Into<String>, sql_type: impl Into<String>) -> Self {
        ColumnDef {
            name: name.into(),
            sql_type: sql_type.into(),
            nullable: true,
            default: None,
            unique: false,
        }
    }

    /// Mark this column `NOT NULL`.
    pub fn not_null(mut self) -> Self {
        self.nullable = false;
        self
    }

    /// Attach a `UNIQUE` constraint to this column.
    pub fn unique(mut self) -> Self {
        self.unique = true;
        self
    }

    /// Set the column default (backend SQL).
    pub fn default(mut self, sql: impl Into<String>) -> Self {
        self.default = Some(sql.into());
        self
    }
}
