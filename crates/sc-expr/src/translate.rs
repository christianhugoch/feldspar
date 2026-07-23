//! Symbolic evaluation: translating a [`Formula`] into a `sc_query::Expr`
//! predicate (TODO Phase 2).
//!
//! One translator serves both consumers. The runtime-check path (Phase 5)
//! translates with [`UserEnv::Inline`], where the current user's values become
//! literals; the RLS path (Phase 6) translates with [`UserEnv::Guc`], where
//! `user.x` becomes a `current_setting('sc.user', true)` JSON extraction. The
//! operation flags never reach SQL: [`translate`] is called *per operation* and
//! the flags are folded to constants first, so `_read || owner === user.id`
//! simply becomes `TRUE` for a select.
//!
//! # The specified semantics
//!
//! The translation *is* the specification of what a formula means; Phase 3's
//! reified evaluator renders normalised JavaScript from the same AST to match
//! it, which is what makes parity between the two provable.
//!
//! - **Equality is JavaScript's, expressed in SQL**: `===`/`==` render as
//!   `IS NOT DISTINCT FROM` (and their negations as `IS DISTINCT FROM`), so
//!   `owner === user.id` with a null `owner` is *false* and its negation *true*
//!   — two-valued, exactly as JS. `x === null` is `x IS NULL`. Loose `==` is
//!   translated as strict: the type-coercion table is not part of the formula
//!   language, and the normalised rendering makes the reified side agree.
//! - **Ordered comparisons keep SQL semantics**: a null operand grants nothing.
//!   JS's `null < 5 === true` coercion is specified away; the normalised
//!   rendering wraps ordered comparisons in null guards to match.
//! - **`&&`/`||`/`!` are boolean here.** In a predicate position that agrees
//!   with JS truthiness; in a *value* position (`x === (a && b)`) JS returns an
//!   operand, not a boolean, so that is refused as untranslatable rather than
//!   silently wrong.
//! - **A bare value as a condition** (`vip && …` where `vip` is a field) needs
//!   the field's type to decide truthiness, which the translator does not have —
//!   untranslatable, with a message suggesting the explicit comparison. The two
//!   knowable cases are translated: bare `user` (object-or-null) and `user.x`
//!   where the env knows `x` is boolean.
//!
//! Anything outside the subset returns [`TranslateError::Untranslatable`]
//! naming the construct — Phase 5 catches that and falls back to the reified
//! evaluator; Phase 6 surfaces it as the reason RLS cannot be enabled.

use std::collections::BTreeMap;
use std::fmt;

use sc_error::Error;
use sc_query::{BinOp as QBinOp, Expr as QExpr, Projection, Select, Source, UnOp as QUnOp, Value};

use crate::analyze::{GLOBALS, JOIN, OpFlag};
use crate::ast::{Ast, BinaryOp, MemberProp, UnaryOp};
use crate::formula::Formula;
use crate::shape::SchemaShape;

/// The GUC that carries the logged-in user as a JSON object in
/// [`UserEnv::Guc`] mode. Phase 6 issues `SET LOCAL sc.user = '<json>'` per
/// transaction; a missing setting reads as SQL `NULL` via
/// `current_setting(…, true)`, so an unset context **fails closed** by
/// construction.
pub const USER_GUC: &str = "sc.user";

/// The access operation a formula is being translated for. Chooses the value
/// of each operation flag (`_write` is true for everything but [`Read`]).
///
/// [`Read`]: Operation::Read
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// A select.
    Read,
    /// An insert (checked against the proposed row).
    Insert,
    /// An update.
    Update,
    /// A delete.
    Delete,
}

impl Operation {
    /// The constant this operation folds `flag` to.
    fn flag_value(self, flag: OpFlag) -> bool {
        match flag {
            OpFlag::Read => self == Operation::Read,
            OpFlag::Insert => self == Operation::Insert,
            OpFlag::Update => self == Operation::Update,
            OpFlag::Delete => self == Operation::Delete,
            OpFlag::Write => self != Operation::Read,
        }
    }
}

/// How `user` renders into SQL — the one point where the two consumers differ.
#[derive(Debug, Clone, PartialEq)]
pub enum UserEnv {
    /// The current user's values are inlined as literals (always
    /// parameterised on render). `None` is "not logged in": `user` is null,
    /// every `user.x` is null. The map is the user's fields by name —
    /// `sc-auth`'s `User.extra` plus `id`/`role`, projected by the caller.
    Inline(Option<BTreeMap<String, Value>>),
    /// `user.x` renders as a cast JSON extraction from the [`USER_GUC`]
    /// setting, for RLS policies that outlive any one request. `field_types`
    /// maps a user field to the SQL type its extracted text is cast to (a
    /// missing entry or `text` means no cast); it is also what lets a bare
    /// `user.x` condition translate when `x` is boolean.
    Guc {
        /// User field name → SQL type name for the cast.
        field_types: BTreeMap<String, String>,
    },
}

