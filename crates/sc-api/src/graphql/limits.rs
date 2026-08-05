//! What one GraphQL operation is allowed to cost.
//!
//! REST's cost is bounded by its shape: a route is a query, and a route nobody
//! wrote cannot be asked for. A GraphQL endpoint is the opposite — the caller
//! writes the query — so "an arbitrary query language is an arbitrary cost"
//! unless something says otherwise. This module is that something, and it is
//! four numbers rather than one because a query can be expensive in four
//! unrelated ways:
//!
//! - **Depth**: `a { b { a { b { … } } } }` over two tables that reference each
//!   other is unbounded nesting from a document of a few hundred bytes.
//! - **Complexity**: a wide document — every column of every table, once per
//!   alias — is shallow and still enormous. `async-graphql` counts one per
//!   field, aliases included, which is exactly the right unit here.
//! - **Rows**: a list field with no `limit` is a request to stream a table into
//!   a response, so an absent `limit` *becomes* the cap and a present one is
//!   clamped to it (see [`args`](super::args)).
//! - **Statements**: depth and complexity are counted before execution, over
//!   the document; the number of *database round trips* is not a property of
//!   the document alone, because one child list field costs one statement per
//!   level however many parents asked for it. So it is counted as it is spent
//!   ([`StatementBudget`]). The unit is one read or one write **the provider
//!   issues** — what the row layer expands a write into is the row layer's
//!   business, and one mutation the caller wrote is one thing they asked for.
//!
//! The first two are `async-graphql`'s own validation, which runs **before any
//! resolver does** — a too-deep or too-expensive query is refused without a
//! statement being issued, which is the property worth having. The last two are
//! this provider's, enforced where the reads are.
//!
//! **Introspection stays on.** It is not a secret being kept: the SDL is served
//! at `{mount}/schema.graphql`, the schema describes tables the application
//! already exposes over REST, and the admin explorer (Phase 9) and every
//! GraphQL tool a developer owns are built on it. The defaults below are chosen
//! to admit the standard introspection query, which is deeper than any data
//! query anybody writes by hand.

use std::sync::atomic::{AtomicUsize, Ordering};

use async_graphql::ServerError;
use sc_error::{Error, Result};

/// The most rows a list field yields when the caller names no `limit`, and the
/// ceiling a `limit` they *do* name is clamped to.
pub const DEFAULT_ROW_CAP: u64 = 500;

/// The deepest field nesting an operation may have.
///
/// A data query at this depth has walked seven relations, which nothing written
/// on purpose does. The number is what it is because of *introspection*: the
/// standard `getIntrospectionQuery` unrolls `ofType` seven times under
/// `__schema { types { fields { type { … } } } }`, reaching depth 12, and a
/// default that refused a tool's first request would be a default nobody could
/// keep.
pub const DEFAULT_MAX_DEPTH: usize = 15;

/// The most fields an operation may name, counted once per selection —
/// `async-graphql`'s complexity, with a field's aliases counted separately
/// because each one is answered separately.
///
/// Comfortably above both the introspection query and any hand-written document
/// over a normal application, and far below a generated one that asks for every
/// column of every table a hundred times.
pub const DEFAULT_MAX_COMPLEXITY: usize = 2_000;

/// The most reads and writes one operation may issue.
///
/// The provider's whole design is one statement per *level* — a root read, one
/// batched read per child relation per level, one per root aggregate, one per
/// mutation and its read-back — so a reasonable document spends a handful. A
/// document that spends thirty has found a shape this design answers badly, and
/// the honest response is to say so rather than to keep going.
pub const DEFAULT_STATEMENT_BUDGET: usize = 32;

/// The four bounds one application's GraphQL API is served under.
///
/// Per application because applications differ: a public read API and an
/// internal reporting one want different numbers, and there is no number that
/// is right for both. The defaults are *set* rather than absent — an unbounded
/// GraphQL endpoint is not a configuration, it is an oversight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphqlLimits {
    /// The deepest field nesting an operation may have.
    pub max_depth: usize,
    /// The most fields an operation may name.
    pub max_complexity: usize,
    /// The bound a list field takes when the caller names none, and the ceiling
    /// one they name is clamped to.
    pub row_cap: u64,
    /// The most reads and writes one operation may issue.
    pub statement_budget: usize,
}

impl Default for GraphqlLimits {
    fn default() -> GraphqlLimits {
        GraphqlLimits {
            max_depth: DEFAULT_MAX_DEPTH,
            max_complexity: DEFAULT_MAX_COMPLEXITY,
            row_cap: DEFAULT_ROW_CAP,
            statement_budget: DEFAULT_STATEMENT_BUDGET,
        }
    }
}

impl GraphqlLimits {
    /// The defaults.
    pub fn new() -> GraphqlLimits {
        GraphqlLimits::default()
    }

    /// The deepest nesting an operation may have.
    pub fn max_depth(mut self, depth: usize) -> GraphqlLimits {
        self.max_depth = depth;
        self
    }

    /// The most fields an operation may name.
    pub fn max_complexity(mut self, complexity: usize) -> GraphqlLimits {
        self.max_complexity = complexity;
        self
    }

    /// The row cap a list field is bounded by.
    pub fn row_cap(mut self, cap: u64) -> GraphqlLimits {
        self.row_cap = cap;
        self
    }

    /// The most reads and writes one operation may issue.
    pub fn statement_budget(mut self, statements: usize) -> GraphqlLimits {
        self.statement_budget = statements;
        self
    }
}

