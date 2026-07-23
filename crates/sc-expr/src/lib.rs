//! JavaScript formula expressions for authorization and calculated fields
//! (technical design §7.3; TODO "Ownership Formulae & RLS" Phase 1).
//!
//! An ownership formula is a single JavaScript expression over a row's fields,
//! the current `user`, the operation flags (`_read`, `_insert`, `_update`,
//! `_delete`, `_write`) and Ⱶ-joinfields (`publisherⱵname` — one *identifier*,
//! because Ⱶ is a letter to JavaScript). This crate owns that language:
//!
//! - [`Formula::parse`] runs swc over the source, requires a single pure
//!   expression, and lowers it into the crate's own [`Ast`] — the one tree both
//!   later evaluators consume, which is what makes their parity provable.
//! - [`Formula::validate`] classifies every free variable against a
//!   [`SchemaShape`] (the caller's projection of its tables — this crate sits
//!   below `sc-catalog` and never sees a `Table`), yielding an [`Analysis`] or
//!   an error naming the identifier an admin has to fix.
//!
//! - [`translate`] is the **symbolic evaluator**: it turns a formula into a
//!   boolean `sc_query::Expr` for one [`Operation`] (the flags fold to
//!   constants) under one [`UserEnv`] — `Inline` (user values as literals, for
//!   runtime WHERE injection) or `Guc` (`current_setting('sc.user', true)`
//!   JSON extraction, for RLS policies). What has no SQL counterpart with
//!   matching semantics is a [`TranslateError::Untranslatable`] naming the
//!   construct, which the runtime path answers by falling back to the reified
//!   evaluator — Phase 3, on `deno_core`, consuming this same [`Ast`].

mod analyze;
mod ast;
#[cfg(feature = "eval")]
mod eval;
mod formula;
#[cfg(feature = "eval")]
mod normalise;
mod shape;
mod translate;

pub use analyze::{Analysis, FreeVars, JOIN, JoinPath, OpFlag};
pub use ast::{Ast, BinaryOp, MemberProp, UnaryOp};
#[cfg(feature = "eval")]
pub use eval::{DenoEvaluator, FormulaCall, JsEvaluator};
pub use formula::Formula;
pub use shape::{FieldShape, KeyShape, SchemaShape, TableShape};
pub use translate::{Operation, TranslateError, USER_GUC, UserEnv, translate};
