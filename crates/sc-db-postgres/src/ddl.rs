//! Rendering a [`SchemaChange`] to Postgres DDL.
//!
//! One [`SchemaChange`] renders to one DDL statement (technical design §5).
//! Identifiers (table and column names) are quoted through the dialect; the
//! `sql_type` and `default` fields of a [`ColumnDef`] are **structural SQL
//! fragments** supplied by trusted schema definitions (e.g. `int8`,
//! `varchar(255)`, `now()`), not user data, so they pass through verbatim —
//! DDL cannot be parameterised in any case.
//!
//! The cardinal rule from the goals is honoured by omission: creating a table
//! emits **exactly** the columns given and **never** invents an `id` column. A
//! primary key is declared only when one is asked for, and may be composite.

use sc_db::{ColumnDef, SchemaChange};
use sc_error::{Error, Result};
use sc_query::SqlDialect;

use crate::dialect::PgDialect;

/// Render `change` to a single Postgres DDL statement.
pub fn render(dialect: &PgDialect, change: &SchemaChange) -> Result<String> {
    let sql = match change {
        SchemaChange::CreateTable {
            name,
            columns,
            primary_key,
        } => {
            // Every primary-key column must be one of the declared columns —
            // catch a malformed change before Postgres does, with a clearer
            // message.
            for pk in primary_key {
                if !columns.iter().any(|c| &c.name == pk) {
                    return Err(Error::invalid(format!(
                        "primary key column `{pk}` is not a column of table `{name}`"
                    )));
                }
            }
            let mut items: Vec<String> = columns.iter().map(|c| column_def(dialect, c)).collect();
            if !primary_key.is_empty() {
                let cols = primary_key
                    .iter()
                    .map(|c| dialect.quote_ident(c))
                    .collect::<Vec<_>>()
                    .join(", ");
                items.push(format!("PRIMARY KEY ({cols})"));
            }
            format!(
                "CREATE TABLE {} ({})",
                dialect.quote_ident(name),
                items.join(", ")
            )
        }
        SchemaChange::DropTable { name, if_exists } => {
            format!(
                "DROP TABLE {}{}",
                if_exists_clause(*if_exists),
                dialect.quote_ident(name)
            )
        }
        SchemaChange::AddColumn { table, column } => {
            format!(
                "ALTER TABLE {} ADD COLUMN {}",
                dialect.quote_ident(table),
                column_def(dialect, column)
            )
        }
        SchemaChange::DropColumn {
            table,
            column,
            if_exists,
        } => {
            format!(
                "ALTER TABLE {} DROP COLUMN {}{}",
                dialect.quote_ident(table),
                if_exists_clause(*if_exists),
                dialect.quote_ident(column)
            )
        }
    };
    Ok(sql)
}

/// Render one column definition: `"name" type [NOT NULL] [DEFAULT …] [UNIQUE]`.
fn column_def(dialect: &PgDialect, col: &ColumnDef) -> String {
    let mut s = dialect.quote_ident(&col.name);
    s.push(' ');
    s.push_str(&col.sql_type);
    if !col.nullable {
        s.push_str(" NOT NULL");
    }
    if let Some(default) = &col.default {
        s.push_str(" DEFAULT ");
        s.push_str(default);
    }
    if col.unique {
        s.push_str(" UNIQUE");
    }
    s
}

/// `"IF EXISTS "` or `""`.
fn if_exists_clause(if_exists: bool) -> &'static str {
    if if_exists { "IF EXISTS " } else { "" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_ok(change: &SchemaChange) -> String {
        render(&PgDialect::new(), change).expect("render")
    }

    #[test]
    fn create_table_with_composite_pk_and_no_invented_id() {
        let change = SchemaChange::CreateTable {
            name: "member".into(),
            columns: vec![
                ColumnDef::new("org", "int8").not_null(),
                ColumnDef::new("user_id", "int8").not_null(),
                ColumnDef::new("email", "text").unique(),
            ],
            primary_key: vec!["org".into(), "user_id".into()],
        };
        let sql = render_ok(&change);
        assert_eq!(
            sql,
            "CREATE TABLE \"member\" (\"org\" int8 NOT NULL, \"user_id\" int8 NOT NULL, \
             \"email\" text UNIQUE, PRIMARY KEY (\"org\", \"user_id\"))"
        );
        // No auto `id` column is ever added.
        assert!(!sql.contains("\"id\""));
    }

    #[test]
    fn create_table_renders_column_default() {
        let change = SchemaChange::CreateTable {
            name: "t".into(),
            columns: vec![ColumnDef::new("active", "bool").not_null().default("true")],
            primary_key: vec![],
        };
        assert_eq!(
            render_ok(&change),
            "CREATE TABLE \"t\" (\"active\" bool NOT NULL DEFAULT true)"
        );
    }

    #[test]
    fn create_table_rejects_pk_column_that_is_not_declared() {
        let change = SchemaChange::CreateTable {
            name: "t".into(),
            columns: vec![ColumnDef::new("a", "int8")],
            primary_key: vec!["b".into()],
        };
        assert!(render(&PgDialect::new(), &change).is_err());
    }

    #[test]
    fn drop_table_with_and_without_if_exists() {
        assert_eq!(
            render_ok(&SchemaChange::DropTable {
                name: "member".into(),
                if_exists: true,
            }),
            "DROP TABLE IF EXISTS \"member\""
        );
        assert_eq!(
            render_ok(&SchemaChange::DropTable {
                name: "member".into(),
                if_exists: false,
            }),
            "DROP TABLE \"member\""
        );
    }

    #[test]
    fn add_and_drop_column() {
        assert_eq!(
            render_ok(&SchemaChange::AddColumn {
                table: "member".into(),
                column: ColumnDef::new("note", "text"),
            }),
            "ALTER TABLE \"member\" ADD COLUMN \"note\" text"
        );
        assert_eq!(
            render_ok(&SchemaChange::DropColumn {
                table: "member".into(),
                column: "note".into(),
                if_exists: true,
            }),
            "ALTER TABLE \"member\" DROP COLUMN IF EXISTS \"note\""
        );
    }
}
