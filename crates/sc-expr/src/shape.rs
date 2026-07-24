//! How the catalog describes tables *to* this crate.
//!
//! `sc-expr` is a generic library below `sc-catalog` in the dependency graph, so
//! it cannot see `Table` or `DataFieldKind`. Instead the caller projects the
//! little a formula needs — which fields exist, which of them are keys and where
//! they point, what fields the user object carries — into a [`SchemaShape`].
//! Phase 4 builds one from the catalog; tests build them by hand.

use std::collections::{BTreeMap, BTreeSet};

/// The tables a formula may reach: the formula's own table plus every table
/// reachable through Ⱶ-join paths, keyed by table name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchemaShape {
    /// Per-table field shapes.
    pub tables: BTreeMap<String, TableShape>,
    /// The fields of the `user` object (the user table's columns as the formula
    /// sees them: `id`, `role`, and the admin-added extras). `None` means the
    /// caller does not know — validation then skips `user.x` membership checks
    /// rather than rejecting every one.
    pub user_fields: Option<BTreeSet<String>>,
}

/// One table's fields, keyed by field name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableShape {
    /// Field name → shape.
    pub fields: BTreeMap<String, FieldShape>,
    /// The table's primary-key column, when known. Aggregations (Phase 7) need
    /// it in two places: `length` counts a non-null column, and `maxBy`/`minBy`
    /// break selector ties on it so both evaluators pick the same child row.
    /// `None` means the caller did not declare it — an aggregation that needs
    /// it then fails to translate rather than guessing.
    pub primary_key: Option<String>,
}

/// What a formula needs to know about one field. A struct rather than a bare
/// `Option<KeyShape>` so later phases can add to it without touching every
/// constructor. (Phase 2's GUC casts ended up not needing a type here — the
/// user field→type map rides in `UserEnv::Guc`, since only that mode wants
/// types and only for the user object.)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldShape {
    /// `Some` when this is a Key field a Ⱶ-path may traverse.
    pub key: Option<KeyShape>,
}

/// Where a Key field points: the link a Ⱶ-path segment follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyShape {
    /// The referenced table.
    pub target_table: String,
    /// The referenced field on that table.
    pub target_field: String,
}

impl SchemaShape {
    /// An empty shape, to add tables to.
    pub fn new() -> SchemaShape {
        SchemaShape::default()
    }

    /// Add a table, replacing any previous shape under the same name.
    pub fn table(mut self, name: impl Into<String>, shape: TableShape) -> SchemaShape {
        self.tables.insert(name.into(), shape);
        self
    }

    /// Declare the user object's fields (enables `user.x` validation).
    pub fn user_fields<I, S>(mut self, fields: I) -> SchemaShape
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.user_fields = Some(fields.into_iter().map(Into::into).collect());
        self
    }

    /// The key fields pointing *at* `table`: every `(child_table, key_field)`
    /// pair in the shape whose Key targets `table`. This is what an inverse
    /// relation `childↃkey` (Phase 7) resolves against — derived from the child
    /// tables' [`KeyShape`]s rather than a parallel index that could drift, so
    /// the caller need only *include* the candidate child tables in the shape.
    /// The result is sorted (by child table, then key field) for a stable order.
    pub fn incoming(&self, table: &str) -> Vec<(&str, &str)> {
        let mut out = Vec::new();
        for (child, shape) in &self.tables {
            for (name, field) in &shape.fields {
                if let Some(key) = &field.key
                    && key.target_table == table
                {
                    out.push((child.as_str(), name.as_str()));
                }
            }
        }
        out
    }
}

impl TableShape {
    /// An empty table shape, to add fields to.
    pub fn new() -> TableShape {
        TableShape::default()
    }

    /// Add a plain (non-key) field.
    pub fn field(mut self, name: impl Into<String>) -> TableShape {
        self.fields.insert(name.into(), FieldShape::default());
        self
    }

    /// Declare the primary-key column (needed for aggregations over this table
    /// as a child — `length`'s count and `maxBy`/`minBy`'s tie-break).
    pub fn primary_key(mut self, name: impl Into<String>) -> TableShape {
        self.primary_key = Some(name.into());
        self
    }

    /// Add a Key field pointing at `target_table.target_field`.
    pub fn key_field(
        mut self,
        name: impl Into<String>,
        target_table: impl Into<String>,
        target_field: impl Into<String>,
    ) -> TableShape {
        self.fields.insert(
            name.into(),
            FieldShape {
                key: Some(KeyShape {
                    target_table: target_table.into(),
                    target_field: target_field.into(),
                }),
            },
        );
        self
    }
}
