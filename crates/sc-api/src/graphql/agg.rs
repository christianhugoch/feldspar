//! What an aggregate selection asks for, and the SQL it lowers to.
//!
//! `employees_aggregate(where: …) { count sum { salary } }` is two questions,
//! not one, and each becomes one expression: `count(*)` and
//! `coalesce(sum("salary"), 0)`. Neither is computed here — both come from
//! [`sc_expr`]'s builders, the same ones a Ↄ-chain in a calculated field or an
//! ownership formula goes through — so `sum` over no rows is `0` and `avg` over
//! no rows is null wherever the question is asked. This module only reads the
//! selection set and decides which builder to call.
//!
//! **Where the values ride back.** Every aggregate is a column of some
//! `SELECT`, and it needs a name to be read back under. That name is the
//! selection's own **response key path** — the alias the caller wrote, or the
//! field name when they wrote none — joined by [`RESPONSE_SEP`]: `count`,
//! `total.salary`, and, for an aggregate correlated into a parent row's query,
//! `employees_aggregate.count`. Response keys are unique within a selection set
//! by GraphQL's own rules, so two aggregates of one relation under different
//! `where` arguments cannot collide; and a response key is a GraphQL *name*
//! (`/[_A-Za-z][_0-9A-Za-z]*/`), so nothing a caller writes reaches SQL as
//! anything but a quoted alias.

use async_graphql::{SelectionField, Value as GqlValue};
use sc_catalog::{DataFieldKind, Table};
use sc_error::{Error, Result};
use sc_expr::{AggFunc, AggregateSpec, aggregate_expr, correlated_aggregate};
use sc_query::{Expr, Projection};

use super::args::ARG_DISTINCT;

/// What separates one level of a response-key path from the next.
pub const RESPONSE_SEP: char = '.';

/// The prefix a correlated aggregate's subquery alias takes.
///
/// Deliberately not `_fd_a`, which is what the formula translator numbers *its*
/// subqueries from: a child table's ownership formula may itself contain a
/// Ↄ-aggregation, and that subquery is nested inside this one. Two different
/// prefixes is how the inner alias cannot shadow the outer one it correlates to.
const CHILD_ALIAS_PREFIX: &str = "_fd_g";

/// One value an aggregate selection asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct AggSelection {
    /// The response-key path this value rides back under, relative to the
    /// aggregate field itself: `count`, or `total.salary`.
    pub key: String,
    /// Which aggregate.
    pub func: AggFunc,
    /// `count(distinct: column)`.
    pub distinct: bool,
    /// The column aggregated; `None` is `count(*)`.
    pub column: Option<String>,
}

/// Read one `XAggregate` selection set: one [`AggSelection`] per requested
/// value, in the order they were asked for.
///
/// The schema has already restricted what can appear here — the aggregate
/// object's fields, and the columns of the result objects — so a name that is
/// not one of those cannot arrive. It is still checked against the table rather
/// than trusted: a column name is about to become an identifier in a statement,
/// and the catalog is the only thing that may decide one.
pub fn selections(table: &Table, field: SelectionField<'_>) -> Result<Vec<AggSelection>> {
    let mut out = Vec::new();
    for sub in field.selection_set() {
        let name = sub.name();
        // `__typename` names no aggregate; the executor answers it itself.
        if name.starts_with("__") {
            continue;
        }
        let key = response_key(&sub);
        match name {
            "count" => {
                let column = distinct_column(table, &sub)?;
                out.push(AggSelection {
                    key,
                    func: AggFunc::Count,
                    distinct: column.is_some(),
                    column,
                });
            }
            "sum" | "avg" | "min" | "max" => {
                let func = match name {
                    "sum" => AggFunc::Sum,
                    "avg" => AggFunc::Avg,
                    "min" => AggFunc::Min,
                    _ => AggFunc::Max,
                };
                for leaf in sub.selection_set() {
                    let column = leaf.name();
                    if column.starts_with("__") {
                        continue;
                    }
                    aggregatable(table, column)?;
                    out.push(AggSelection {
                        key: format!("{key}{RESPONSE_SEP}{}", response_key(&leaf)),
                        func: func.clone(),
                        distinct: false,
                        column: Some(column.to_owned()),
                    });
                }
            }
            other => {
                return Err(Error::invalid(format!(
                    "`{other}` is not an aggregate over `{}`",
                    table.name
                )));
            }
        }
    }
    Ok(out)
}

