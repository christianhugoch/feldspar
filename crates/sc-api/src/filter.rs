//! The comparison vocabulary every filtering syntax lowers through.
//!
//! `eq`/`ne`/`gt`/`gte`/`lt`/`lte`/`in`/`nin`/`like`/`ilike`/`is_null` are what a
//! caller may say about one column, and this module is the **one** place that
//! decides what each of them means. A GraphQL `where: { due: { lt: "2026-01-01" } }`
//! and a REST `?due=lt.2026-01-01` are two spellings of one question, and they
//! reach the row layer as the same [`Expr`] — so a Date filter binds a date in
//! both surfaces, `is_null` means SQL's nullness in both, and a comparison added
//! here appears in both at once. A second lowering would be a second answer, and
//! the one that is wrong is the one nobody is reading.
//!
//! Nothing here reaches SQL as text: the column name comes from the catalog, the
//! operator comes from the `match` below, and every literal the caller wrote
//! becomes an [`Expr::Lit`] the query layer parameterises on render.
//!
//! **Literals are coerced against the column, not against JSON.** A filter value
//! goes through the row layer's own [`rows::column_value`], the same function an
//! insert's body goes through, so `"2026-01-01"` binds a *date* and not a string
//! — the comparison the database performs is then the one the column's type
//! means. The one deliberate exception is `like`/`ilike`, whose argument is a
//! **pattern** rather than a value of the column: `"%urgent%"` is not a legal
//! value for a `String` field with an `options` attribute, and holding a pattern
//! to the column's value rules would refuse a query that is perfectly well
//! formed.
//!
//! Null semantics are SQL's, not JavaScript's: `eq`/`ne` are `=` and `<>`, so
//! neither matches a null, and `is_null` is how nullness is asked about. That is
//! what a caller who knows Hasura or PostgREST expects, and inventing a
//! null-safe `eq` here would make the same filter mean different things in a
//! GraphQL `where`, in a REST query string and in an ownership formula.

use sc_catalog::Table;
use sc_error::{Error, Result};
use sc_query::{BinOp, Expr, InSet, UnOp, Value};
use serde_json::Value as Json;

use crate::rows;

/// The comparisons a filter may name, in the order an error message lists them.
///
/// Public so a syntax that has to *describe* the vocabulary — a query-string
/// parser refusing an operator by name — quotes this list rather than a second
/// copy of it.
pub const OPERATORS: [&str; 11] = [
    "eq", "ne", "gt", "gte", "lt", "lte", "in", "nin", "like", "ilike", "is_null",
];

/// One comparison operator applied to one column of `table`.
///
/// `col` is the column *expression* rather than the column name, because the
/// same comparison is built against a bare column (`"due"`) at the root of a
/// query and against a qualified one (`"_sc_a1"."due"`) inside a correlated
/// subquery. `column` is still the catalog name the operand is coerced against.
///
/// The operand is JSON: it is what a GraphQL input value converts to and what a
/// query-string token parses as, and [`rows::column_value`] is the coercion
/// both need anyway.
pub fn comparison(
    table: &Table,
    column: &str,
    col: Expr,
    op: &str,
    operand: &Json,
) -> Result<Expr> {
    let lit = |operand: &Json| -> Result<Expr> {
        Ok(Expr::lit(rows::column_value(table, column, operand)?))
    };
    let binary = |op: BinOp, operand: &Json| -> Result<Expr> {
        Ok(Expr::binary(op, col.clone(), lit(operand)?))
    };
    match op {
        "eq" => binary(BinOp::Eq, operand),
        "ne" => binary(BinOp::Ne, operand),
        "gt" => binary(BinOp::Gt, operand),
        "gte" => binary(BinOp::Ge, operand),
        "lt" => binary(BinOp::Lt, operand),
        "lte" => binary(BinOp::Le, operand),
        // A pattern is not a value of the column (see the module docs), so it is
        // bound as the text it is.
        "like" => Ok(Expr::binary(BinOp::Like, col, pattern(op, operand)?)),
        "ilike" => Ok(Expr::binary(BinOp::ILike, col, pattern(op, operand)?)),
        "in" | "nin" => {
            let Json::Array(items) = operand else {
                return Err(Error::invalid(format!(
                    "`{op}` on `{}`.`{column}` takes a list",
                    table.name
                )));
            };
            let set = InSet::List(items.iter().map(&lit).collect::<Result<Vec<_>>>()?);
            let member = Expr::In {
                e: Box::new(col),
                set,
            };
            Ok(match op {
                "in" => member,
                _ => Expr::unary(UnOp::Not, member),
            })
        }
        "is_null" => match operand {
            Json::Bool(true) => Ok(Expr::unary(UnOp::IsNull, col)),
            Json::Bool(false) => Ok(Expr::unary(UnOp::IsNotNull, col)),
            _ => Err(Error::invalid(format!(
                "`is_null` on `{}`.`{column}` takes a boolean",
                table.name
            ))),
        },
        other => Err(Error::invalid(format!(
            "`{other}` is not a comparison on `{}`.`{column}` — the comparisons are {}",
            table.name,
            OPERATORS.join(", ")
        ))),
    }
}

