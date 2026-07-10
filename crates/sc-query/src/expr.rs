//! Scalar expressions ([`Expr`]) used inside a [`Statement`](crate::Statement).
//!
//! An `Expr` is the tree of column references, literals, operators, function
//! calls, `IN` sets, JSON accesses, and `CASE` branches that appear in a
//! `WHERE`/`ON`/projection/`SET` position. Everything is plain, serializable
//! data (technical design §4), so a query can be inspected or reconstructed by a
//! code adapter.
//!
//! Two design requirements shape the tree:
//!
//! - **Composite keys and foreign keys to non-primary-key columns** — a join or
//!   filter condition is an arbitrary [`Expr`] over general [`ColRef`]s, never
//!   "the id column".
//! - **JSON is built in** — [`Expr::Json`] (paired with [`Value::Json`]) is a
//!   first-class access path, not an add-on.
//!
//! Literals live in [`Expr::Lit`] and are **always parameterised on render**;
//! constructing a literal here never interpolates it into SQL text.

use serde::{Deserialize, Serialize};

use crate::Value;

/// A table-qualified column reference.
///
/// `table` is optional because a column may be unambiguous in a single-source
/// query; when a join is present it should be set. The reference is a plain
/// name pair so it can point at any column — including a non-primary-key column
/// that a foreign key targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColRef {
    /// Optional table (or alias) qualifier.
    pub table: Option<String>,
    /// Column name.
    pub column: String,
}

impl ColRef {
    /// A column with no table qualifier.
    pub fn bare(column: impl Into<String>) -> Self {
        ColRef {
            table: None,
            column: column.into(),
        }
    }

    /// A `table.column` reference.
    pub fn qualified(table: impl Into<String>, column: impl Into<String>) -> Self {
        ColRef {
            table: Some(table.into()),
            column: column.into(),
        }
    }
}

/// Binary operators supported by the MVP AST.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BinOp {
    /// `=`
    Eq,
    /// `<>`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// Logical `AND`.
    And,
    /// Logical `OR`.
    Or,
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Mod,
    /// `LIKE`
    Like,
    /// `ILIKE`
    ILike,
    /// String concatenation (`||`).
    Concat,
}

/// Unary operators supported by the MVP AST.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnOp {
    /// Logical `NOT`.
    Not,
    /// Arithmetic negation (`-`).
    Neg,
    /// `IS NULL`.
    IsNull,
    /// `IS NOT NULL`.
    IsNotNull,
}

/// One step of a JSON access path (`Expr::Json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonStep {
    /// Object field access by key.
    Field(String),
    /// Array element access by index.
    Index(i64),
}

/// The right-hand side of an `IN` expression.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InSet {
    /// A literal list of expressions: `IN (a, b, c)`.
    List(Vec<Expr>),
    /// A scalar subquery: `IN (SELECT …)`.
    Subquery(Box<crate::Select>),
}

/// One `WHEN … THEN …` arm of a [`Expr::Case`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseArm {
    /// The `WHEN` condition (a boolean expression, or a value to match against
    /// the case operand when one is given).
    pub when: Expr,
    /// The `THEN` result.
    pub then: Expr,
}

/// A scalar expression tree.
///
/// Kept intentionally minimal — extended only as real queries demand it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Expr {
    /// A (possibly table-qualified) column reference.
    Col(ColRef),
    /// A literal value; always parameterised on render.
    Lit(Value),
    /// A positional bind parameter (0-based index into the caller's binds).
    Param(usize),
    /// A binary operation.
    Binary {
        /// The operator.
        op: BinOp,
        /// Left operand.
        l: Box<Expr>,
        /// Right operand.
        r: Box<Expr>,
    },
    /// A unary operation.
    Unary {
        /// The operator.
        op: UnOp,
        /// Operand.
        e: Box<Expr>,
    },
    /// A function call: `name(args…)`.
    Func {
        /// Function name.
        name: String,
        /// Argument expressions.
        args: Vec<Expr>,
    },
    /// An `IN` test.
    In {
        /// The expression being tested.
        e: Box<Expr>,
        /// The set to test membership against.
        set: InSet,
    },
    /// A JSON access path into `target`.
    Json {
        /// The JSON-typed expression to index into.
        target: Box<Expr>,
        /// The access path.
        path: Vec<JsonStep>,
    },
    /// A `CASE` expression.
    Case {
        /// Optional operand for the simple `CASE operand WHEN v …` form; `None`
        /// for the searched `CASE WHEN cond …` form.
        operand: Option<Box<Expr>>,
        /// The `WHEN … THEN …` arms.
        arms: Vec<CaseArm>,
        /// Optional `ELSE` result.
        else_result: Option<Box<Expr>>,
    },
}

impl Expr {
    /// A bare (unqualified) column reference.
    pub fn col(column: impl Into<String>) -> Self {
        Expr::Col(ColRef::bare(column))
    }

    /// A `table.column` reference.
    pub fn qcol(table: impl Into<String>, column: impl Into<String>) -> Self {
        Expr::Col(ColRef::qualified(table, column))
    }

    /// A literal value.
    pub fn lit(v: impl Into<Value>) -> Self {
        Expr::Lit(v.into())
    }

    /// Build a binary expression from two operands.
    pub fn binary(op: BinOp, l: Expr, r: Expr) -> Self {
        Expr::Binary {
            op,
            l: Box::new(l),
            r: Box::new(r),
        }
    }

    /// Build a unary expression.
    pub fn unary(op: UnOp, e: Expr) -> Self {
        Expr::Unary { op, e: Box::new(e) }
    }

    /// `self = other`.
    pub fn eq(self, other: Expr) -> Self {
        Expr::binary(BinOp::Eq, self, other)
    }

    /// `self AND other`.
    pub fn and(self, other: Expr) -> Self {
        Expr::binary(BinOp::And, self, other)
    }

    /// `self OR other`.
    pub fn or(self, other: Expr) -> Self {
        Expr::binary(BinOp::Or, self, other)
    }
}

/// A literal value becomes an [`Expr::Lit`].
impl From<Value> for Expr {
    fn from(v: Value) -> Self {
        Expr::Lit(v)
    }
}

/// A column reference becomes an [`Expr::Col`].
impl From<ColRef> for Expr {
    fn from(c: ColRef) -> Self {
        Expr::Col(c)
    }
}
