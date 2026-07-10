//! The Postgres SQL dialect.
//!
//! Rendering a [`Statement`](sc_query::Statement) is almost entirely shared: the
//! [`SqlDialect`] trait carries a default `render` that walks the AST and emits
//! clause order, operator spelling, `RETURNING`, `CASE`, `IN`, and the JSON
//! access path (`->`) — all of which are already valid Postgres. A concrete
//! dialect therefore only supplies the two things that genuinely differ between
//! backends: how an identifier is quoted and how a bind placeholder is spelled
//! (technical design §4, §5).
//!
//! For Postgres that is:
//!
//! - **Quoting**: wrap in double quotes and double any embedded double quote, so
//!   `we"ird` becomes `"we""ird"`. This makes any identifier — including a
//!   reserved word or one with unusual characters — safe and case-exact.
//! - **Placeholders**: the numbered `$1`, `$2`, … form that tokio-postgres binds
//!   positionally.
//!
//! Everything a `Value` literal carries leaves through the bind vector the shared
//! renderer builds, never interpolated — so this dialect is injection-safe by
//! construction, exactly as the query layer guarantees.

use sc_query::SqlDialect;

/// The Postgres [`SqlDialect`]. Zero-sized: a dialect holds no state, it only
/// spells identifiers and placeholders.
///
/// This is the dialect a Postgres `DatabaseDriver` hands back from
/// `dialect()`; it renders a Postgres-flavoured migration or query to
/// `(sql, binds)`.
#[derive(Debug, Clone, Copy, Default)]
pub struct PgDialect;

impl PgDialect {
    /// Construct the Postgres dialect.
    pub const fn new() -> Self {
        PgDialect
    }
}

impl SqlDialect for PgDialect {
    fn quote_ident(&self, ident: &str) -> String {
        // Postgres quoting: double quotes, with embedded quotes doubled.
        let mut out = String::with_capacity(ident.len() + 2);
        out.push('"');
        for ch in ident.chars() {
            if ch == '"' {
                out.push('"');
            }
            out.push(ch);
        }
        out.push('"');
        out
    }

    fn placeholder(&self, position: usize) -> String {
        format!("${position}")
    }
}

#[cfg(test)]
mod tests {
    use sc_query::{
        Assignment, Delete, Expr, InSet, Insert, Join, JoinKind, JsonStep, OrderBy, Projection,
        Select, Source, SqlDialect, Statement, Update, Value,
    };

    use super::*;

    fn render(stmt: impl Into<Statement>) -> (String, Vec<Value>) {
        PgDialect.render(&stmt.into()).expect("render")
    }

    #[test]
    fn quotes_identifiers_and_escapes_embedded_quotes() {
        assert_eq!(PgDialect.quote_ident("email"), "\"email\"");
        // A reserved word is safe once quoted.
        assert_eq!(PgDialect.quote_ident("select"), "\"select\"");
        // Embedded double quotes are doubled.
        assert_eq!(PgDialect.quote_ident("we\"ird"), "\"we\"\"ird\"");
    }

    #[test]
    fn placeholders_are_dollar_numbered() {
        assert_eq!(PgDialect.placeholder(1), "$1");
        assert_eq!(PgDialect.placeholder(42), "$42");
    }

