//! SQL rendering: the [`SqlDialect`] trait and a shared, dialect-parameterised
//! renderer.
//!
//! Rendering is a trait so each database driver controls the details that differ
//! between SQL dialects — identifier quoting and bind-placeholder syntax
//! (technical design §4). Everything else (clause order, operator spelling, the
//! `RETURNING` / `CASE` / `IN` / JSON structure) is shared: [`SqlDialect`]
//! provides a default [`render`](SqlDialect::render) that walks a
//! [`Statement`](crate::Statement) and delegates only the dialect-specific bits
//! back to the two required hooks.
//!
//! The cardinal rule: **every [`Value`] literal is parameterised, never
//! interpolated** into the SQL string. Column/table *names* are quoted via
//! [`quote_ident`](SqlDialect::quote_ident); *values* — including JSON path
//! steps — always leave through the returned bind vector. This is the
//! query-layer half of the injection-safety story.

use sc_error::Result;

use crate::{
    Assignment, BinOp, CaseArm, ColRef, Delete, Expr, InSet, Insert, Join, JoinKind, JsonStep,
    Nulls, OrderBy, OrderDir, Projection, Select, Source, Statement, UnOp, Update, Value,
};

/// Renders a [`Statement`](crate::Statement) to `(sql, binds)` for one SQL
/// dialect.
///
/// Implementors provide only [`quote_ident`](Self::quote_ident) and
/// [`placeholder`](Self::placeholder); the default [`render`](Self::render)
/// supplies the rest. A dialect that needs to diverge further may override
/// `render` wholesale.
pub trait SqlDialect {
    /// Quote an identifier (table, column, or alias) for this dialect, escaping
    /// as needed. Postgres, for example, wraps in double quotes and doubles any
    /// embedded double quote.
    fn quote_ident(&self, ident: &str) -> String;

    /// The placeholder text for the `position`-th bind parameter (1-based).
    /// Postgres renders `$1`, `$2`, …; a `?`-style dialect ignores `position`.
    fn placeholder(&self, position: usize) -> String;

    /// Quote a **string literal** for inlining into SQL text — the escaping half
    /// of [`render_policy_expr`]. Postgres wraps in single quotes and doubles any
    /// embedded single quote. Only reached for DDL that cannot bind; ordinary
    /// rendering always parameterises instead.
    fn quote_literal(&self, literal: &str) -> String {
        format!("'{}'", literal.replace('\'', "''"))
    }

    /// Render a statement to a SQL string plus its ordered bind values.
    ///
    /// The returned `Vec<Value>` is exactly the parameters the SQL placeholders
    /// refer to, in order; no literal is ever inlined into the string.
    fn render(&self, stmt: &Statement) -> Result<(String, Vec<Value>)> {
        let mut r = Renderer::new(self);
        r.statement(stmt)?;
        Ok((r.sql, r.binds))
    }
}

/// Render an [`Expr`] to a standalone SQL string with **literals inlined** — for
/// the DDL contexts that cannot take bind parameters, namely a row-level-security
/// `CREATE POLICY` expression (§7.3) and, later, a `CHECK` constraint.
///
/// This is the deliberate exception to the "every literal is parameterised" rule
/// (see [`SqlDialect`]), and it is a narrow one. It exists only for expressions
/// built by trusted code — a translated ownership formula, whose literals are the
/// strings, numbers and booleans an admin typed and which were parsed and
/// validated first. Even so, every inlined value is quoted through the dialect's
/// own [`quote_literal`](SqlDialect::quote_literal), and any [`Value`] that is
/// not a plain scalar (bytes, a uuid, a temporal value — none of which a formula
/// literal can be) is **refused** rather than guessed at. A [`Param`] has no
/// value to inline and is likewise refused.
pub fn render_policy_expr<D: SqlDialect + ?Sized>(dialect: &D, expr: &Expr) -> Result<String> {
    let mut r = Renderer::new(dialect);
    r.inline_literals = true;
    r.expr(expr)?;
    Ok(r.sql)
}