/// Why a formula did not become SQL. The two variants are different verdicts:
/// [`Untranslatable`](TranslateError::Untranslatable) is a property of the
/// formula's *shape* — fall back to the reified evaluator (Phase 5) or refuse
/// to enable RLS naming the construct (Phase 6) — while
/// [`Error`](TranslateError::Error) is a real mistake (an unknown identifier,
/// a broken join path) that validation would also have caught.
#[derive(Debug)]
pub enum TranslateError {
    /// The construct has no SQL counterpart with matching semantics.
    Untranslatable(String),
    /// The formula is wrong regardless of evaluator.
    Error(Error),
}

impl fmt::Display for TranslateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TranslateError::Untranslatable(what) => {
                write!(f, "cannot be translated to SQL: {what}")
            }
            TranslateError::Error(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for TranslateError {}

impl From<TranslateError> for Error {
    fn from(e: TranslateError) -> Error {
        match e {
            TranslateError::Untranslatable(_) => Error::invalid(e.to_string()),
            TranslateError::Error(err) => err,
        }
    }
}

fn untranslatable<T>(what: impl Into<String>) -> Result<T, TranslateError> {
    Err(TranslateError::Untranslatable(what.into()))
}

/// Translate `formula` into a boolean `sc_query::Expr` over `table`, for one
/// operation, under one user environment. The result is what Phase 5 ANDs into
/// a statement's WHERE and Phase 6 renders into an RLS policy.
///
/// The caller is expected to have run [`Formula::validate`]; an invalid formula
/// still fails here (as [`TranslateError::Error`]), just with less polish.
pub fn translate(
    formula: &Formula,
    op: Operation,
    env: &UserEnv,
    shape: &SchemaShape,
    table: &str,
) -> Result<QExpr, TranslateError> {
    if !shape.tables.contains_key(table) {
        return Err(TranslateError::Error(Error::invalid(format!(
            "formula on `{table}`: unknown table `{table}`"
        ))));
    }
    let folded = fold(formula.ast(), op, &mut Vec::new());
    let mut tr = Translator {
        env,
        shape,
        table,
        aliases: 0,
    };
    tr.predicate(&folded)
}

/// Fold the operation flags to constants and simplify what the constants
/// decide. Only **value-exact** simplifications are applied — ones where the
/// JS value, not just its truthiness, is preserved — because folding runs
/// before we know whether a subtree sits in value or predicate position:
/// `false && x` → `false`, `true && x` → `x`, `true || x` → `true`,
/// `false || x` → `x`, `!literal`, and a conditional with a literal test.
/// (`x && true` is *not* folded: JS returns `x` when `x` is falsy.)
///
/// `locals` tracks arrow parameters, which shadow the flag names.
fn fold(ast: &Ast, op: Operation, locals: &mut Vec<String>) -> Ast {
    match ast {
        Ast::Ident(name) if !locals.iter().any(|l| l == name) => match OpFlag::from_ident(name) {
            Some(flag) => Ast::Bool(op.flag_value(flag)),
            None => ast.clone(),
        },
        Ast::Ident(_) | Ast::Str(_) | Ast::Num(_) | Ast::Bool(_) | Ast::Null => ast.clone(),
        Ast::Member {
            obj,
            prop,
            optional,
        } => Ast::Member {
            obj: Box::new(fold(obj, op, locals)),
            prop: match prop {
                MemberProp::Static(s) => MemberProp::Static(s.clone()),
                MemberProp::Computed(e) => MemberProp::Computed(Box::new(fold(e, op, locals))),
            },
            optional: *optional,
        },
        Ast::Call {
            callee,
            args,
            optional,
        } => Ast::Call {
            callee: Box::new(fold(callee, op, locals)),
            args: args.iter().map(|a| fold(a, op, locals)).collect(),
            optional: *optional,
        },
        Ast::Unary { op: uop, expr } => {
            let expr = fold(expr, op, locals);
            match (uop, &expr) {
                (UnaryOp::Not, Ast::Bool(b)) => Ast::Bool(!b),
                _ => Ast::Unary {
                    op: *uop,
                    expr: Box::new(expr),
                },
            }
        }
        Ast::Binary { op: bop, l, r } => {
            let l = fold(l, op, locals);
            let r = fold(r, op, locals);
            match (bop, &l) {
                (BinaryOp::And, Ast::Bool(false)) => Ast::Bool(false),
                (BinaryOp::And, Ast::Bool(true)) => r,
                (BinaryOp::Or, Ast::Bool(true)) => Ast::Bool(true),
                (BinaryOp::Or, Ast::Bool(false)) => r,
                _ => Ast::Binary {
                    op: *bop,
                    l: Box::new(l),
                    r: Box::new(r),
                },
            }
        }
        Ast::Cond { test, cons, alt } => {
            let test = fold(test, op, locals);
            match test {
                Ast::Bool(true) => fold(cons, op, locals),
                Ast::Bool(false) => fold(alt, op, locals),
                _ => Ast::Cond {
                    test: Box::new(test),
                    cons: Box::new(fold(cons, op, locals)),
                    alt: Box::new(fold(alt, op, locals)),
                },
            }
        }
        Ast::Array(elems) => Ast::Array(elems.iter().map(|e| fold(e, op, locals)).collect()),
        Ast::Template { quasis, exprs } => Ast::Template {
            quasis: quasis.clone(),
            exprs: exprs.iter().map(|e| fold(e, op, locals)).collect(),
        },
        Ast::Arrow { params, body } => {
            let depth = locals.len();
            locals.extend(params.iter().cloned());
            let body = fold(body, op, locals);
            locals.truncate(depth);
            Ast::Arrow {
                params: params.clone(),
                body: Box::new(body),
            }
        }
    }
}

struct Translator<'a> {
    env: &'a UserEnv,
    shape: &'a SchemaShape,
    table: &'a str,
    /// Counter for join-subquery aliases. Prefixed `_sc_` because user tables
    /// cannot start with it (§9 reserves the prefix), so an alias can never
    /// shadow a real table a correlated column reference points at.
    aliases: usize,
}

