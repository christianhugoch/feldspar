//! Universal query language: enum AST and SQL rendering trait (layer 1)
//!
//! The query layer is a plain, serializable data representation of a SQL
//! statement — an enum AST, not fluent builder calls (technical design §4).
//! Each [`SqlDialect`] renders a [`Statement`] to concrete SQL with its literals
//! parameterised; a `TableProvider` may interpret the same AST directly.
//!
//! The pieces:
//!
//! - [`Value`] — the universal row value type carried by literals and binds.
//! - [`Expr`] and friends — scalar expression trees.
//! - [`Statement`] ([`Select`]/[`Insert`]/[`Update`]/[`Delete`]) — the
//!   top-level query AST.

mod dialect;
mod expr;
mod statement;
mod value;

pub use dialect::{SqlDialect, render_policy_expr};
pub use expr::{BinOp, CaseArm, ColRef, Expr, InSet, JsonStep, UnOp};
pub use statement::{
    Assignment, Delete, Insert, Join, JoinKind, Nulls, OrderBy, OrderDir, Projection, Select,
    Source, Statement, Update,
};
pub use value::Value;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_is_exported() {
        assert_eq!(Value::Int(1).kind(), "int");
    }

    #[test]
    fn build_select_with_join_filter_and_order() {
        // SELECT u.id, u.email FROM users u
        //   INNER JOIN roles r ON u.role = r.id
        //   WHERE u.email = <lit> AND r.id = <lit>
        //   ORDER BY u.email ASC LIMIT 10 OFFSET 5
        let select = Select::from(Source::table_as("users", "u"))
            .columns(vec![
                Projection::expr(Expr::qcol("u", "id")),
                Projection::expr(Expr::qcol("u", "email")),
            ])
            .join(Join {
                kind: JoinKind::Inner,
                source: Source::table_as("roles", "r"),
                on: Some(Expr::qcol("u", "role").eq(Expr::qcol("r", "id"))),
            })
            .filter(
                Expr::qcol("u", "email")
                    .eq(Expr::lit("a@b.c"))
                    .and(Expr::qcol("r", "id").eq(Expr::lit(1_i64))),
            )
            .limit(10)
            .offset(5);
        let select = Select {
            order: vec![OrderBy::asc(Expr::qcol("u", "email"))],
            ..select
        };

        let stmt: Statement = select.into();
        match &stmt {
            Statement::Select(s) => {
                assert_eq!(s.columns.len(), 2);
                assert_eq!(s.joins.len(), 1);
                assert_eq!(s.joins[0].kind, JoinKind::Inner);
                assert!(s.filter.is_some());
                assert_eq!(s.order.len(), 1);
                assert_eq!(s.limit, Some(10));
                assert_eq!(s.offset, Some(5));
            }
            _ => panic!("expected a select"),
        }
    }

    #[test]
    fn build_insert_update_delete() {
        let insert: Statement = Insert::row(
            "users",
            vec!["email".into(), "role".into()],
            vec![Expr::lit("a@b.c"), Expr::lit(1_i64)],
        )
        .returning(vec![Projection::expr(Expr::col("id"))])
        .into();
        assert!(matches!(insert, Statement::Insert(_)));

        let update: Statement =
            Update::new("users", vec![Assignment::new("role", Expr::lit(2_i64))])
                .filter(Expr::col("email").eq(Expr::lit("a@b.c")))
                .into();
        assert!(matches!(update, Statement::Update(_)));

        let delete: Statement = Delete::from("users")
            .filter(Expr::col("id").eq(Expr::lit(7_i64)))
            .into();
        match delete {
            Statement::Delete(d) => {
                assert_eq!(d.table, "users");
                assert!(d.filter.is_some());
            }
            _ => panic!("expected a delete"),
        }
    }

    #[test]
    fn expr_json_in_and_case_are_expressible() {
        // WHERE (data -> 'a' -> 0) IN (1, 2)
        let json = Expr::Json {
            target: Box::new(Expr::col("data")),
            path: vec![JsonStep::Field("a".into()), JsonStep::Index(0)],
        };
        let in_expr = Expr::In {
            e: Box::new(json),
            set: InSet::List(vec![Expr::lit(1_i64), Expr::lit(2_i64)]),
        };
        assert!(matches!(in_expr, Expr::In { .. }));

        let case = Expr::Case {
            operand: None,
            arms: vec![CaseArm {
                when: Expr::col("active").eq(Expr::lit(true)),
                then: Expr::lit("yes"),
            }],
            else_result: Some(Box::new(Expr::lit("no"))),
        };
        assert!(matches!(case, Expr::Case { .. }));
    }

    #[test]
    fn a_window_expression_round_trips_through_serde() {
        // A window carries an `OrderBy` inside an `Expr`, the one place the two
        // modules refer to each other — so it is worth asserting the AST is
        // still plain, serializable data (§4).
        let stmt: Statement = Select::from(Source::table_as("c", "c"))
            .columns(vec![Projection::Expr {
                expr: Expr::row_number(
                    vec![Expr::qcol("c", "parent")],
                    vec![OrderBy::desc(Expr::qcol("c", "id"))],
                ),
                alias: Some("rn".into()),
            }])
            .into();
        let json = serde_json::to_string(&stmt).expect("serialize");
        let back: Statement = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(stmt, back);
    }

    #[test]
    fn statement_round_trips_through_serde() {
        let stmt: Statement = Select::from(Source::table("users"))
            .filter(Expr::col("email").eq(Expr::lit("a@b.c")))
            .into();
        let json = serde_json::to_string(&stmt).expect("serialize");
        let back: Statement = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(stmt, back);
    }
}