/// Accumulates SQL text and bind values while walking the AST.
struct Renderer<'a, D: ?Sized> {
    dialect: &'a D,
    sql: String,
    binds: Vec<Value>,
    /// When set, a [`Value`] literal is written into the SQL text (quoted)
    /// rather than parameterised — the [`render_policy_expr`] DDL mode.
    inline_literals: bool,
}

impl<'a, D: SqlDialect + ?Sized> Renderer<'a, D> {
    fn new(dialect: &'a D) -> Self {
        Renderer {
            dialect,
            sql: String::new(),
            binds: Vec::new(),
            inline_literals: false,
        }
    }

    fn push(&mut self, s: &str) {
        self.sql.push_str(s);
    }

    /// Append a value to the bind list and emit its placeholder. This is the
    /// only path by which a [`Value`] enters the SQL, guaranteeing literals are
    /// parameterised — except in [`render_policy_expr`]'s inline mode, where the
    /// value is quoted into the text instead (for DDL that cannot bind).
    fn bind(&mut self, v: Value) -> Result<()> {
        if self.inline_literals {
            let literal = self.inline_literal(&v)?;
            self.push(&literal);
            return Ok(());
        }
        self.binds.push(v);
        let placeholder = self.dialect.placeholder(self.binds.len());
        self.push(&placeholder);
        Ok(())
    }

    /// A scalar [`Value`] as inlined SQL text, quoted where it is a string. Any
    /// non-scalar value is refused — see [`render_policy_expr`].
    fn inline_literal(&self, v: &Value) -> Result<String> {
        Ok(match v {
            Value::Null => "NULL".to_owned(),
            Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_owned(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) if f.is_finite() => format!("{f:?}"),
            Value::Text(s) => self.dialect.quote_literal(s),
            other => {
                return Err(sc_error::Error::query(format!(
                    "cannot inline a {} literal into SQL DDL",
                    other.kind()
                )));
            }
        })
    }

    fn ident(&mut self, ident: &str) {
        let quoted = self.dialect.quote_ident(ident);
        self.push(&quoted);
    }

    fn statement(&mut self, stmt: &Statement) -> Result<()> {
        match stmt {
            Statement::Select(s) => self.select(s),
            Statement::Insert(i) => self.insert(i),
            Statement::Update(u) => self.update(u),
            Statement::Delete(d) => self.delete(d),
        }
    }

    fn select(&mut self, s: &Select) -> Result<()> {
        if s.columns.is_empty() {
            return Err(sc_error::Error::query("SELECT has no projection columns"));
        }
        self.push("SELECT ");
        for (i, col) in s.columns.iter().enumerate() {
            if i > 0 {
                self.push(", ");
            }
            self.projection(col)?;
        }
        self.push(" FROM ");
        self.source(&s.from)?;
        for join in &s.joins {
            self.join(join)?;
        }
        if let Some(filter) = &s.filter {
            self.push(" WHERE ");
            self.expr(filter)?;
        }
        if !s.group.is_empty() {
            self.push(" GROUP BY ");
            for (i, g) in s.group.iter().enumerate() {
                if i > 0 {
                    self.push(", ");
                }
                self.expr(g)?;
            }
        }
        if let Some(having) = &s.having {
            self.push(" HAVING ");
            self.expr(having)?;
        }
        if !s.order.is_empty() {
            self.push(" ORDER BY ");
            for (i, o) in s.order.iter().enumerate() {
                if i > 0 {
                    self.push(", ");
                }
                self.order_by(o)?;
            }
        }
        if let Some(limit) = s.limit {
            self.push(" LIMIT ");
            self.bind(Value::Int(limit as i64))?;
        }
        if let Some(offset) = s.offset {
            self.push(" OFFSET ");
            self.bind(Value::Int(offset as i64))?;
        }
        Ok(())
    }

