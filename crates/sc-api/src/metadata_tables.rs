//! Adding one of Saltcorn's own **metadata tables** (`_fd_triggers`,
//! `_fd_file_stores`, …) to the tables list, and taking it off again.
//!
//! A metadata table is not created: it is already in the database, and the
//! catalog already knows it — as a system table, hidden from every list. Adding
//! it writes a `_fd_tables` row carrying
//! [`ATTR_METADATA_TABLE`](sc_catalog::ATTR_METADATA_TABLE), and from then on
//! it is listed, its rows are edited and imported through the ordinary row
//! endpoints, and its access rules, ownership formula, label and description are
//! set through `updateTable` like any other table's.
//!
//! What stays Saltcorn's is the **shape**: the schema editor refuses every DDL
//! operation on an `_fd_*` table and `save_field_meta` refuses its fields, so
//! there is nothing to add here for that. And removing one is forgetting the
//! row — the table and its rows are the server's, and are never dropped.

use sc_catalog::{
    Catalog, SchemaChanged, Table, TableMeta, delete_table_meta, load_table_meta_by_name,
    save_table_meta_row,
};
use sc_error::{Error, Result};

/// The metadata tables that could be added: every system table not already in
/// the tables list, by name.
pub fn available(catalog: &Catalog) -> Result<Vec<String>> {
    let mut names: Vec<String> = catalog
        .tables()?
        .into_iter()
        .filter(Table::is_hidden)
        .map(|t| t.name)
        .collect();
    names.sort();
    Ok(names)
}

/// Add the system table `name` to the tables list, returning it as the catalog
/// now presents it. Admin-only until its settings say otherwise, exactly as a
/// fresh overlay row always is.
pub async fn add(catalog: &Catalog, name: &str) -> Result<Table> {
    let name = name.trim();
    let table = catalog.get(name)?.filter(Table::is_system).ok_or_else(|| {
        Error::not_found(format!("`{name}` is not one of Saltcorn's metadata tables"))
    })?;
    if table.is_metadata() {
        return Err(Error::invalid(format!(
            "`{name}` is already in the tables list"
        )));
    }
    let mut meta = load_table_meta_by_name(catalog, name)
        .await?
        .unwrap_or_else(|| TableMeta::new(name));
    meta.set_metadata_table(true);
    save_table_meta_row(catalog, &meta).await?;
    catalog.reload().await?;
    catalog.require(name)
}

/// After rows of `table` were written through the ordinary row endpoints: if it
/// is a metadata table the catalog is built from, reload the catalog so the
/// change takes effect now. A metadata table the catalog does not read
/// (`_fd_runs`, say), and every ordinary table, costs nothing.
///
/// There is no partial reload, so this is the whole catalog.
pub async fn after_row_write(catalog: &Catalog, table: &Table) -> Result<()> {
    if table.is_metadata() && Catalog::reload_reads(&table.name) {
        catalog.reload().await?;
    }
    Ok(())
}

/// Take a metadata table off the tables list: forget its row, settings and
/// all. `false` when it was not on it. The table and its rows are untouched.
pub async fn remove(catalog: &Catalog, name: &str) -> Result<bool> {
    let Some(meta) = load_table_meta_by_name(catalog, name).await? else {
        return Ok(false);
    };
    if !meta.is_metadata_table() {
        return Err(Error::invalid(format!("`{name}` is not a metadata table")));
    }
    let deleted = delete_table_meta(catalog, meta.id).await?;
    // An application exposing it must stop exposing it now.
    catalog.notify_schema_changed(&SchemaChanged::TableChanged(name.to_owned()))?;
    Ok(deleted)
}