/// The key one selection's value is returned under: the caller's alias, or the
/// field's name when they wrote none — exactly what the response is keyed by.
pub fn response_key<'a>(field: &SelectionField<'a>) -> String {
    field.alias().unwrap_or_else(|| field.name()).to_owned()
}

/// The projections a **root** aggregate lowers to: one expression per value,
/// over the rows the enclosing `SELECT` already restricts.
pub fn root_projections(table: &Table, selections: &[AggSelection]) -> Result<Vec<Projection>> {
    selections
        .iter()
        .map(|s| {
            let value = s.column.as_ref().map(|c| Expr::col(c.clone()));
            let expr = aggregate_expr(&s.func, s.distinct, value, &table.name)?;
            Ok(Projection::expr_as(expr, s.key.clone()))
        })
        .collect()
}

/// Which relation a correlated aggregate follows, and how the parent row is
/// named in the query it is being projected into.
pub struct Correlation<'a> {
    /// The child table being aggregated.
    pub child: &'a Table,
    /// The child's key column — the one referencing the parent.
    pub key_field: &'a str,
    /// The parent's table name or alias, as the enclosing query spells it.
    pub parent: &'a str,
    /// The parent column that key references.
    pub parent_field: &'a str,
    /// The response key of the aggregate field itself, which every value of it
    /// rides back under.
    pub response_key: &'a str,
}

/// The projections a **child** aggregate lowers to: one correlated subquery per
/// value, projected as another column of the parent's own `SELECT`.
///
/// This is the milestone's motivating case (docs/GRAPHQL_API.md §3). Each value
/// gets its own subquery alias — numbered by `aliases`, which the caller keeps
/// across the whole statement — and `constrain` supplies whatever else that
/// subquery's rows must satisfy, given the alias: the caller's `where` and the
/// child table's ownership predicate, both of which have to name the alias
/// rather than the table.
pub fn child_projections(
    rel: &Correlation<'_>,
    selections: &[AggSelection],
    aliases: &mut usize,
    mut constrain: impl FnMut(&str) -> Result<Option<Expr>>,
) -> Result<Vec<Projection>> {
    let mut out = Vec::with_capacity(selections.len());
    for s in selections {
        *aliases += 1;
        let alias = format!("{CHILD_ALIAS_PREFIX}{aliases}");
        let expr = correlated_aggregate(AggregateSpec {
            child_table: rel.child.name.clone(),
            key_field: rel.key_field.to_owned(),
            parent: rel.parent.to_owned(),
            parent_field: rel.parent_field.to_owned(),
            func: s.func.clone(),
            distinct: s.distinct,
            value: s
                .column
                .as_ref()
                .map(|c| Expr::qcol(alias.clone(), c.clone())),
            filter: constrain(&alias)?,
            alias,
        })?;
        out.push(Projection::expr_as(
            expr,
            format!("{}{RESPONSE_SEP}{}", rel.response_key, s.key),
        ));
    }
    Ok(out)
}

/// The column a `count(distinct:)` names, checked against the table.
fn distinct_column(table: &Table, field: &SelectionField<'_>) -> Result<Option<String>> {
    let args = field
        .arguments()
        .map_err(|e| Error::invalid(format!("`count(distinct:)`: {e}")))?;
    let Some((_, value)) = args.iter().find(|(name, _)| name.as_str() == ARG_DISTINCT) else {
        return Ok(None);
    };
    // An enum literal in the document arrives as `Enum`; the same enum through
    // a variable arrives as `String`. They mean the same column.
    let column = match value {
        GqlValue::Enum(name) => name.as_str(),
        GqlValue::String(name) => name.as_str(),
        GqlValue::Null => return Ok(None),
        _ => {
            return Err(Error::invalid(format!(
                "`{ARG_DISTINCT}` on `{}` names a column",
                table.name
            )));
        }
    };
    aggregatable(table, column)?;
    Ok(Some(column.to_owned()))
}