    fn insert(&mut self, ins: &Insert) -> Result<()> {
        if ins.columns.is_empty() {
            return Err(sc_error::Error::query("INSERT has no columns"));
        }
        if ins.rows.is_empty() {
            return Err(sc_error::Error::query("INSERT has no rows"));
        }
        self.push("INSERT INTO ");
        self.ident(&ins.table);
        self.push(" (");
        for (i, c) in ins.columns.iter().enumerate() {
            if i > 0 {
                self.push(", ");
            }
            self.ident(c);
        }
        self.push(") VALUES ");
        for (ri, row) in ins.rows.iter().enumerate() {
            if row.len() != ins.columns.len() {
                return Err(sc_error::Error::query(format!(
                    "INSERT row {ri} has {} values but {} columns",
                    row.len(),
                    ins.columns.len()
                )));
            }
            if ri > 0 {
                self.push(", ");
            }
            self.push("(");
            for (i, v) in row.iter().enumerate() {
                if i > 0 {
                    self.push(", ");
                }
                self.expr(v)?;
            }
            self.push(")");
        }
        self.returning(&ins.returning)?;
        Ok(())
    }

    fn update(&mut self, upd: &Update) -> Result<()> {
        if upd.assignments.is_empty() {
            return Err(sc_error::Error::query("UPDATE has no assignments"));
        }
        self.push("UPDATE ");
        self.ident(&upd.table);
        self.push(" SET ");
        for (i, a) in upd.assignments.iter().enumerate() {
            if i > 0 {
                self.push(", ");
            }
            self.assignment(a)?;
        }
        if let Some(filter) = &upd.filter {
            self.push(" WHERE ");
            self.expr(filter)?;
        }
        self.returning(&upd.returning)?;
        Ok(())
    }

    fn delete(&mut self, del: &Delete) -> Result<()> {
        self.push("DELETE FROM ");
        self.ident(&del.table);
        if let Some(filter) = &del.filter {
            self.push(" WHERE ");
            self.expr(filter)?;
        }
        self.returning(&del.returning)?;
        Ok(())
    }

    fn returning(&mut self, cols: &[Projection]) -> Result<()> {
        if cols.is_empty() {
            return Ok(());
        }
        self.push(" RETURNING ");
        for (i, c) in cols.iter().enumerate() {
            if i > 0 {
                self.push(", ");
            }
            self.projection(c)?;
        }
        Ok(())
    }

    fn assignment(&mut self, a: &Assignment) -> Result<()> {
        self.ident(&a.column);
        self.push(" = ");
        self.expr(&a.value)
    }

    fn projection(&mut self, p: &Projection) -> Result<()> {
        match p {
            Projection::Wildcard { table } => {
                if let Some(t) = table {
                    self.ident(t);
                    self.push(".");
                }
                self.push("*");
                Ok(())
            }
            Projection::Expr { expr, alias } => {
                self.expr(expr)?;
                if let Some(alias) = alias {
                    self.push(" AS ");
                    self.ident(alias);
                }
                Ok(())
            }
        }
    }

    fn source(&mut self, src: &Source) -> Result<()> {
        match src {
            Source::Table { name, alias } => {
                self.ident(name);
                if let Some(alias) = alias {
                    self.push(" AS ");
                    self.ident(alias);
                }
                Ok(())
            }
            Source::Subquery { query, alias } => {
                self.push("(");
                self.select(query)?;
                self.push(") AS ");
                self.ident(alias);
                Ok(())
            }
        }
    }

    fn join(&mut self, join: &Join) -> Result<()> {
        let keyword = match join.kind {
            JoinKind::Inner => " INNER JOIN ",
            JoinKind::Left => " LEFT JOIN ",
            JoinKind::Right => " RIGHT JOIN ",
            JoinKind::Full => " FULL JOIN ",
            JoinKind::Cross => " CROSS JOIN ",
        };
        self.push(keyword);
        self.source(&join.source)?;
        if let Some(on) = &join.on {
            self.push(" ON ");
            self.expr(on)?;
        }
        Ok(())
    }

    fn order_by(&mut self, o: &OrderBy) -> Result<()> {
        self.expr(&o.expr)?;
        match o.dir {
            OrderDir::Asc => self.push(" ASC"),
            OrderDir::Desc => self.push(" DESC"),
        }
        if let Some(nulls) = o.nulls {
            match nulls {
                Nulls::First => self.push(" NULLS FIRST"),
                Nulls::Last => self.push(" NULLS LAST"),
            }
        }
        Ok(())
    }

    fn col_ref(&mut self, c: &ColRef) {
        if let Some(table) = &c.table {
            self.ident(table);
            self.push(".");
        }
        self.ident(&c.column);
    }

