//! Catalog, Table, Field, TableProvider trait, cache (layer 4).
//!
//! This crate is the hub of the data layer (technical design §8). It holds the
//! connected database driver and an in-memory cache of its [`Table`]s, each a set
//! of [`DataField`]s built from live introspection — the MVP stores **no**
//! metadata beyond `information_schema`, so a freshly connected database is
//! immediately usable with zero setup. A [`Catalog`] can also create tables and
//! fields, keeping its cache in step, and hand out a [`TableProvider`] to run
//! queries against a table.
//!
//! The `_sc_tables` overlay ([`TableMeta`]) is the first stored metadata that
//! *adds* to introspection rather than replacing it: a table with no overlay row
//! is exactly as usable as it was before the overlay existed, which is what
//! keeps the zero-setup promise true (§9).
//!
//! Deferred (see the design): `FormField` and calculated fields, rich types, the
//! `_sc_fields` overlay, virtual and materialised providers, and cross-process
//! cache invalidation over a bus.

mod calc;
mod caller;
mod catalog;
mod events;
mod field;
mod field_meta;
mod file_stores;
mod observer;
mod prefetch;
mod projection;
mod provider;
mod rls;
mod table;
mod table_meta;

pub use caller::CallerContext;
pub use catalog::{Catalog, SchemaStep};
pub use events::{TableEvents, TableWrite, WriteOp};
pub use field::{Attrs, BaseField, DataField, DataFieldKind, DbId, FieldId, FileStoreId, TableId};
pub use field_meta::{
    FIELD_META_TABLE, FieldMeta, FieldMetaId, KIND_CALC, KIND_FILE, KIND_KEY, KIND_PLAIN,
    bootstrap_field_meta, delete_field_meta, file_kind_config_spec, key_kind_config_spec,
    list_field_meta, list_field_meta_for_table, load_field_meta, load_field_meta_by_field,
    save_field_meta, save_field_meta_row,
};
pub use file_stores::{
    FILE_STORES_TABLE, FileStoreConnections, QUERY_FILE_STORES, bootstrap_file_stores,
    check_file_store_saveable, choosable_file_stores, connect_all_file_stores,
    connect_file_store_def, delete_file_store, file_store_field_references, list_file_stores,
    load_file_store, load_file_store_by_name, resolve_options, save_file_store,
};
pub use observer::{SchemaChanged, SchemaObserver};
pub use prefetch::prefetch_bindings;
pub use projection::SchemaProjection;
pub use provider::{DriverTableProvider, TableProvider};
pub use rls::{ROLE_GUC, disable_rls, disable_rls_sql, enable_rls, enable_rls_sql, run_in_context};
pub use table::{AccessRules, FieldMergeIssue, Table, TableSource};
pub use table_meta::{
    ATTR_OWNERSHIP_FORMULA, ATTR_RLS_ENABLED, TABLE_META_TABLE, TableMeta, TableMetaId,
    bootstrap_table_meta, delete_table_meta, list_table_meta, load_table_meta,
    load_table_meta_by_name, orphan_table_meta, save_table_meta, save_table_meta_row,
};

#[cfg(test)]
mod tests {
    use super::*;
    use sc_db::{Column, ForeignKey, PhysicalTable};
    use sc_types::{BasicType, TypeRef};

    #[test]
    fn data_field_builder_and_column_def() {
        let f = DataField::plain("email", TypeRef::Basic(BasicType::Text))
            .label("Email address")
            .required()
            .unique();
        assert_eq!(f.base.label, "Email address");
        assert!(f.required && f.unique && !f.primary_key);

        let col = f.to_column_def();
        assert_eq!(col.name, "email");
        assert_eq!(col.sql_type, "text");
        assert!(!col.nullable); // required → NOT NULL
        assert!(col.unique);
    }

    #[test]
    fn primary_key_field_has_no_default_and_maps_type() {
        let id = DataField::plain("id", TypeRef::Basic(BasicType::Uuid))
            .required()
            .primary_key();
        let col = id.to_column_def();
        assert_eq!(col.sql_type, "uuid");
        assert!(!col.nullable);
        assert!(col.default.is_none());
        assert!(id.primary_key);
    }

    #[test]
    fn base_field_label_defaults_to_name() {
        let b = BaseField::new("count", TypeRef::Basic(BasicType::Int));
        assert_eq!(b.name, "count");
        assert_eq!(b.label, "count");
        assert!(b.attributes.is_empty());
    }

    /// A physical table with a primary key and a single-column foreign key, used
    /// to check the introspection → catalog mapping.
    fn physical_with_fk() -> PhysicalTable {
        PhysicalTable {
            name: "book".into(),
            schema: Some("public".into()),
            columns: vec![
                Column {
                    name: "id".into(),
                    sql_type: "int8".into(),
                    nullable: false,
                    default: None,
                },
                Column {
                    name: "title".into(),
                    sql_type: "text".into(),
                    nullable: false,
                    default: None,
                },
                Column {
                    name: "author".into(),
                    sql_type: "int8".into(),
                    nullable: true,
                    default: None,
                },
            ],
            primary_key: vec!["id".into()],
            foreign_keys: vec![ForeignKey {
                columns: vec!["author".into()],
                referenced_table: "person".into(),
                referenced_columns: vec!["id".into()],
            }],
        }
    }