/// What `async-graphql` says when its depth rule refuses a document.
const TOO_DEEP: &str = "Query is nested too deep.";

/// …and its complexity rule.
const TOO_COMPLEX: &str = "Query is too complex.";

/// Rewrite a depth or complexity refusal to name the bound it hit.
///
/// `async-graphql`'s own messages are "Query is nested too deep." and "Query is
/// too complex." — true, and useless to the person holding the query, who
/// cannot tell how deep is too deep or by how much they overshot. The bound is
/// this application's configuration, so this is the only place that knows it.
///
/// Matching on the library's message text is admittedly a seam. It is a narrow
/// one: the two strings are constants in `async_graphql::validation`, an
/// unrecognised message is left exactly as it was, and the worst a version bump
/// can do is give the caller back the terser sentence.
pub fn name_the_bound(errors: &mut [ServerError], limits: GraphqlLimits) {
    for error in errors {
        if error.message == TOO_DEEP {
            error.message = format!(
                "this query nests fields more than {} levels deep, which is the most this \
                 application allows a GraphQL request; ask for fewer levels of related rows \
                 in one query",
                limits.max_depth
            );
        } else if error.message == TOO_COMPLEX {
            error.message = format!(
                "this query names more than the {} fields this application allows a GraphQL \
                 request (every alias of a field counts as one); ask for fewer, or split it",
                limits.max_complexity
            );
        }
    }
}

/// The statements one request has left to spend.
///
/// Counted rather than predicted, because the count is not a property of the
/// document: `departments { employees { tasks { … } } }` is three statements
/// whether it returns three rows or three thousand, but a document whose
/// aliases ask for the same relation under six different `where` arguments is
/// six, and only execution knows which it is.
///
/// One per request, shared by every resolver and by the child loader — the
/// budget is the *operation's*, not a field's.
#[derive(Debug)]
pub struct StatementBudget {
    limit: usize,
    spent: AtomicUsize,
}

impl StatementBudget {
    /// A budget of `limit` statements.
    pub fn new(limit: usize) -> StatementBudget {
        StatementBudget {
            limit,
            spent: AtomicUsize::new(0),
        }
    }

    /// Charge one read or write about to be issued against `what`, or refuse.
    ///
    /// Charged *before* the statement runs, so the refusal replaces it rather
    /// than following it: the point of a budget is the work not done.
    pub fn charge(&self, what: &str) -> Result<()> {
        // `fetch_add` rather than a load-then-store: resolvers run concurrently,
        // and two fields that both saw "one left" would both spend it.
        let spent = self.spent.fetch_add(1, Ordering::Relaxed) + 1;
        if spent > self.limit {
            return Err(Error::invalid(format!(
                "this operation would issue more than the {} database reads and writes this \
                 application allows a GraphQL request (`{what}` was the {spent}th); \
                 ask for fewer relations in one query, or split it",
                self.limit
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_admit_the_standard_introspection_query() {
        // The one document every GraphQL tool sends first. Its depth is 12 and
        // its complexity is in the low hundreds; a default that refused it
        // would make "introspection stays on" a fiction.
        let limits = GraphqlLimits::default();
        assert!(limits.max_depth >= 12, "{limits:?}");
        assert!(limits.max_complexity >= 500, "{limits:?}");
    }

    #[test]
    fn a_budget_refuses_the_statement_that_would_exceed_it_not_the_one_after() {
        let budget = StatementBudget::new(2);
        assert!(budget.charge("departments").is_ok());
        assert!(budget.charge("employees").is_ok());
        let err = budget.charge("tasks").expect_err("the third is refused");
        // The message says which read was refused and what the bound is —
        // both of which are what somebody would change.
        let text = format!("{err}");
        assert!(text.contains("tasks"), "{text}");
        assert!(text.contains('2'), "{text}");
        // An exhausted budget stays exhausted: the refusal is not a one-off
        // that the next concurrently-resolving field gets to spend past.
        assert!(budget.charge("notes").is_err());
    }

    #[test]
    fn a_limit_refusal_is_rewritten_to_name_the_bound_and_nothing_else_is() {
        let limits = GraphqlLimits::new().max_depth(4).max_complexity(9);
        let mut errors = vec![
            ServerError::new(TOO_DEEP, None),
            ServerError::new(TOO_COMPLEX, None),
            ServerError::new("no field `nope` on `Departments`", None),
        ];
        name_the_bound(&mut errors, limits);
        assert!(errors[0].message.contains("4 levels deep"), "{errors:?}");
        assert!(errors[1].message.contains("9 fields"), "{errors:?}");
        // A message this function does not recognise is the executor's own and
        // is left alone — the rewrite is a translation, not a filter.
        assert_eq!(errors[2].message, "no field `nope` on `Departments`");
    }

    #[test]
    fn limits_are_built_from_the_defaults_one_at_a_time() {
        let limits = GraphqlLimits::new().row_cap(10).max_depth(4);
        assert_eq!(limits.row_cap, 10);
        assert_eq!(limits.max_depth, 4);
        // Everything not named keeps the default rather than becoming zero,
        // which would be "no statements allowed".
        assert_eq!(limits.statement_budget, DEFAULT_STATEMENT_BUDGET);
        assert_eq!(limits.max_complexity, DEFAULT_MAX_COMPLEXITY);
    }
}
