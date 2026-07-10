//! Live-schema introspection: read the connected database's tables straight from
//! the catalog into [`PhysicalTable`]s.
//!
//! Per the goals there is no discovery/registration step — everything a
//! connection can see is returned here (technical design §5). We read all base
//! tables in non-system schemas.
//!
//! Columns and their types come from `information_schema`; primary keys and
//! foreign keys come from `pg_catalog` instead, because the goals require
//! **composite primary keys** and **foreign keys to non-primary-key columns**,
//! and only the catalog's `conkey`/`confkey` attribute-number arrays give the
//! key columns in the correct order (the `information_schema` constraint views
//! lose that ordering for composite keys). `unnest(… ) WITH ORDINALITY`
//! preserves it.

use std::collections::BTreeMap;

use sc_db::{Column, ForeignKey, PhysicalTable};
use sc_error::{Error, Result};
use tokio_postgres::{Client, Row};

/// All user base tables, keyed later by `(schema, name)`.
const TABLES_SQL: &str = "\
    SELECT table_schema, table_name \
    FROM information_schema.tables \
    WHERE table_type = 'BASE TABLE' \
      AND table_schema NOT IN ('pg_catalog', 'information_schema') \
    ORDER BY table_schema, table_name";

/// Every column of every user table, in declaration order. `udt_name` is the
/// backend's own type name (`int8`, `text`, `timestamptz`, …), matching what
/// `apply_schema` emits, rather than the friendlier `data_type`.
const COLUMNS_SQL: &str = "\
    SELECT table_schema, table_name, column_name, udt_name, is_nullable, column_default \
    FROM information_schema.columns \
    WHERE table_schema NOT IN ('pg_catalog', 'information_schema') \
    ORDER BY table_schema, table_name, ordinal_position";

/// Primary-key columns, one row per key column, ordered within each key so a
/// composite key is reassembled correctly.
const PK_SQL: &str = "\
    SELECT n.nspname, t.relname, a.attname \
    FROM pg_constraint c \
    JOIN pg_class t ON t.oid = c.conrelid \
    JOIN pg_namespace n ON n.oid = t.relnamespace \
    JOIN LATERAL unnest(c.conkey) WITH ORDINALITY AS k(attnum, ord) ON true \
    JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum \
    WHERE c.contype = 'p' \
      AND n.nspname NOT IN ('pg_catalog', 'information_schema') \
    ORDER BY n.nspname, t.relname, k.ord";

/// Foreign-key columns, one row per constrained column, pairing each local
/// column with the referenced column it points at (which need not be a primary
/// key). Ordered within each constraint.
const FK_SQL: &str = "\
    SELECT n.nspname, t.relname, c.conname, la.attname, ft.relname, fa.attname \
    FROM pg_constraint c \
    JOIN pg_class t ON t.oid = c.conrelid \
    JOIN pg_namespace n ON n.oid = t.relnamespace \
    JOIN pg_class ft ON ft.oid = c.confrelid \
    JOIN LATERAL unnest(c.conkey, c.confkey) WITH ORDINALITY AS k(local_attnum, ref_attnum, ord) ON true \
    JOIN pg_attribute la ON la.attrelid = c.conrelid AND la.attnum = k.local_attnum \
    JOIN pg_attribute fa ON fa.attrelid = c.confrelid AND fa.attnum = k.ref_attnum \
    WHERE c.contype = 'f' \
      AND n.nspname NOT IN ('pg_catalog', 'information_schema') \
    ORDER BY n.nspname, t.relname, c.conname, k.ord";

/// The `(schema, table)` identity used to collate rows from the separate
/// queries.
type TableKey = (String, String);

/// Introspect all user base tables reachable through `client`.
pub async fn introspect(client: &Client) -> Result<Vec<PhysicalTable>> {
    let mut tables: BTreeMap<TableKey, PhysicalTable> = BTreeMap::new();

    for row in run(client, TABLES_SQL).await? {
        let schema: String = row.get(0);
        let name: String = row.get(1);
        tables.insert(
            (schema.clone(), name.clone()),
            PhysicalTable {
                name,
                schema: Some(schema),
                columns: Vec::new(),
                primary_key: Vec::new(),
                foreign_keys: Vec::new(),
            },
        );
    }

    for row in run(client, COLUMNS_SQL).await? {
        let key: TableKey = (row.get(0), row.get(1));
        if let Some(table) = tables.get_mut(&key) {
            let is_nullable: String = row.get(4);
            table.columns.push(Column {
                name: row.get(2),
                sql_type: row.get(3),
                nullable: is_nullable == "YES",
                default: row.get(5),
            });
        }
    }

    for row in run(client, PK_SQL).await? {
        let key: TableKey = (row.get(0), row.get(1));
        if let Some(table) = tables.get_mut(&key) {
            table.primary_key.push(row.get(2));
        }
    }

    // Foreign keys span multiple rows (one per column pair); collate by
    // constraint name before turning each into a `ForeignKey`.
    type FkParts = (String, Vec<String>, Vec<String>); // (ref_table, local, referenced)
    let mut by_table: BTreeMap<TableKey, BTreeMap<String, FkParts>> = BTreeMap::new();
    for row in run(client, FK_SQL).await? {
        let key: TableKey = (row.get(0), row.get(1));
        let conname: String = row.get(2);
        let local_col: String = row.get(3);
        let ref_table: String = row.get(4);
        let ref_col: String = row.get(5);
        let parts = by_table
            .entry(key)
            .or_default()
            .entry(conname)
            .or_insert_with(|| (ref_table, Vec::new(), Vec::new()));
        parts.1.push(local_col);
        parts.2.push(ref_col);
    }
    for (key, constraints) in by_table {
        if let Some(table) = tables.get_mut(&key) {
            for (_conname, (ref_table, columns, referenced_columns)) in constraints {
                table.foreign_keys.push(ForeignKey {
                    columns,
                    referenced_table: ref_table,
                    referenced_columns,
                });
            }
        }
    }

    Ok(tables.into_values().collect())
}

/// Run a parameterless catalog query, mapping the driver error into ours.
async fn run(client: &Client, sql: &str) -> Result<Vec<Row>> {
    client
        .query(sql, &[])
        .await
        .map_err(|e| Error::database(format!("introspect query failed: {e}")))
}