    fn expr(&mut self, e: &Expr) -> Result<()> {
        match e {
            Expr::Col(c) => {
                self.col_ref(c);
                Ok(())
            }
            Expr::Lit(v) => self.bind(v.clone()),
            Expr::Param(i) => {
                // A pre-bound external parameter: emit a placeholder for the
                // 1-based position without adding to the collected binds.
                let placeholder = self.dialect.placeholder(i + 1);
                self.push(&placeholder);
                Ok(())
            }
            Expr::Binary { op, l, r } => {
                self.push("(");
                self.expr(l)?;
                self.push(" ");
                self.push(bin_op(*op));
                self.push(" ");
                self.expr(r)?;
                self.push(")");
                Ok(())
            }
            Expr::Unary { op, e } => self.unary(*op, e),
            Expr::Func { name, args } => {
                // The function name is structural (from the AST), not user data.
                self.push(name);
                self.push("(");
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        self.push(", ");
                    }
                    self.expr(a)?;
                }
                self.push(")");
                Ok(())
            }
            Expr::In { e, set } => {
                self.push("(");
                self.expr(e)?;
                self.push(" IN ");
                match set {
                    InSet::List(items) => {
                        self.push("(");
                        for (i, item) in items.iter().enumerate() {
                            if i > 0 {
                                self.push(", ");
                            }
                            self.expr(item)?;
                        }
                        self.push(")");
                    }
                    InSet::Subquery(q) => {
                        self.push("(");
                        self.select(q)?;
                        self.push(")");
                    }
                }
                self.push(")");
                Ok(())
            }
            Expr::Json { target, path } => {
                self.expr(target)?;
                // Each path step is parameterised: `-> $n`, never inlined, so a
                // field name can never break out into SQL.
                for step in path {
                    self.push(" -> ");
                    match step {
                        JsonStep::Field(f) => self.bind(Value::Text(f.clone()))?,
                        JsonStep::Index(i) => self.bind(Value::Int(*i))?,
                    }
                }
                Ok(())
            }
            Expr::Case {
                operand,
                arms,
                else_result,
            } => self.case(operand.as_deref(), arms, else_result.as_deref()),
            Expr::Subquery(q) => {
                self.push("(");
                self.select(q)?;
                self.push(")");
                Ok(())
            }
            Expr::Cast { expr, type_name } => {
                // The type name is structural, but it is the one string in the
                // AST that is neither quoted as an identifier nor bound as a
                // value — so refuse anything that could not be a SQL type name
                // rather than trust the caller entirely.
                if !is_sql_type_name(type_name) {
                    return Err(sc_error::Error::query(format!(
                        "invalid SQL type name in CAST: {type_name:?}"
                    )));
                }
                self.push("CAST(");
                self.expr(expr)?;
                self.push(" AS ");
                self.push(type_name);
                self.push(")");
                Ok(())
            }
        }
    }

    fn unary(&mut self, op: UnOp, e: &Expr) -> Result<()> {
        match op {
            UnOp::Not => {
                self.push("(NOT ");
                self.expr(e)?;
                self.push(")");
            }
            UnOp::Neg => {
                self.push("(-");
                self.expr(e)?;
                self.push(")");
            }
            UnOp::IsNull => {
                self.push("(");
                self.expr(e)?;
                self.push(" IS NULL)");
            }
            UnOp::IsNotNull => {
                self.push("(");
                self.expr(e)?;
                self.push(" IS NOT NULL)");
            }
        }
        Ok(())
    }

    fn case(
        &mut self,
        operand: Option<&Expr>,
        arms: &[CaseArm],
        else_result: Option<&Expr>,
    ) -> Result<()> {
        self.push("CASE");
        if let Some(operand) = operand {
            self.push(" ");
            self.expr(operand)?;
        }
        for arm in arms {
            self.push(" WHEN ");
            self.expr(&arm.when)?;
            self.push(" THEN ");
            self.expr(&arm.then)?;
        }
        if let Some(else_result) = else_result {
            self.push(" ELSE ");
            self.expr(else_result)?;
        }
        self.push(" END");
        Ok(())
    }
}