impl Translator<'_> {
    // ---- predicates --------------------------------------------------------

    /// Translate `ast` in boolean (predicate) position.
    fn predicate(&mut self, ast: &Ast) -> Result<QExpr, TranslateError> {
        match ast {
            Ast::Bool(b) => Ok(QExpr::lit(*b)),
            Ast::Unary {
                op: UnaryOp::Not,
                expr,
            } => Ok(QExpr::unary(QUnOp::Not, self.predicate(expr)?)),
            Ast::Binary { op, l, r } => match op {
                BinaryOp::And => Ok(self.predicate(l)?.and(self.predicate(r)?)),
                BinaryOp::Or => Ok(self.predicate(l)?.or(self.predicate(r)?)),
                BinaryOp::Eq | BinaryOp::StrictEq => self.equality(l, r, false),
                BinaryOp::NotEq | BinaryOp::StrictNotEq => self.equality(l, r, true),
                BinaryOp::Lt => self.comparison(QBinOp::Lt, l, r),
                BinaryOp::LtEq => self.comparison(QBinOp::Le, l, r),
                BinaryOp::Gt => self.comparison(QBinOp::Gt, l, r),
                BinaryOp::GtEq => self.comparison(QBinOp::Ge, l, r),
                BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => {
                    untranslatable("arithmetic as a condition")
                }
                BinaryOp::Nullish => untranslatable("`??` as a condition"),
            },
            Ast::Cond { test, cons, alt } => Ok(QExpr::Case {
                operand: None,
                arms: vec![sc_query::CaseArm {
                    when: self.predicate(test)?,
                    then: self.predicate(cons)?,
                }],
                else_result: Some(Box::new(self.predicate(alt)?)),
            }),
            Ast::Ident(name) if name == "user" => Ok(self.user_truthy()),
            Ast::Member { obj, prop, .. } if is_user(obj) => match prop {
                MemberProp::Static(field) => self.user_field_truthy(field),
                MemberProp::Computed(_) => untranslatable("computed access on `user`"),
            },
            Ast::Ident(_) => untranslatable(
                "a bare value as a condition (write an explicit comparison, \
                 e.g. `x === true` or `x !== null`)",
            ),
            other => untranslatable(format!("{} as a condition", describe(other))),
        }
    }

    /// `a === b` / `a !== b` (and their loose spellings) — JavaScript's
    /// two-valued equality, which in SQL is `IS [NOT] DISTINCT FROM`, with the
    /// null-literal and `user` cases given their direct forms.
    fn equality(&mut self, l: &Ast, r: &Ast, negated: bool) -> Result<QExpr, TranslateError> {
        let (null_op, distinct_op) = if negated {
            (QUnOp::IsNotNull, QBinOp::IsDistinct)
        } else {
            (QUnOp::IsNull, QBinOp::IsNotDistinct)
        };
        match (l, r) {
            // `null === null` is true; folded rather than sent to SQL.
            (Ast::Null, Ast::Null) => Ok(QExpr::lit(!negated)),
            (Ast::Null, other) | (other, Ast::Null) => {
                if is_user(other) {
                    // `user === null`: "not logged in", in each env's terms.
                    let is_null = self.user_is_null();
                    return Ok(if negated {
                        QExpr::unary(QUnOp::Not, is_null)
                    } else {
                        is_null
                    });
                }
                Ok(QExpr::unary(null_op, self.value(other)?))
            }
            _ if is_user(l) || is_user(r) => {
                untranslatable("comparing the `user` object itself (compare one of its fields)")
            }
            _ => Ok(QExpr::binary(distinct_op, self.value(l)?, self.value(r)?)),
        }
    }

    /// Ordered comparisons translate operand-for-operand and keep SQL's null
    /// semantics: a null operand yields SQL `NULL`, which grants nothing.
    fn comparison(&mut self, op: QBinOp, l: &Ast, r: &Ast) -> Result<QExpr, TranslateError> {
        Ok(QExpr::binary(op, self.value(l)?, self.value(r)?))
    }

    // ---- values ------------------------------------------------------------

    /// Translate `ast` in value position.
    fn value(&mut self, ast: &Ast) -> Result<QExpr, TranslateError> {
        match ast {
            Ast::Str(s) => Ok(QExpr::lit(s.as_str())),
            Ast::Num(n) => Ok(QExpr::Lit(num_value(*n))),
            Ast::Bool(b) => Ok(QExpr::lit(*b)),
            Ast::Null => Ok(QExpr::Lit(Value::Null)),
            Ast::Ident(name) => self.ident_value(name),
            Ast::Member { obj, prop, .. } if is_user(obj) => match prop {
                MemberProp::Static(field) => Ok(self.user_field(field)),
                MemberProp::Computed(_) => untranslatable("computed access on `user`"),
            },
            Ast::Member { .. } => untranslatable("property access on something other than `user`"),
            Ast::Unary { op, expr } => match op {
                UnaryOp::Neg => Ok(QExpr::unary(QUnOp::Neg, self.value(expr)?)),
                UnaryOp::Not => Ok(QExpr::unary(QUnOp::Not, self.predicate(expr)?)),
                UnaryOp::Pos => untranslatable("unary `+` (numeric coercion)"),
                UnaryOp::TypeOf => untranslatable("`typeof`"),
            },
            Ast::Binary { op, l, r } => match op {
                BinaryOp::Add => self.arithmetic(QBinOp::Add, l, r),
                BinaryOp::Sub => self.arithmetic(QBinOp::Sub, l, r),
                BinaryOp::Mul => self.arithmetic(QBinOp::Mul, l, r),
                BinaryOp::Div => self.arithmetic(QBinOp::Div, l, r),
                BinaryOp::Mod => self.arithmetic(QBinOp::Mod, l, r),
                // JS `??` on SQL values is COALESCE exactly (there is no
                // `undefined` on this side of the boundary).
                BinaryOp::Nullish => Ok(QExpr::Func {
                    name: "COALESCE".into(),
                    args: vec![self.value(l)?, self.value(r)?],
                }),
                // A comparison used as a value is a boolean value in both
                // worlds; `&&`/`||` are not (JS returns an operand).
                BinaryOp::Eq
                | BinaryOp::StrictEq
                | BinaryOp::NotEq
                | BinaryOp::StrictNotEq
                | BinaryOp::Lt
                | BinaryOp::LtEq
                | BinaryOp::Gt
                | BinaryOp::GtEq => self.predicate(ast),
                BinaryOp::And | BinaryOp::Or => untranslatable(
                    "`&&`/`||` as a value (JavaScript yields an operand, SQL a boolean)",
                ),
            },
            Ast::Cond { test, cons, alt } => Ok(QExpr::Case {
                operand: None,
                arms: vec![sc_query::CaseArm {
                    when: self.predicate(test)?,
                    then: self.value(cons)?,
                }],
                else_result: Some(Box::new(self.value(alt)?)),
            }),
            other => untranslatable(describe(other)),
        }
    }

    /// An identifier in value position: a field, a Ⱶ-join path, or a refusal
    /// that says which kind.
    fn ident_value(&mut self, name: &str) -> Result<QExpr, TranslateError> {
        if name == "user" {
            return untranslatable(
                "the `user` object itself has no SQL value (compare one of its fields)",
            );
        }
        if let Some(table_shape) = self.shape.tables.get(self.table)
            && table_shape.fields.contains_key(name)
        {
            return Ok(QExpr::qcol(self.table, name));
        }
        if name.contains(JOIN) {
            return self.join_value(name);
        }
        if GLOBALS.contains(&name) {
            return untranslatable(format!("the JavaScript global `{name}`"));
        }
        Err(TranslateError::Error(Error::invalid(format!(
            "formula on `{}`: unknown identifier `{name}`",
            self.table
        ))))
    }

    fn arithmetic(&mut self, op: QBinOp, l: &Ast, r: &Ast) -> Result<QExpr, TranslateError> {
        Ok(QExpr::binary(op, self.value(l)?, self.value(r)?))
    }

    // ---- Ⱶ-join paths ------------------------------------------------------

    /// A Ⱶ-identifier as a value: nested correlated scalar subselects, one per
    /// link. `publisherⱵname` on `books` becomes
    /// `(SELECT _sc_j1.name FROM publishers AS _sc_j1
    ///    WHERE _sc_j1.id = books.publisher)`;
    /// a null foreign key selects no row, the subquery yields SQL `NULL`, and
    /// nothing is granted — which *is* the Ⱶ optional-chaining contract, for
    /// free.
    fn join_value(&mut self, ident: &str) -> Result<QExpr, TranslateError> {
        let segments: Vec<&str> = ident.split(JOIN).collect();
        if segments.iter().any(|s| s.is_empty()) {
            return Err(self.path_error(ident, "empty segment around Ⱶ"));
        }
        let mut current = self.table.to_string();
        let mut expr: Option<QExpr> = None;
        for (i, segment) in segments.iter().enumerate() {
            let last = i == segments.len() - 1;
            let table_shape = self.shape.tables.get(&current).ok_or_else(|| {
                self.path_error(
                    ident,
                    &format!("table `{current}` is not in the schema shape"),
                )
            })?;
            let field = table_shape.fields.get(*segment).ok_or_else(|| {
                self.path_error(
                    ident,
                    &format!("`{segment}` is not a field of table `{current}`"),
                )
            })?;
            if last {
                break;
            }
            let Some(key) = &field.key else {
                return Err(self.path_error(
                    ident,
                    &format!("`{segment}` is not a Key field on `{current}`"),
                ));
            };
            // The correlation value: the root table's own FK column for the
            // first link, the previous subquery for every later one.
            let corr = match expr.take() {
                None => QExpr::qcol(self.table, *segment),
                Some(prev) => prev,
            };
            self.aliases += 1;
            let alias = format!("_sc_j{}", self.aliases);
            let next = segments[i + 1];
            let sub = Select::from(Source::table_as(key.target_table.clone(), alias.clone()))
                .columns(vec![Projection::expr(QExpr::qcol(alias.clone(), next))])
                .filter(QExpr::binary(
                    QBinOp::Eq,
                    QExpr::qcol(alias, key.target_field.clone()),
                    corr,
                ));
            expr = Some(QExpr::Subquery(Box::new(sub)));
            current = key.target_table.clone();
        }
        expr.ok_or_else(|| {
            // A Ⱶ-identifier with a single segment cannot exist (`split` on a
            // contained char yields ≥ 2 parts), so this is unreachable — but
            // an error beats a panic if that invariant ever shifts.
            self.path_error(ident, "join path has no links")
        })
    }

    fn path_error(&self, ident: &str, msg: &str) -> TranslateError {
        TranslateError::Error(Error::invalid(format!(
            "formula on `{}`: `{ident}`: {msg}",
            self.table
        )))
    }

    // ---- the user environment ---------------------------------------------

    /// Bare `user` as a condition: object-or-null, so truthy ⇔ logged in.
    fn user_truthy(&self) -> QExpr {
        match self.env {
            UserEnv::Inline(user) => QExpr::lit(user.is_some()),
            UserEnv::Guc { .. } => QExpr::unary(QUnOp::IsNotNull, guc_raw()),
        }
    }

    /// `user === null`.
    fn user_is_null(&self) -> QExpr {
        match self.env {
            UserEnv::Inline(user) => QExpr::lit(user.is_none()),
            UserEnv::Guc { .. } => QExpr::unary(QUnOp::IsNull, guc_raw()),
        }
    }

    /// `user.x` as a value.
    fn user_field(&self, field: &str) -> QExpr {
        match self.env {
            UserEnv::Inline(user) => {
                let value = user
                    .as_ref()
                    .and_then(|u| u.get(field).cloned())
                    .unwrap_or(Value::Null);
                QExpr::Lit(value)
            }
            UserEnv::Guc { field_types } => {
                let text = QExpr::Func {
                    name: "jsonb_extract_path_text".into(),
                    args: vec![
                        QExpr::Cast {
                            expr: Box::new(guc_raw()),
                            type_name: "jsonb".into(),
                        },
                        QExpr::lit(field),
                    ],
                };
                match field_types.get(field).map(String::as_str) {
                    None | Some("text") => text,
                    Some(type_name) => QExpr::Cast {
                        expr: Box::new(text),
                        type_name: type_name.into(),
                    },
                }
            }
        }
    }

    /// Bare `user.x` as a condition. Translatable exactly when the truthiness
    /// is knowable: inline, the actual value decides; in GUC mode only a field
    /// the env declares boolean (null-safe `IS NOT DISTINCT FROM TRUE`, so a
    /// missing user or field is false, as JS truthiness of `undefined` is).
    fn user_field_truthy(&self, field: &str) -> Result<QExpr, TranslateError> {
        match self.env {
            UserEnv::Inline(user) => {
                let truthy = user
                    .as_ref()
                    .and_then(|u| u.get(field))
                    .is_some_and(js_truthy);
                Ok(QExpr::lit(truthy))
            }
            UserEnv::Guc { field_types } => match field_types.get(field).map(String::as_str) {
                Some("boolean") | Some("bool") => Ok(QExpr::binary(
                    QBinOp::IsNotDistinct,
                    self.user_field(field),
                    QExpr::lit(true),
                )),
                _ => untranslatable(format!(
                    "`user.{field}` as a condition (only boolean user fields \
                     are; write an explicit comparison)"
                )),
            },
        }
    }
}

