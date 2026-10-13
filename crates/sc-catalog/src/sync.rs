//! Offline sync, its first half: recording every change to a synced table
//! (§14.11, `TODO-offline-sync.md` S1).
//!
//! A table with sync switched on (`_fd_tables.attributes.sync.enabled`) gets one
//! row trigger, and every insert, update and delete on it writes an entry to
//! [`_fd_changes`](CHANGES_TABLE) **in the same transaction** — so a change and
//! its entry commit or roll back together, and a write made outside Saltcorn
//! (`psql`, another program) is recorded like any other.
//!
//! All tables share one `plpgsql` function, [`RECORD_FUNCTION`]; the trigger
//! passes it the name of the table's primary-key column. Reading the row as JSON
//! means a column added later is recorded without touching the trigger.
//!
//! User tables gain nothing: no version column, no "deleted" flag. A delete is an
//! entry in the log.

use sc_db::ColumnGenerator;
use sc_error::{Error, Result};
use sc_query::SqlDialect;
use sc_types::{BasicType, TypeRef};
use serde_json::Value as Json;

use crate::catalog::{Catalog, SchemaStep};
use crate::field::{Attrs, DataField};
use crate::table::Table;

/// The change-log: one entry per recorded insert, update or delete.
pub const CHANGES_TABLE: &str = "_fd_changes";
/// The function every sync trigger calls.
pub const RECORD_FUNCTION: &str = "_fd_record_change";
/// The name of the trigger on a synced table — one per table, so a table's
/// trigger is found and dropped by this name.
pub const SYNC_TRIGGER: &str = "_fd_sync";
/// Set by a device's upload for the length of its transaction, so the entries
/// it causes name the device (§14.11). Unset, the entry's `client_id` is null.
pub const SYNC_CLIENT_GUC: &str = "sc.sync_client";
/// The device's own number for the change being applied.
pub const SYNC_MUTATION_GUC: &str = "sc.sync_mutation";

/// `_fd_tables.attributes` key holding a table's sync settings, an object
/// (`{ "enabled": true }`) so later settings have somewhere to go.
pub const ATTR_SYNC: &str = "sync";

/// The log's columns. `txid` is the writing transaction's id, which a pull
/// compares against the oldest transaction still running (the watermark), so
/// a transaction that commits late is never skipped.
fn changes_fields() -> Vec<DataField> {
    let int = || TypeRef::Basic(BasicType::Int);
    let text = || TypeRef::Basic(BasicType::Text);
    vec![
        DataField::plain("seq", int())
            .required()
            .primary_key()
            .generated(ColumnGenerator::Identity),
        DataField::plain("table_name", text()).required(),
        DataField::plain("row_key", text()).required(),
        DataField::plain("op", text()).required(),
        // An update's changed columns, as a JSON array of names; null otherwise.
        DataField::plain("fields", TypeRef::Basic(BasicType::Json)),
        DataField::plain("txid", int()).required(),
        DataField::plain("client_id", TypeRef::Basic(BasicType::Uuid)),
        DataField::plain("mutation", int()),
        DataField::plain("at", TypeRef::Basic(BasicType::Timestamp))
            .required()
            .generated(ColumnGenerator::Default("now()".into())),
    ]
}

/// Ensure the change-log, its indexes and the trigger function exist.
///
/// Idempotent, and a no-op on a backend that cannot record changes with a
/// trigger: there is nothing to sync there yet, and switching a table's sync
/// on is refused with a sentence saying so.
pub async fn bootstrap_sync(catalog: &Catalog) -> Result<()> {
    if !catalog.primary().capabilities().sync_triggers {
        return Ok(());
    }
    catalog
        .bootstrap_table(CHANGES_TABLE, &changes_fields())
        .await?;
    let dialect = catalog.primary().dialect();
    let table = dialect.quote_ident(CHANGES_TABLE);
    catalog
        .apply_schema_batch(&[SchemaStep::Sql(format!(
            "CREATE INDEX IF NOT EXISTS {by_tx} ON {table} (txid, seq);\n\
             CREATE INDEX IF NOT EXISTS {by_row} ON {table} (table_name, row_key, txid);\n\
             {function}",
            by_tx = dialect.quote_ident("_fd_changes_txid"),
            by_row = dialect.quote_ident("_fd_changes_row"),
            function = record_function_sql(),
        ))])
        .await
}