/// Refuse a column an aggregate cannot be taken over, by name.
///
/// The same rule the schema builder applied when it decided which columns the
/// result objects have: a stored column (a `Key` is one), never a calculated
/// field — which has no column for the database to aggregate — and never a
/// `File`.
fn aggregatable(table: &Table, column: &str) -> Result<()> {
    match table.field(column) {
        Some(f) if matches!(f.kind, DataFieldKind::Plain | DataFieldKind::Key { .. }) => Ok(()),
        Some(_) => Err(Error::invalid(format!(
            "`{}`.`{column}` is not a column the database can aggregate",
            table.name
        ))),
        None => Err(Error::invalid(format!(
            "`{}` has no field `{column}`",
            table.name
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphql::testing::{id_field, plain_field, table_of, typed_field};
    use sc_query::{Select, Source, SqlDialect, Statement, Value};
    use sc_types::BasicType;

    struct Pg;

    impl SqlDialect for Pg {
        fn quote_ident(&self, ident: &str) -> String {
            format!("\"{}\"", ident.replace('"', "\"\""))
        }
        fn placeholder(&self, position: usize) -> String {
            format!("${position}")
        }
    }

    fn employees() -> Table {
        table_of(
            "employees",
            vec![
                id_field(),
                plain_field("name"),
                typed_field("salary", BasicType::Int),
            ],
        )
    }

    /// The rendered `SELECT` of a set of projections over `employees`.
    fn rendered(projections: Vec<Projection>) -> (String, Vec<Value>) {
        let stmt: Statement = Select::from(Source::table("employees"))
            .columns(projections)
            .into();
        Pg.render(&stmt).expect("renders")
    }

    fn sel(key: &str, func: AggFunc, column: Option<&str>) -> AggSelection {
        AggSelection {
            key: key.to_owned(),
            func,
            distinct: false,
            column: column.map(str::to_owned),
        }
    }

    #[test]
    fn a_root_aggregate_is_one_projection_per_requested_value() {
        // And the empty-relation semantics are the builder's: `sum` coalesces,
        // `avg` does not.
        let (sql, _) = rendered(
            root_projections(
                &employees(),
                &[
                    sel("count", AggFunc::Count, None),
                    sel("sum.salary", AggFunc::Sum, Some("salary")),
                    sel("avg.salary", AggFunc::Avg, Some("salary")),
                ],
            )
            .expect("lowers"),
        );
        assert!(sql.contains("count(*) AS \"count\""), "{sql}");
        // The empty-relation default is bound like every other literal.
        assert!(
            sql.contains("COALESCE(sum(\"salary\"), $1) AS \"sum.salary\""),
            "{sql}"
        );
        assert!(sql.contains("avg(\"salary\") AS \"avg.salary\""), "{sql}");
    }

    #[test]
    fn a_child_aggregate_is_a_correlated_subquery_per_value_with_its_own_alias() {
        // The motivating case: two aggregates of one relation, two subqueries,
        // two aliases, two response keys — and the caller's constraint folded
        // into each subquery's own WHERE rather than applied afterwards.
        let child = employees();
        let rel = Correlation {
            child: &child,
            key_field: "department",
            parent: "departments",
            parent_field: "id",
            response_key: "cheap",
        };
        let mut aliases = 0;
        let projections = child_projections(
            &rel,
            &[
                sel("count", AggFunc::Count, None),
                sel("total.salary", AggFunc::Sum, Some("salary")),
            ],
            &mut aliases,
            |alias| {
                Ok(Some(sc_query::Expr::binary(
                    sc_query::BinOp::Lt,
                    Expr::qcol(alias, "salary"),
                    Expr::lit(Value::Int(50_000)),
                )))
            },
        )
        .expect("lowers");
        let stmt: Statement = Select::from(Source::table("departments"))
            .columns(projections)
            .into();
        let (sql, binds) = Pg.render(&stmt).expect("renders");
        assert!(
            sql.contains(
                "(SELECT count(*) FROM \"employees\" AS \"_fd_g1\" \
                 WHERE ((\"_fd_g1\".\"department\" = \"departments\".\"id\") \
                 AND (\"_fd_g1\".\"salary\" < $1))) AS \"cheap.count\""
            ),
            "{sql}"
        );
        assert!(sql.contains("\"_fd_g2\""), "{sql}");
        assert!(sql.contains("AS \"cheap.total.salary\""), "{sql}");
        // The bound is a parameter, not text in the statement — and so is the
        // `0` a `sum` over no rows coalesces to.
        assert_eq!(
            binds,
            vec![Value::Int(50_000), Value::Int(50_000), Value::Int(0)]
        );
        assert_eq!(aliases, 2);
    }

    #[test]
    fn an_aggregate_over_something_that_is_not_a_column_is_refused_by_name() {
        let err = aggregatable(&employees(), "nope").unwrap_err();
        assert!(format!("{err}").contains("has no field `nope`"), "{err}");
    }
}