/// `current_setting('sc.user', true)` — `true` is `missing_ok`, so an unset
/// GUC is SQL `NULL` rather than an error, and everything built on it fails
/// closed.
fn guc_raw() -> QExpr {
    QExpr::Func {
        name: "current_setting".into(),
        args: vec![QExpr::lit(USER_GUC), QExpr::lit(true)],
    }
}

fn is_user(ast: &Ast) -> bool {
    matches!(ast, Ast::Ident(name) if name == "user")
}

/// A JS number literal as a SQL value: integral f64s (the common case — row
/// ids, role numbers) become `Int` so they compare cleanly with integer
/// columns; everything else stays `Float`.
fn num_value(n: f64) -> Value {
    const MAX_EXACT_INT: f64 = 9_007_199_254_740_992.0; // 2^53
    if n.fract() == 0.0 && n.abs() <= MAX_EXACT_INT {
        Value::Int(n as i64)
    } else {
        Value::Float(n)
    }
}

/// JavaScript truthiness of a SQL value — used only in [`UserEnv::Inline`],
/// where the actual value is at hand.
fn js_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Int(i) => *i != 0,
        Value::Float(f) => *f != 0.0 && !f.is_nan(),
        Value::Text(s) => !s.is_empty(),
        Value::Decimal(d) => !d.is_zero(),
        Value::Json(j) => match j {
            serde_json::Value::Null => false,
            serde_json::Value::Bool(b) => *b,
            serde_json::Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
            serde_json::Value::String(s) => !s.is_empty(),
            // Arrays and objects are objects: truthy, even when empty.
            _ => true,
        },
        // Bytes, UUIDs and temporal values reach JS as objects or non-empty
        // strings: truthy.
        Value::Bytes(_)
        | Value::Uuid(_)
        | Value::Date(_)
        | Value::Time(_)
        | Value::Timestamp(_) => true,
    }
}