/// The SQL spelling of a binary operator.
fn bin_op(op: BinOp) -> &'static str {
    match op {
        BinOp::Eq => "=",
        BinOp::Ne => "<>",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::And => "AND",
        BinOp::Or => "OR",
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::Like => "LIKE",
        BinOp::ILike => "ILIKE",
        BinOp::Concat => "||",
        BinOp::IsNotDistinct => "IS NOT DISTINCT FROM",
        BinOp::IsDistinct => "IS DISTINCT FROM",
    }
}

/// Whether `s` is plausibly a SQL type name: letters, digits, underscores,
/// spaces (`timestamp with time zone`), a precision list (`numeric(10,2)`) or
/// an array suffix (`text[]`). Anything else — quotes, semicolons, comment
/// starts — is refused by the `CAST` renderer.
fn is_sql_type_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '_' | ' ' | '(' | ')' | ',' | '[' | ']')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Projection, Source};

    /// A minimal Postgres-flavoured dialect used only to exercise the shared
    /// renderer: double-quoted identifiers and `$n` placeholders.
    struct TestDialect;

    impl SqlDialect for TestDialect {
        fn quote_ident(&self, ident: &str) -> String {
            format!("\"{}\"", ident.replace('"', "\"\""))
        }
        fn placeholder(&self, position: usize) -> String {
            format!("${position}")
        }
    }

    fn render(stmt: impl Into<Statement>) -> (String, Vec<Value>) {
        TestDialect.render(&stmt.into()).expect("render")
    }

    #[test]
    fn select_with_join_filter_order_limit() {
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
    fn insert_multi_row_with_returning() {
        let stmt = Insert {
            table: "users".into(),
            columns: vec!["email".into(), "role".into()],
            rows: vec![
                vec![Expr::lit("a@b.c"), Expr::lit(1_i64)],
                vec![Expr::lit("d@e.f"), Expr::lit(2_i64)],
            ],
            returning: vec![Projection::expr(Expr::col("id"))],
        };
        let (sql, binds) = render(stmt);
        assert_eq!(
            sql,
            "INSERT INTO \"users\" (\"email\", \"role\") VALUES ($1, $2), ($3, $4) \
             RETURNING \"id\""
        );
        assert_eq!(
            binds,
            vec![
                Value::Text("a@b.c".into()),
                Value::Int(1),
                Value::Text("d@e.f".into()),
                Value::Int(2),
            ]
        );
    }

    #[test]
    fn update_and_delete() {
        let (sql, binds) = render(
            Update::new("users", vec![Assignment::new("role", Expr::lit(3_i64))])
                .filter(Expr::col("email").eq(Expr::lit("a@b.c"))),
        );
        assert_eq!(
            sql,
            "UPDATE \"users\" SET \"role\" = $1 WHERE (\"email\" = $2)"
        );
        assert_eq!(binds, vec![Value::Int(3), Value::Text("a@b.c".into())]);

        let (sql, binds) =
            render(Delete::from("users").filter(Expr::col("id").eq(Expr::lit(7_i64))));
        assert_eq!(sql, "DELETE FROM \"users\" WHERE (\"id\" = $1)");
        assert_eq!(binds, vec![Value::Int(7)]);
    }

    #[test]
    fn json_in_and_case_are_parameterised() {
        // WHERE (data -> 'a' -> 0) IN ('x', 'y')
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
        // The JSON field name and index are binds, not inlined text.
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
    fn literals_are_never_interpolated() {
        // A classic injection payload must survive intact as a single bind and
        // never appear in the SQL text.
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

    #[test]
    fn empty_projection_is_an_error() {
        let stmt: Statement = Select::from(Source::table("t")).columns(vec![]).into();
        assert!(TestDialect.render(&stmt).is_err());
    }

    #[test]
    fn distinct_from_operators_render() {
        let (sql, binds) = render(Select::from(Source::table("t")).filter(Expr::binary(
            BinOp::IsNotDistinct,
            Expr::col("owner"),
            Expr::lit("u1"),
        )));
        assert_eq!(
            sql,
            "SELECT * FROM \"t\" WHERE (\"owner\" IS NOT DISTINCT FROM $1)"
        );
        assert_eq!(binds, vec![Value::Text("u1".into())]);

        let (sql, _) = render(Select::from(Source::table("t")).filter(Expr::binary(
            BinOp::IsDistinct,
            Expr::col("owner"),
            Expr::lit("u1"),
        )));
        assert!(sql.contains("IS DISTINCT FROM"), "got: {sql}");
    }

    #[test]
    fn a_scalar_subquery_renders_parenthesised_with_its_binds_in_order() {
        // WHERE (SELECT p.name FROM publishers AS p WHERE (p.id = t.publisher))
        //       IS NOT DISTINCT FROM 'ACME'
        let sub = Select::from(Source::table_as("publishers", "p"))
            .columns(vec![Projection::expr(Expr::qcol("p", "name"))])
            .filter(Expr::qcol("p", "id").eq(Expr::qcol("t", "publisher")));
        let filter = Expr::binary(
            BinOp::IsNotDistinct,
            Expr::Subquery(Box::new(sub)),
            Expr::lit("ACME"),
        );
        let (sql, binds) = render(Select::from(Source::table("t")).filter(filter));
        assert_eq!(
            sql,
            "SELECT * FROM \"t\" WHERE ((SELECT \"p\".\"name\" FROM \"publishers\" AS \"p\" \
             WHERE (\"p\".\"id\" = \"t\".\"publisher\")) IS NOT DISTINCT FROM $1)"
        );
        assert_eq!(binds, vec![Value::Text("ACME".into())]);
    }

    #[test]
    fn policy_expr_inlines_literals_and_quotes_strings() {
        // The DDL exception: a policy predicate has no bind list, so literals
        // are written into the text — strings quoted, an embedded quote doubled.
        let expr = Expr::binary(
            BinOp::IsNotDistinct,
            Expr::col("owner"),
            Expr::lit("O'Brien"),
        );
        let sql = render_policy_expr(&TestDialect, &expr).unwrap();
        assert_eq!(sql, "(\"owner\" IS NOT DISTINCT FROM 'O''Brien')");

        // Numbers, bools and null inline bare; no `$1` anywhere.
        let expr = Expr::binary(
            BinOp::Or,
            Expr::col("pages").eq(Expr::lit(100_i64)),
            Expr::Lit(Value::Null),
        );
        let sql = render_policy_expr(&TestDialect, &expr).unwrap();
        assert_eq!(sql, "((\"pages\" = 100) OR NULL)");
        assert!(
            !sql.contains('$'),
            "a policy expr must not parameterise: {sql}"
        );
    }

    #[test]
    fn policy_expr_refuses_a_non_scalar_literal() {
        // A formula literal can only be a string/number/bool/null; anything
        // else (a uuid, bytes) has no safe inline form and is refused rather
        // than guessed — the DDL would otherwise be malformed or unsafe.
        let expr = Expr::Lit(Value::Uuid(uuid::Uuid::nil()));
        assert!(render_policy_expr(&TestDialect, &expr).is_err());
    }

    #[test]
    fn cast_renders_and_refuses_a_type_name_that_is_not_one() {
        let (sql, _) = render(
            Select::from(Source::table("t")).filter(
                Expr::Cast {
                    expr: Box::new(Expr::col("x")),
                    type_name: "uuid".into(),
                }
                .eq(Expr::lit("y")),
            ),
        );
        assert!(sql.contains("CAST(\"x\" AS uuid)"), "got: {sql}");

        // Compound type names pass; injection-shaped ones do not.
        for good in ["timestamp with time zone", "numeric(10,2)", "text[]"] {
            let stmt: Statement = Select::from(Source::table("t"))
                .filter(Expr::Cast {
                    expr: Box::new(Expr::col("x")),
                    type_name: good.into(),
                })
                .into();
            assert!(TestDialect.render(&stmt).is_ok(), "{good} should render");
        }
        for bad in ["uuid; DROP TABLE users", "uuid'--", ""] {
            let stmt: Statement = Select::from(Source::table("t"))
                .filter(Expr::Cast {
                    expr: Box::new(Expr::col("x")),
                    type_name: bad.into(),
                })
                .into();
            assert!(TestDialect.render(&stmt).is_err(), "{bad:?} should refuse");
        }
    }
}