/// A `like`/`ilike` pattern, bound as text.
fn pattern(op: &str, operand: &Json) -> Result<Expr> {
    match operand {
        Json::String(s) => Ok(Expr::lit(Value::Text(s.clone()))),
        _ => Err(Error::invalid(format!("`{op}` takes a string pattern"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphql::testing::{id_field, plain_field, table_of, typed_field};
    use sc_query::SqlDialect;
    use sc_types::BasicType;

    /// Postgres-flavoured rendering, as in `sc-query`'s own tests.
    struct Pg;

    impl SqlDialect for Pg {
        fn quote_ident(&self, ident: &str) -> String {
            format!("\"{}\"", ident.replace('"', "\"\""))
        }
        fn placeholder(&self, position: usize) -> String {
            format!("${position}")
        }
    }

    fn tasks() -> Table {
        table_of(
            "tasks",
            vec![
                id_field(),
                plain_field("title"),
                typed_field("priority", BasicType::Int),
                typed_field("due", BasicType::Date),
            ],
        )
    }

    /// The rendered SQL and binds of one comparison.
    fn rendered(expr: &Expr) -> (String, Vec<Value>) {
        let stmt: sc_query::Statement = sc_query::Select::from(sc_query::Source::table("tasks"))
            .filter(expr.clone())
            .into();
        let (sql, binds) = Pg.render(&stmt).expect("renders");
        let where_ = sql
            .split_once(" WHERE ")
            .map(|(_, w)| w.to_owned())
            .unwrap_or_else(|| panic!("no WHERE in {sql}"));
        (where_, binds)
    }

    #[test]
    fn every_comparison_lowers_to_its_operator() {
        for (op, sql_op) in [
            ("eq", "="),
            ("ne", "<>"),
            ("gt", ">"),
            ("gte", ">="),
            ("lt", "<"),
            ("lte", "<="),
        ] {
            let expr = comparison(
                &tasks(),
                "priority",
                Expr::col("priority"),
                op,
                &serde_json::json!(3),
            )
            .expect("lowers");
            let (sql, binds) = rendered(&expr);
            assert_eq!(sql, format!("(\"priority\" {sql_op} $1)"));
            assert_eq!(binds, vec![Value::Int(3)]);
        }
    }

    #[test]
    fn a_date_filter_binds_a_date_and_not_a_string() {
        // The reason the coercion goes through the row layer: `"2026-01-01"` is
        // a string in JSON and a *date* in the column, and only the second one
        // compares the way the caller meant. Both syntaxes get this for free
        // precisely because they share this function.
        let expr = comparison(
            &tasks(),
            "due",
            Expr::col("due"),
            "lt",
            &serde_json::json!("2026-01-01"),
        )
        .expect("lowers");
        let (_, binds) = rendered(&expr);
        assert!(
            matches!(binds.as_slice(), [Value::Date(_)]),
            "expected a bound date, got {binds:?}"
        );
    }

    #[test]
    fn an_unknown_operator_is_refused_naming_it_and_the_vocabulary() {
        // The message is read by whoever typed the query string, so it says what
        // they could have typed instead.
        let err = comparison(
            &tasks(),
            "priority",
            Expr::col("priority"),
            "between",
            &serde_json::json!(3),
        )
        .unwrap_err();
        let text = format!("{err}");
        assert!(text.contains("`between` is not a comparison"), "{text}");
        assert!(text.contains("gte"), "{text}");
    }
}