    #[test]
    fn table_from_physical_maps_pk_types_and_nullability() {
        let table = Table::from_physical(DbId::primary(), &physical_with_fk());
        assert_eq!(table.id, TableId("book".into()));
        assert_eq!(table.name, "book");
        assert_eq!(table.database, DbId::primary());
        assert_eq!(table.source, TableSource::Database);
        assert_eq!(table.primary_key, vec!["id".to_string()]);
        assert_eq!(table.access, AccessRules::default());

        let id = table.field("id").expect("id field");
        assert!(id.primary_key);
        assert!(id.required); // NOT NULL
        assert_eq!(id.base.type_, TypeRef::Basic(BasicType::Int));

        let title = table.field("title").expect("title field");
        assert!(!title.primary_key);
        assert_eq!(title.kind, DataFieldKind::Plain);
    }

    #[test]
    fn table_from_physical_derives_key_kind_from_foreign_key() {
        let table = Table::from_physical(DbId::primary(), &physical_with_fk());
        let author = table.field("author").expect("author field");
        assert!(!author.required); // nullable
        assert_eq!(
            author.kind,
            DataFieldKind::Key {
                target_table: TableId("person".into()),
                target_field: FieldId("id".into()),
                summary_field: None,
            }
        );
    }

    #[test]
    fn access_rules_default_is_admin_only() {
        let rules = AccessRules::default();
        assert_eq!(rules.min_role_read, 1);
        assert_eq!(rules.min_role_write, 1);
    }

    #[test]
    fn is_system_detects_sc_prefixed_tables() {
        let mut physical = physical_with_fk();
        assert!(!Table::from_physical(DbId::primary(), &physical).is_system());
        physical.name = "_sc_config".into();
        assert!(Table::from_physical(DbId::primary(), &physical).is_system());
    }

    #[test]
    fn apply_overlay_adds_what_the_database_cannot_know_and_nothing_else() {
        let physical = physical_with_fk();
        let plain = Table::from_physical(DbId::primary(), &physical);
        let mut table = plain.clone();

        let meta = TableMeta::new("book")
            .label("Books")
            .description("The library catalogue")
            .access(80, 40);
        table.apply_overlay(&meta);

        // Added: exactly the four things the overlay is the authority on.
        assert_eq!(table.label, "Books");
        assert_eq!(table.description, "The library catalogue");
        assert_eq!(table.access.min_role_read, 80);
        assert_eq!(table.access.min_role_write, 40);
        assert_eq!(table.overlay, Some(meta.id));

        // Untouched: everything the database is the authority on. The merge has
        // no conflict semantics because the two sets do not intersect, and this
        // is what asserts they still don't.
        assert_eq!(table.id, plain.id);
        assert_eq!(table.name, plain.name);
        assert_eq!(table.fields, plain.fields);
        assert_eq!(table.primary_key, plain.primary_key);
        assert_eq!(table.source, plain.source);
        assert_eq!(table.database, plain.database);
    }

    #[test]
    fn an_empty_label_means_none_given_not_a_blank_label() {
        let mut table = Table::from_physical(DbId::primary(), &physical_with_fk());
        table.apply_overlay(&TableMeta::new("book").access(100, 100));
        assert_eq!(table.label, "book", "the table's own name stays the label");
        assert_eq!(table.description, "");
        assert_eq!(table.access.min_role_read, 100);
    }

    #[test]
    fn a_system_table_never_takes_an_overlay() {
        // `save_table_meta` refuses to write such a row, so this only fires on
        // one inserted behind the API's back — a hand-edited database, a
        // restored dump. `_sc_*` tables are hidden from users (§9) and their
        // access is not configurable, so the row is ignored rather than obeyed.
        let mut physical = physical_with_fk();
        physical.name = "_sc_config".into();
        let mut table = Table::from_physical(DbId::primary(), &physical);
        table.apply_overlay(
            &TableMeta::new("_sc_config")
                .label("Config")
                .access(100, 100),
        );

        assert_eq!(table.access, AccessRules::default());
        assert_eq!(table.label, "_sc_config");
        assert_eq!(table.overlay, None);
    }

    #[test]
    fn attrs_and_base_field_are_re_exported_from_sc_types_not_redefined() {
        // Both moved down to `sc-types` (layer 3) so `FormField` — which carries
        // a `BaseField` and describes `Attrs` entries — can live beside them.
        // This crate re-exports both, so the old paths still resolve and every
        // existing call site is untouched. These assignments compile only if the
        // names are the *same* types, not look-alikes.
        let attrs: Attrs = sc_types::Attrs::new();
        let _: sc_types::Attrs = attrs;
        let base: BaseField = sc_types::BaseField::new("x", TypeRef::Basic(BasicType::Text));
        let _: sc_types::BaseField = base;

        // `DataField` stays here — its `Key`/`File` kinds reference catalog ids —
        // and still builds on the very same `BaseField`.
        let mut field = DataField::plain("x", TypeRef::Basic(BasicType::Text));
        field.base.attributes.insert("max".to_owned(), 10.into());
        let _: &sc_types::BaseField = &field.base;
        let _: &sc_types::Attrs = &field.base.attributes;
    }
}
