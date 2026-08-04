//! Table fixtures shared by this module's unit tests.
//!
//! Building a [`Table`] by hand is a dozen fields of noise that says nothing
//! about the test doing it. These build the smallest table the naming and
//! schema-building code can be asked about — no catalog, no database, no
//! overlay — so a test reads as the schema it is about.

use sc_catalog::{
    AccessRules, DataField, DataFieldKind, DbId, FieldId, FileStoreId, Table, TableId, TableSource,
};
use sc_types::{Attrs, BasicType, TypeRef};

/// A table with `fields`, whose primary key is `id` when it has such a column.
pub fn table_of(name: &str, fields: Vec<DataField>) -> Table {
    let primary_key = fields
        .iter()
        .filter(|f| f.primary_key)
        .map(|f| f.base.name.clone())
        .collect();
    Table {
        id: TableId(name.to_owned()),
        name: name.to_owned(),
        database: DbId::primary(),
        source: TableSource::Database,
        primary_key,
        fields,
        label: name.to_owned(),
        description: String::new(),
        access: AccessRules::default(),
        attributes: Attrs::new(),
        overlay: None,
        ownership: None,
        ownership_error: None,
        rls_enabled: false,
    }
}

/// A nullable text column.
pub fn plain_field(name: &str) -> DataField {
    DataField::plain(name, TypeRef::Basic(BasicType::Text))
}

/// A column of an explicit basic type.
pub fn typed_field(name: &str, ty: BasicType) -> DataField {
    DataField::plain(name, TypeRef::Basic(ty))
}

/// A required integer primary key.
pub fn id_field() -> DataField {
    DataField::plain("id", TypeRef::Basic(BasicType::Int))
        .required()
        .primary_key()
}

/// A `Key` column pointing at `target_table`.`target_field`.
pub fn key_field(name: &str, target_table: &str, target_field: &str) -> DataField {
    let mut field = DataField::plain(name, TypeRef::Basic(BasicType::Int));
    field.kind = DataFieldKind::Key {
        target_table: TableId(target_table.to_owned()),
        target_field: FieldId(target_field.to_owned()),
        summary_field: None,
    };
    field
}

/// A `File` column in the named store.
pub fn file_field(name: &str, store: &str) -> DataField {
    let mut field = DataField::plain(name, TypeRef::Basic(BasicType::Text));
    field.kind = DataFieldKind::File {
        store: FileStoreId(store.to_owned()),
        folder: None,
        mime_allow: Vec::new(),
    };
    field
}
