//! The `_fd_views` and `_fd_pages` tables: their schema and one-time bootstrap
//! (TODO "Saltcorn UI" §1).
//!
//! Both obey §9's required columns (UUID `id`, `name`, `description`,
//! `attributes`). The one addition to v1's shape is `application`, and the
//! uniqueness moves with it: a name is unique on (`application`, `name`), not on
//! its own, so two applications may each hold a `List Books`.
//!
//! `application` is **not** a foreign key onto `_fd_applications`, for the
//! reason `_fd_workflow_versions.workflow` is not one: the schema layer renders
//! no `ON DELETE` action, so a key would make deleting an application impossible
//! rather than tidy. Deleting an application deletes its views and pages, and
//! [`delete_application_views_and_pages`](crate::delete_application_views_and_pages)
//! is what does it. `min_role` is not a key onto `_fd_roles` either: it is
//! checked on save, where the refusal can name the role.

use sc_catalog::{Catalog, ConstraintKind, DataField, SchemaStep, Table, TableConstraint};
use sc_db::SchemaChange;
use sc_error::Result;
use sc_types::{BasicType, TypeRef};

/// Name of the views table in the primary database.
pub const VIEWS_TABLE: &str = "_fd_views";
/// Name of the pages table in the primary database.
pub const PAGES_TABLE: &str = "_fd_pages";

/// The UUID primary-key column (§9).
pub const COL_ID: &str = "id";
/// The owning application's id.
pub const COL_APPLICATION: &str = "application";
/// The name, unique per application (§9).
pub const COL_NAME: &str = "name";
/// The description (§9). Nullable; `NULL` reads back as the empty string.
pub const COL_DESCRIPTION: &str = "description";
/// A view's pattern name.
pub const COL_VIEWPATTERN: &str = "viewpattern";
/// A view's table. Nullable, for a tableless pattern.
pub const COL_TABLE_NAME: &str = "table_name";
/// A view's v1-shaped configuration — JSON, always an object.
pub const COL_CONFIGURATION: &str = "configuration";
/// The minimum role, `1..=100`.
pub const COL_MIN_ROLE: &str = "min_role";
/// A view's v1 slug — JSON, nullable.
pub const COL_SLUG: &str = "slug";
/// A page's title.
pub const COL_TITLE: &str = "title";
/// A page's v1-shaped layout — JSON.
pub const COL_LAYOUT: &str = "layout";
/// The sparse per-row values column (§9) — JSON, always an object.
pub const COL_ATTRIBUTES: &str = "attributes";

fn text() -> TypeRef {
    TypeRef::Basic(BasicType::Text)
}
fn json() -> TypeRef {
    TypeRef::Basic(BasicType::Json)
}
fn uuid() -> TypeRef {
    TypeRef::Basic(BasicType::Uuid)
}
fn int() -> TypeRef {
    TypeRef::Basic(BasicType::Int)
}

/// The fields of `_fd_views`, in declaration order.
pub(crate) fn views_fields() -> Vec<DataField> {
    vec![
        DataField::plain(COL_ID, uuid()).required().primary_key(),
        DataField::plain(COL_APPLICATION, uuid()).required(),
        DataField::plain(COL_NAME, text()).required(),
        DataField::plain(COL_DESCRIPTION, text()),
        DataField::plain(COL_VIEWPATTERN, text()).required(),
        DataField::plain(COL_TABLE_NAME, text()),
        DataField::plain(COL_CONFIGURATION, json()).required(),
        DataField::plain(COL_MIN_ROLE, int()).required(),
        DataField::plain(COL_SLUG, json()),
        DataField::plain(COL_ATTRIBUTES, json()).required(),
    ]
}

/// The fields of `_fd_pages`, in declaration order.
pub(crate) fn pages_fields() -> Vec<DataField> {
    vec![
        DataField::plain(COL_ID, uuid()).required().primary_key(),
        DataField::plain(COL_APPLICATION, uuid()).required(),
        DataField::plain(COL_NAME, text()).required(),
        DataField::plain(COL_TITLE, text()).required(),
        DataField::plain(COL_DESCRIPTION, text()),
        DataField::plain(COL_LAYOUT, json()).required(),
        DataField::plain(COL_MIN_ROLE, int()).required(),
        DataField::plain(COL_ATTRIBUTES, json()).required(),
    ]
}

/// The jointly-unique key (`application`, `name`). Save checks it first so the
/// refusal names the clash; the database is still the authority, because two
/// admins saving at once cannot see each other's transaction.
pub(crate) fn name_key() -> ConstraintKind {
    ConstraintKind::Unique {
        fields: vec![COL_APPLICATION.to_owned(), COL_NAME.to_owned()],
    }
}

/// Ensure `_fd_views` and `_fd_pages` exist, each with its (`application`,
/// `name`) key.
///
/// Idempotent and additively reconciled, like every other bootstrap: an existing
/// table gains a declared column it lacks, and the key is added only when
/// introspection says it is not already there.
pub async fn bootstrap(catalog: &Catalog) -> Result<()> {
    bootstrap_one(catalog, VIEWS_TABLE, &views_fields()).await?;
    bootstrap_one(catalog, PAGES_TABLE, &pages_fields()).await?;
    Ok(())
}

async fn bootstrap_one(catalog: &Catalog, name: &str, fields: &[DataField]) -> Result<Table> {
    let table = catalog.bootstrap_table(name, fields).await?;
    let key = name_key();
    if table.constraints.iter().any(|c| c.kind == key) {
        return Ok(table);
    }
    let constraint = TableConstraint::derived_name(name, &key, "");
    catalog
        .apply_schema_batch(&[SchemaStep::Change(SchemaChange::AddUniqueConstraint {
            table: name.to_owned(),
            name: constraint,
            columns: vec![COL_APPLICATION.to_owned(), COL_NAME.to_owned()],
        })])
        .await?;
    catalog.reload().await?;
    catalog.require(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_tables_have_the_section_9_columns_and_an_application() {
        for fields in [views_fields(), pages_fields()] {
            let by_name = |n: &str| fields.iter().find(|f| f.base.name == n).unwrap();
            assert!(by_name(COL_ID).primary_key && by_name(COL_ID).required);
            assert!(by_name(COL_NAME).required);
            assert!(!by_name(COL_DESCRIPTION).required);
            assert_eq!(by_name(COL_ATTRIBUTES).base.type_, json());
            assert_eq!(by_name(COL_APPLICATION).base.type_, uuid());
            // Unique per application, not globally.
            assert!(!by_name(COL_NAME).unique);
        }
        assert_eq!(
            name_key(),
            ConstraintKind::Unique {
                fields: vec![COL_APPLICATION.to_owned(), COL_NAME.to_owned()]
            }
        );
    }

    #[test]
    fn a_view_table_and_slug_may_be_absent() {
        let fields = views_fields();
        let by_name = |n: &str| fields.iter().find(|f| f.base.name == n).unwrap();
        assert!(!by_name(COL_TABLE_NAME).required);
        assert!(!by_name(COL_SLUG).required);
        assert!(by_name(COL_CONFIGURATION).required);
    }
}