/// The trigger function, `CREATE OR REPLACE`d on every boot so a fix to it
/// reaches every synced table at once.
///
/// - An update that changes nothing records nothing.
/// - A change of primary key is recorded as a delete of the old key and an
///   insert of the new one: to a device, they are two different rows.
/// - The device and its change number come from transaction-local settings,
///   which only a device's upload sets.
/// - It runs with its owner's rights (`SECURITY DEFINER`), so whoever writes a
///   synced table needs no rights on the change-log, and cannot forge entries
///   in it. Its `search_path` is pinned to the schema the change-log was created
///   in, so nobody can slip another `_fd_changes` in front of it.
fn record_function_sql() -> String {
    format!(
        r#"CREATE OR REPLACE FUNCTION {RECORD_FUNCTION}() RETURNS trigger LANGUAGE plpgsql
SECURITY DEFINER AS $fd$
DECLARE
  pk      text  := TG_ARGV[0];
  new_row jsonb := CASE WHEN TG_OP <> 'DELETE' THEN to_jsonb(NEW) END;
  old_row jsonb := CASE WHEN TG_OP <> 'INSERT' THEN to_jsonb(OLD) END;
  changed jsonb;
  cid     uuid   := nullif(current_setting('{SYNC_CLIENT_GUC}', true), '')::uuid;
  mut     bigint := nullif(current_setting('{SYNC_MUTATION_GUC}', true), '')::bigint;
BEGIN
  IF TG_OP = 'UPDATE' THEN
    SELECT jsonb_agg(n.key ORDER BY n.key) INTO changed
      FROM jsonb_each(new_row) n
     WHERE n.value IS DISTINCT FROM old_row -> n.key;
    IF changed IS NULL THEN
      RETURN NULL;
    END IF;
    IF old_row ->> pk IS DISTINCT FROM new_row ->> pk THEN
      INSERT INTO {CHANGES_TABLE} (table_name, row_key, op, txid, client_id, mutation)
      VALUES (TG_TABLE_NAME, old_row ->> pk, 'd', txid_current(), cid, mut),
             (TG_TABLE_NAME, new_row ->> pk, 'i', txid_current(), cid, mut);
      RETURN NULL;
    END IF;
  END IF;
  INSERT INTO {CHANGES_TABLE} (table_name, row_key, op, fields, txid, client_id, mutation)
  VALUES (TG_TABLE_NAME, coalesce(new_row, old_row) ->> pk, lower(left(TG_OP, 1)),
          changed, txid_current(), cid, mut);
  RETURN NULL;
END
$fd$;
DO $fd$ BEGIN
  EXECUTE format('ALTER FUNCTION {RECORD_FUNCTION}() SET search_path = %I, pg_temp',
                 current_schema());
END $fd$;"#
    )
}

/// The DDL that starts recording `table`'s changes, keyed by the column `pk`.
/// Drops a trigger left from before first, so it also re-points the trigger
/// when the primary key changed.
pub fn enable_sync_sql(dialect: &dyn SqlDialect, table: &str, pk: &str) -> String {
    format!(
        "{drop}\nCREATE TRIGGER {trigger} AFTER INSERT OR UPDATE OR DELETE ON {table} \
         FOR EACH ROW EXECUTE FUNCTION {RECORD_FUNCTION}({pk});",
        drop = disable_sync_sql(dialect, table),
        trigger = dialect.quote_ident(SYNC_TRIGGER),
        table = dialect.quote_ident(table),
        pk = dialect.quote_literal(pk),
    )
}

/// The DDL that stops recording `table`'s changes. The entries already in the
/// log stay: a device may still need them.
pub fn disable_sync_sql(dialect: &dyn SqlDialect, table: &str) -> String {
    format!(
        "DROP TRIGGER IF EXISTS {} ON {};",
        dialect.quote_ident(SYNC_TRIGGER),
        dialect.quote_ident(table)
    )
}

/// Whether `attributes` (a table's, or its overlay row's) has sync switched on.
pub fn sync_enabled(attributes: &Attrs) -> bool {
    attributes
        .get(ATTR_SYNC)
        .and_then(|s| s.get("enabled"))
        .and_then(Json::as_bool)
        .unwrap_or(false)
}

/// Switch sync on or off in `attributes`. Off removes the key, so a table that
/// never synced and one that stopped look the same.
pub fn set_sync_enabled(attributes: &mut Attrs, enabled: bool) {
    if enabled {
        attributes.insert(ATTR_SYNC.into(), serde_json::json!({ "enabled": true }));
    } else {
        attributes.remove(ATTR_SYNC);
    }
}

/// Why `table` cannot have sync switched on, or `Ok` with the primary-key
/// column its trigger is keyed by.
///
/// A device addresses a row by its key, so the key must be one column. Rows
/// must be in Saltcorn's own database, since the trigger is created there, and
/// not in a metadata table.
pub fn sync_key(catalog: &Catalog, table: &Table) -> Result<String> {
    let name = &table.name;
    if !catalog.primary().capabilities().sync_triggers {
        return Err(Error::invalid(
            "offline sync needs a Postgres database; this one cannot record changes yet",
        ));
    }
    if table.is_system() {
        return Err(Error::invalid(format!(
            "table `{name}` is one of Saltcorn's own tables and cannot be synced"
        )));
    }
    if let Some((module, provider)) = table.provider() {
        return Err(Error::invalid(format!(
            "table `{name}` is served by the table provider `{provider}` of `{module}`; \
             its rows are not in a database, so there is nothing to record changes in"
        )));
    }
    if table.database != crate::DbId::primary() {
        return Err(Error::invalid(format!(
            "table `{name}` lives in database connection `{}`; offline sync records changes \
             in Saltcorn's own database only",
            table.database.0
        )));
    }
    match table.primary_key.as_slice() {
        [pk] => Ok(pk.clone()),
        [] => Err(Error::invalid(format!(
            "table `{name}` has no primary key; a device finds a row by its key, so a synced \
             table needs one"
        ))),
        _ => Err(Error::invalid(format!(
            "table `{name}` has a primary key of several columns; offline sync needs a key \
             of one column"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_is_an_object_in_the_attributes_and_off_leaves_nothing() {
        let mut attrs = Attrs::new();
        assert!(!sync_enabled(&attrs));
        set_sync_enabled(&mut attrs, true);
        assert!(sync_enabled(&attrs));
        assert_eq!(attrs[ATTR_SYNC], serde_json::json!({ "enabled": true }));
        set_sync_enabled(&mut attrs, false);
        assert!(attrs.is_empty());
    }
}
