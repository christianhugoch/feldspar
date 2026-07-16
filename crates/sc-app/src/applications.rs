//! The `_sc_applications` table: its schema and one-time bootstrap (design §9,
//! §13.2).
//!
//! This is the **one stored-metadata table the MVP needs**, and the reason is
//! the distinction §9 draws between an overlay and a definition. `_sc_tables` /
//! `_sc_fields` are overlays: introspection already yields the tables and
//! fields, so a row only *adds* access rules and attributes, and a legacy
//! database needs none. An application has no such underlying reality — there is
//! nothing to introspect it from — so its row **is** the application.
//!
//! Being an `_sc_*` table it is a system table, hidden from users
//! ([`Table::is_system`](sc_catalog::Table::is_system)), and it obeys the §9
//! required columns: UUID [`id`](COL_ID), [`name`](COL_NAME),
//! [`description`](COL_DESCRIPTION), [`attributes`](COL_ATTRIBUTES).
//!
//! The remaining columns follow the §9 column-vs-attributes rule — *a value
//! present for many rows gets its own column; a sparse value goes into
//! `attributes`* — and every app has all of them, so each gets a column:
//! subdomain, framework, extra frameworks, the table/store subsets, the API and
//! static-dir lists, and the CSP. Only [`subdomain`](COL_SUBDOMAIN) is a scalar
//! the database can index and constrain; the rest are structured lists and maps,
//! stored as JSON. That is deliberate — they are read as a whole app or not at
//! all, and no query filters on them.
//!
//! Like [`sc_auth::bootstrap`], this goes through the [`Catalog`] rather than
//! issuing DDL directly, so it is driver-agnostic, and it invents no
//! database-side defaults: the UUID is generated in Rust when the app is saved.

use sc_catalog::{Catalog, DataField, Table};
use sc_error::Result;
use sc_types::{BasicType, TypeRef};

/// Name of the applications table in the primary database.
pub const APPLICATIONS_TABLE: &str = "_sc_applications";

/// The UUID primary-key column (§9).
pub const COL_ID: &str = "id";
/// The human-readable name column (§9).
pub const COL_NAME: &str = "name";
/// The human-readable description column (§9). Nullable: a description is
/// genuinely optional, and a `NULL` reads back as the empty string.
pub const COL_DESCRIPTION: &str = "description";
/// The sparse per-app values column (§9) — JSON, always an object.
pub const COL_ATTRIBUTES: &str = "attributes";

/// The subdomain the app is served on: the unique routing key (§13.2).
pub const COL_SUBDOMAIN: &str = "subdomain";
/// The primary framework reference (JSON `{name, config}`).
pub const COL_FRAMEWORK: &str = "framework";
/// Additional framework references (JSON array).
pub const COL_EXTRA_FRAMEWORKS: &str = "extra_frameworks";
/// The declared table subset (JSON array of table names).
pub const COL_TABLES: &str = "tables";
/// The declared file-store subset (JSON array of store names).
pub const COL_FILE_STORES: &str = "file_stores";
/// The enabled API providers (JSON array of `{provider, mount}`).
pub const COL_APIS: &str = "apis";
/// The statically-served directories (JSON array of `{mount, store, path}`).
pub const COL_STATIC_DIRS: &str = "static_dirs";
/// The content-security policy (JSON object of directive → source list).
pub const COL_CSP: &str = "csp";

/// The fields of the `_sc_applications` table, in declaration order.
///
/// `subdomain` carries the `UNIQUE` constraint: routing dispatches on it, so two
/// apps claiming the same one is not a state the system can serve. Enforcing it
/// here means the database rejects it even when two admins save concurrently —
/// the in-memory mount registry cannot see the other transaction, so it is not
/// the place to be authoritative.
fn applications_fields() -> Vec<DataField> {
    let text = || TypeRef::Basic(BasicType::Text);
    let json = || TypeRef::Basic(BasicType::Json);
    let uuid = || TypeRef::Basic(BasicType::Uuid);
    vec![
        DataField::plain(COL_ID, uuid()).required().primary_key(),
        DataField::plain(COL_NAME, text()).required(),
        DataField::plain(COL_DESCRIPTION, text()),
        DataField::plain(COL_SUBDOMAIN, text()).required().unique(),
        DataField::plain(COL_FRAMEWORK, json()).required(),
        DataField::plain(COL_EXTRA_FRAMEWORKS, json()).required(),
        DataField::plain(COL_TABLES, json()).required(),
        DataField::plain(COL_FILE_STORES, json()).required(),
        DataField::plain(COL_APIS, json()).required(),
        DataField::plain(COL_STATIC_DIRS, json()).required(),
        DataField::plain(COL_CSP, json()).required(),
        DataField::plain(COL_ATTRIBUTES, json()).required(),
    ]
}

/// Ensure the `_sc_applications` table exists, creating it if absent, and return
/// it.
///
/// Idempotent: an existing table is returned unchanged (the MVP performs no
/// schema reconciliation). Call this once at startup after the [`Catalog`] is
/// initialised — including against a database that has never seen Saltcorn,
/// which is exactly the case the table's absence covers: a legacy database
/// bootstraps into one holding applications without any migration step.
pub async fn bootstrap(catalog: &Catalog) -> Result<Table> {
    if let Some(existing) = catalog.get(APPLICATIONS_TABLE)? {
        return Ok(existing);
    }
    catalog
        .create_table(APPLICATIONS_TABLE, &applications_fields())
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_has_the_section_9_required_columns() {
        let fields = applications_fields();
        let by_name = |n: &str| fields.iter().find(|f| f.base.name == n).unwrap();

        // §9: every system metadata table MUST have id (UUID), name,
        // description, attributes (JSON object).
        let id = by_name(COL_ID);
        assert!(id.primary_key && id.required);
        assert_eq!(id.base.type_, TypeRef::Basic(BasicType::Uuid));
        assert!(by_name(COL_NAME).required);
        assert_eq!(
            by_name(COL_ATTRIBUTES).base.type_,
            TypeRef::Basic(BasicType::Json)
        );
        // A description is optional; NULL reads back as "".
        assert!(!by_name(COL_DESCRIPTION).required);
    }

    #[test]
    fn subdomain_is_the_unique_routing_key() {
        let fields = applications_fields();
        let subdomain = fields
            .iter()
            .find(|f| f.base.name == COL_SUBDOMAIN)
            .unwrap();
        assert!(subdomain.required && subdomain.unique);
        assert_eq!(subdomain.base.type_, TypeRef::Basic(BasicType::Text));
    }

    #[test]
    fn the_structured_columns_are_json_and_required() {
        let fields = applications_fields();
        for name in [
            COL_FRAMEWORK,
            COL_EXTRA_FRAMEWORKS,
            COL_TABLES,
            COL_FILE_STORES,
            COL_APIS,
            COL_STATIC_DIRS,
            COL_CSP,
            COL_ATTRIBUTES,
        ] {
            let f = fields.iter().find(|f| f.base.name == name).unwrap();
            assert_eq!(f.base.type_, TypeRef::Basic(BasicType::Json), "{name}");
            // Every one is always written (an empty list is `[]`, not NULL), so
            // a reader never has to distinguish "absent" from "empty".
            assert!(f.required, "{name} should be NOT NULL");
        }
    }

    #[test]
    fn the_table_is_a_hidden_system_table() {
        assert!(APPLICATIONS_TABLE.starts_with("_sc_"));
    }
}