    #[test]
    fn select_with_join_filter_order_uses_pg_quoting_and_placeholders() {
        let stmt = Select::from(Source::table_as("users", "u"))
            .columns(vec![
                Projection::expr(Expr::qcol("u", "id")),
                Projection::expr_as(Expr::qcol("u", "email"), "e"),
            ])
            .join(Join {
                kind: JoinKind::Inner,
                source: Source::table_as("roles", "r"),
                on: Some(Expr::qcol("u", "role").eq(Expr::qcol("r", "id"))),
            })
            .filter(Expr::qcol("u", "email").eq(Expr::lit("a@b.c")))
            .limit(10)
            .offset(5);
        let stmt = Select {
            order: vec![OrderBy::desc(Expr::qcol("u", "id"))],
            ..stmt
        };

        let (sql, binds) = render(stmt);
        assert_eq!(
            sql,
            "SELECT \"u\".\"id\", \"u\".\"email\" AS \"e\" FROM \"users\" AS \"u\" \
             INNER JOIN \"roles\" AS \"r\" ON (\"u\".\"role\" = \"r\".\"id\") \
             WHERE (\"u\".\"email\" = $1) ORDER BY \"u\".\"id\" DESC LIMIT $2 OFFSET $3"
        );
        assert_eq!(
            binds,
            vec![Value::Text("a@b.c".into()), Value::Int(10), Value::Int(5)]
        );
    }

    #[test]
    fn insert_renders_returning() {
        let stmt = Insert {
            table: "users".into(),
            columns: vec!["email".into(), "role".into()],
            rows: vec![vec![Expr::lit("a@b.c"), Expr::lit(1_i64)]],
            returning: vec![Projection::expr(Expr::col("id"))],
        };
        let (sql, binds) = render(stmt);
        assert_eq!(
            sql,
            "INSERT INTO \"users\" (\"email\", \"role\") VALUES ($1, $2) RETURNING \"id\""
        );
        assert_eq!(binds, vec![Value::Text("a@b.c".into()), Value::Int(1)]);
    }

    #[test]
    fn update_and_delete_render_with_returning() {
        let (sql, binds) = render(
            Update::new("users", vec![Assignment::new("role", Expr::lit(3_i64))])
                .filter(Expr::col("id").eq(Expr::lit(7_i64))),
        );
        assert_eq!(
            sql,
            "UPDATE \"users\" SET \"role\" = $1 WHERE (\"id\" = $2)"
        );
        assert_eq!(binds, vec![Value::Int(3), Value::Int(7)]);

        let stmt = Delete {
            table: "users".into(),
            filter: Some(Expr::col("id").eq(Expr::lit(7_i64))),
            returning: vec![Projection::expr(Expr::col("email"))],
        };
        let (sql, binds) = render(stmt);
        assert_eq!(
            sql,
            "DELETE FROM \"users\" WHERE (\"id\" = $1) RETURNING \"email\""
        );
        assert_eq!(binds, vec![Value::Int(7)]);
    }

    #[test]
    fn json_access_uses_arrow_operator_and_parameterises_path() {
        // WHERE (data -> 'a' -> 0) IN ('x', 'y') — the arrow operator is valid
        // Postgres JSON access, and every path step is a bind, not inlined.
        let json = Expr::Json {
            target: Box::new(Expr::col("data")),
            path: vec![JsonStep::Field("a".into()), JsonStep::Index(0)],
        };
        let filter = Expr::In {
            e: Box::new(json),
            set: InSet::List(vec![Expr::lit("x"), Expr::lit("y")]),
        };
        let (sql, binds) = render(Select::from(Source::table("t")).filter(filter));
        assert_eq!(
            sql,
            "SELECT * FROM \"t\" WHERE (\"data\" -> $1 -> $2 IN ($3, $4))"
        );
        assert_eq!(
            binds,
            vec![
                Value::Text("a".into()),
                Value::Int(0),
                Value::Text("x".into()),
                Value::Text("y".into()),
            ]
        );
    }

    #[test]
    fn injection_payload_stays_a_single_bind() {
        let payload = "'; DROP TABLE users; --";
        let (sql, binds) = render(
            Select::from(Source::table("t")).filter(Expr::col("name").eq(Expr::lit(payload))),
        );
        assert!(
            !sql.contains("DROP TABLE"),
            "payload leaked into SQL: {sql}"
        );
        assert_eq!(sql, "SELECT * FROM \"t\" WHERE (\"name\" = $1)");
        assert_eq!(binds, vec![Value::Text(payload.into())]);
    }
}