/// A human name for an AST shape, for untranslatable-messages.
fn describe(ast: &Ast) -> &'static str {
    match ast {
        Ast::Call { .. } => "a function call",
        Ast::Array(_) => "an array literal",
        Ast::Template { .. } => "a template literal",
        Ast::Arrow { .. } => "an arrow function",
        Ast::Member { .. } => "property access",
        Ast::Ident(_) => "an identifier",
        Ast::Str(_) | Ast::Num(_) | Ast::Bool(_) | Ast::Null => "a literal",
        Ast::Unary { .. } => "a unary operation",
        Ast::Binary { .. } => "a binary operation",
        Ast::Cond { .. } => "a conditional",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::TableShape;
    use sc_query::{SqlDialect, Statement};

    /// Postgres-flavoured rendering, as in `sc-query`'s own tests: the golden
    /// strings below are the exact WHERE clauses Phase 5 injects and Phase 6
    /// bakes into policies.
    struct Pg;

    impl SqlDialect for Pg {
        fn quote_ident(&self, ident: &str) -> String {
            format!("\"{}\"", ident.replace('"', "\"\""))
        }
        fn placeholder(&self, position: usize) -> String {
            format!("${position}")
        }
    }

    /// books(id, title, pages, owner, publisher→publishers.id);
    /// publishers(id, name, country→countries.code); countries(code, name).
    fn shape() -> SchemaShape {
        SchemaShape::new()
            .table(
                "books",
                TableShape::new()
                    .field("id")
                    .field("title")
                    .field("pages")
                    .field("owner")
                    .key_field("publisher", "publishers", "id"),
            )
            .table(
                "publishers",
                TableShape::new().field("id").field("name").key_field(
                    "country",
                    "countries",
                    "code",
                ),
            )
            .table("countries", TableShape::new().field("code").field("name"))
            .user_fields(["id", "role", "email", "is_admin"])
    }

    fn inline_user(fields: &[(&str, Value)]) -> UserEnv {
        UserEnv::Inline(Some(
            fields
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        ))
    }

    fn guc(types: &[(&str, &str)]) -> UserEnv {
        UserEnv::Guc {
            field_types: types
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    /// Translate `src` on `books` and render it as the WHERE clause of a
    /// `SELECT * FROM books`, returning the clause text and its binds.
    fn where_sql(src: &str, op: Operation, env: &UserEnv) -> (String, Vec<Value>) {
        let formula = Formula::parse(src).unwrap();
        let pred = translate(&formula, op, env, &shape(), "books").unwrap();
        let stmt: Statement = Select::from(Source::table("books")).filter(pred).into();
        let (sql, binds) = Pg.render(&stmt).unwrap();
        let clause = sql
            .strip_prefix("SELECT * FROM \"books\" WHERE ")
            .unwrap_or_else(|| panic!("unexpected statement shape: {sql}"))
            .to_string();
        (clause, binds)
    }

    fn err_of(src: &str, op: Operation, env: &UserEnv) -> TranslateError {
        let formula = Formula::parse(src).unwrap();
        translate(&formula, op, env, &shape(), "books").unwrap_err()
    }

    #[test]
    fn inline_equality_is_null_safe_and_binds_the_users_value() {
        let env = inline_user(&[("id", Value::Text("u1".into()))]);
        let (sql, binds) = where_sql("owner === user.id", Operation::Read, &env);
        assert_eq!(sql, "(\"books\".\"owner\" IS NOT DISTINCT FROM $1)");
        assert_eq!(binds, vec![Value::Text("u1".into())]);
    }

    #[test]
    fn guc_equality_extracts_from_the_setting_and_casts() {
        let env = guc(&[("id", "uuid")]);
        let (sql, binds) = where_sql("owner === user.id", Operation::Read, &env);
        assert_eq!(
            sql,
            "(\"books\".\"owner\" IS NOT DISTINCT FROM \
             CAST(jsonb_extract_path_text(CAST(current_setting($1, $2) AS jsonb), $3) AS uuid))"
        );
        assert_eq!(
            binds,
            vec![
                Value::Text(USER_GUC.into()),
                Value::Bool(true),
                Value::Text("id".into()),
            ]
        );
    }

    #[test]
    fn a_text_user_field_is_not_cast() {
        let env = guc(&[("email", "text")]);
        let (sql, _) = where_sql("owner === user.email", Operation::Read, &env);
        assert!(!sql.contains("AS text"), "needless cast in: {sql}");
        assert!(sql.contains("jsonb_extract_path_text"), "got: {sql}");
    }

    #[test]
    fn flags_fold_per_operation() {
        let env = inline_user(&[("id", Value::Text("u1".into()))]);
        // `_read || …` is TRUE for a select — the whole formula folds away…
        let (sql, binds) = where_sql("_read || owner === user.id", Operation::Read, &env);
        assert_eq!(sql, "$1");
        assert_eq!(binds, vec![Value::Bool(true)]);
        // …and exactly the ownership clause for a write.
        let (sql, _) = where_sql("_read || owner === user.id", Operation::Update, &env);
        assert_eq!(sql, "(\"books\".\"owner\" IS NOT DISTINCT FROM $1)");
        // `_write && …` is FALSE for a select.
        let (sql, binds) = where_sql("_write && owner === user.id", Operation::Read, &env);
        assert_eq!(sql, "$1");
        assert_eq!(binds, vec![Value::Bool(false)]);
        // Each write flag reaches its own operation.
        for (op, expect) in [
            (Operation::Insert, true),
            (Operation::Update, false),
            (Operation::Delete, false),
        ] {
            let (_, binds) = where_sql("_insert", op, &env);
            assert_eq!(binds, vec![Value::Bool(expect)], "{op:?}");
        }
    }

    #[test]
    fn a_join_path_becomes_a_correlated_scalar_subselect() {
        let env = UserEnv::Inline(None);
        let (sql, binds) = where_sql("publisherⱵname === 'ACME'", Operation::Read, &env);
        assert_eq!(
            sql,
            "((SELECT \"_sc_j1\".\"name\" FROM \"publishers\" AS \"_sc_j1\" \
             WHERE (\"_sc_j1\".\"id\" = \"books\".\"publisher\")) IS NOT DISTINCT FROM $1)"
        );
        assert_eq!(binds, vec![Value::Text("ACME".into())]);
    }

    #[test]
    fn a_chained_join_path_nests_one_subselect_per_link() {
        let env = UserEnv::Inline(None);
        let (sql, _) = where_sql("publisherⱵcountryⱵname === 'DK'", Operation::Read, &env);
        assert_eq!(
            sql,
            "((SELECT \"_sc_j2\".\"name\" FROM \"countries\" AS \"_sc_j2\" WHERE \
             (\"_sc_j2\".\"code\" = \
             (SELECT \"_sc_j1\".\"country\" FROM \"publishers\" AS \"_sc_j1\" \
             WHERE (\"_sc_j1\".\"id\" = \"books\".\"publisher\")))) IS NOT DISTINCT FROM $1)"
        );
    }

    #[test]
    fn null_literal_comparisons_use_is_null() {
        let env = UserEnv::Inline(None);
        let (sql, _) = where_sql("owner === null", Operation::Read, &env);
        assert_eq!(sql, "(\"books\".\"owner\" IS NULL)");
        let (sql, _) = where_sql("owner !== null", Operation::Read, &env);
        assert_eq!(sql, "(\"books\".\"owner\" IS NOT NULL)");
        let (sql, binds) = where_sql("null === null", Operation::Read, &env);
        assert_eq!(sql, "$1");
        assert_eq!(binds, vec![Value::Bool(true)]);
    }

    #[test]
    fn user_null_checks_translate_per_env() {
        let (sql, binds) = where_sql("user === null", Operation::Read, &UserEnv::Inline(None));
        assert_eq!((sql.as_str(), binds), ("$1", vec![Value::Bool(true)]));
        let logged_in = inline_user(&[]);
        let (_, binds) = where_sql("user === null", Operation::Read, &logged_in);
        assert_eq!(binds, vec![Value::Bool(false)]);
        let (sql, _) = where_sql("user === null", Operation::Read, &guc(&[]));
        assert_eq!(sql, "(current_setting($1, $2) IS NULL)");
        // Bare `user` as a condition is the logged-in test.
        let (sql, _) = where_sql(
            "user && owner === user.id",
            Operation::Read,
            &guc(&[("id", "uuid")]),
        );
        assert!(
            sql.starts_with("((current_setting($1, $2) IS NOT NULL) AND "),
            "got: {sql}"
        );
    }

    #[test]
    fn an_anonymous_users_field_is_null_which_matches_null() {
        // The documented corner of the two-valued semantics: with no user,
        // `user.id` is null, and `owner === user.id` therefore *matches rows
        // whose owner is null* (`null === null` is true in JS and in
        // IS NOT DISTINCT FROM alike). A formula that must not grant
        // anonymously writes `user && …` — bare `user` is object-or-null, so
        // its truthiness is exactly the logged-in test.
        let (sql, binds) = where_sql("owner === user.id", Operation::Read, &UserEnv::Inline(None));
        assert_eq!(sql, "(\"books\".\"owner\" IS NOT DISTINCT FROM $1)");
        assert_eq!(binds, vec![Value::Null]);
        // The guard in action: anonymously, the whole formula folds to FALSE.
        let (sql, binds) = where_sql(
            "user && owner === user.id",
            Operation::Read,
            &UserEnv::Inline(None),
        );
        assert_eq!(
            (sql.as_str(), binds),
            (
                "($1 AND (\"books\".\"owner\" IS NOT DISTINCT FROM $2))",
                vec![Value::Bool(false), Value::Null]
            )
        );
    }

    #[test]
    fn ordered_comparisons_keep_sql_null_semantics() {
        let env = UserEnv::Inline(None);
        let (sql, binds) = where_sql("pages >= 100", Operation::Read, &env);
        assert_eq!(sql, "(\"books\".\"pages\" >= $1)");
        assert_eq!(binds, vec![Value::Int(100)]);
    }

    #[test]
    fn integral_numbers_bind_as_int_and_fractional_as_float() {
        let env = UserEnv::Inline(None);
        let (_, binds) = where_sql("pages === 3", Operation::Read, &env);
        assert_eq!(binds, vec![Value::Int(3)]);
        let (_, binds) = where_sql("pages === 3.5", Operation::Read, &env);
        assert_eq!(binds, vec![Value::Float(3.5)]);
    }

    #[test]
    fn nullish_coalescing_is_coalesce() {
        let env = UserEnv::Inline(None);
        let (sql, binds) = where_sql("(title ?? 'anon') === 'x'", Operation::Read, &env);
        assert_eq!(
            sql,
            "(COALESCE(\"books\".\"title\", $1) IS NOT DISTINCT FROM $2)"
        );
        assert_eq!(
            binds,
            vec![Value::Text("anon".into()), Value::Text("x".into())]
        );
    }

    #[test]
    fn a_conditional_translates_to_a_searched_case() {
        let env = inline_user(&[("id", Value::Text("u1".into()))]);
        let (sql, _) = where_sql(
            "title === 'wiki' ? true : owner === user.id",
            Operation::Read,
            &env,
        );
        assert_eq!(
            sql,
            "CASE WHEN (\"books\".\"title\" IS NOT DISTINCT FROM $1) THEN $2 \
             ELSE (\"books\".\"owner\" IS NOT DISTINCT FROM $3) END"
        );
    }

    #[test]
    fn a_boolean_user_field_is_a_condition_in_both_envs() {
        // Inline: the actual value decides.
        let env = inline_user(&[("is_admin", Value::Bool(true))]);
        let (_, binds) = where_sql("user.is_admin", Operation::Read, &env);
        assert_eq!(binds, vec![Value::Bool(true)]);
        // GUC: declared boolean → null-safe IS NOT DISTINCT FROM TRUE.
        let env = guc(&[("is_admin", "boolean")]);
        let (sql, _) = where_sql("user.is_admin", Operation::Read, &env);
        assert!(sql.contains("IS NOT DISTINCT FROM"), "got: {sql}");
        // GUC: a non-boolean user field's truthiness is not knowable.
        let env = guc(&[("email", "text")]);
        let err = err_of("user.email && owner === user.id", Operation::Read, &env);
        assert!(
            matches!(err, TranslateError::Untranslatable(_)),
            "got: {err}"
        );
    }

    #[test]
    fn untranslatable_constructs_are_named() {
        let env = UserEnv::Inline(None);
        for (src, names) in [
            ("title.includes('x')", "a function call"),
            ("`${title}!` === title", "a template literal"),
            ("owner && true", "a bare value as a condition"),
            ("Math.random() > 0.5", "a function call"),
            (
                "publisher.name === 'x'",
                "property access on something other",
            ),
            ("owner === user[title]", "computed access on `user`"),
            ("title === undefined", "the JavaScript global `undefined`"),
            ("owner === (title && owner)", "`&&`/`||` as a value"),
            ("pages + 1", "arithmetic as a condition"),
            ("owner === user", "comparing the `user` object itself"),
        ] {
            let err = err_of(src, Operation::Read, &env);
            let TranslateError::Untranslatable(msg) = &err else {
                panic!("{src}: expected Untranslatable, got {err:?}");
            };
            assert!(msg.contains(names), "{src}: expected `{names}` in: {msg}");
        }
    }

    #[test]
    fn arithmetic_translates_in_value_position() {
        let env = UserEnv::Inline(None);
        let (sql, binds) = where_sql("pages % 2 === 0", Operation::Read, &env);
        assert_eq!(sql, "((\"books\".\"pages\" % $1) IS NOT DISTINCT FROM $2)");
        assert_eq!(binds, vec![Value::Int(2), Value::Int(0)]);
    }

    #[test]
    fn an_unknown_identifier_is_an_error_not_a_fallback() {
        // Phase 5 falls back to the reified evaluator on Untranslatable; a
        // typo must not ride that path into V8 and "work" by throwing.
        let err = err_of("writer === 1", Operation::Read, &UserEnv::Inline(None));
        let TranslateError::Error(e) = err else {
            panic!("expected Error, got {err:?}");
        };
        assert!(
            e.to_string().contains("unknown identifier `writer`"),
            "got: {e}"
        );
    }

    #[test]
    fn translate_error_converts_to_workspace_error() {
        let err = err_of(
            "title.includes('x')",
            Operation::Read,
            &UserEnv::Inline(None),
        );
        let e: Error = err.into();
        assert!(matches!(e.repr(), sc_error::Repr::Invalid(_)));
        assert!(e.to_string().contains("cannot be translated"), "got: {e}");
    }
}
