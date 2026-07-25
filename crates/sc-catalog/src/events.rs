//! The emit seam: how a row write becomes something observable (§10.2, Phase 4).
//!
//! Decision 1 of the trigger design is that a table event fires **after** the
//! write succeeds, and that the row layer raises it without knowing whether
//! anything is listening. That needs a seam, and the seam has to run the *other
//! way* from the dependency: `sc-action` is layer 6 and the row layer is layer 8,
//! so the writer cannot call the dispatcher by name.
//!
//! So the catalog — which both of them already hold — owns the seam. It defines
//! what an observable write *is* ([`TableWrite`]) and the one trait a listener
//! implements ([`TableEvents`]); `sc-action`'s dispatcher implements it, and a
//! server installs that once at boot with
//! [`Catalog::set_table_events`](crate::Catalog::set_table_events). The catalog
//! knows **that** writes are observable; `sc-action` knows **what** observing
//! means. Neither knows the other.
//!
//! ## Nothing listening costs nothing
//!
//! GOALS asks that firing an event be a lookup rather than a query, so
//! [`observes`](TableEvents::observes) is a **synchronous predicate** the row
//! layer asks *before* doing any work for the feature. That is what lets an
//! update fetch its pre-image only when something will read it: a deployment with
//! no triggers pays one map lookup per write, not a `SELECT`.

use serde_json::Value as Json;

use sc_error::Result;

use crate::caller::CallerContext;
use crate::catalog::Catalog;
use crate::table::Table;

/// Which of the three observable row operations happened.
///
/// Its own enum rather than [`sc_expr::Operation`], which also has a `Read`: a
/// read is not a write, and a type that cannot say otherwise is one less state to
/// handle at every match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WriteOp {
    /// A row was inserted.
    Insert,
    /// A row was updated.
    Update,
    /// A row was deleted.
    Delete,
}

impl WriteOp {
    /// The lowercase name — the same word the event kind is stored under, so the
    /// two sides of the seam spell it identically.
    pub fn as_str(self) -> &'static str {
        match self {
            WriteOp::Insert => "insert",
            WriteOp::Update => "update",
            WriteOp::Delete => "delete",
        }
    }
}

impl std::fmt::Display for WriteOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One write that has already happened, as a listener sees it.
///
/// Rows travel as **JSON**, not as `Value` maps: that is what an event carries to
/// a formula, to an action's configuration and over the wire to a `fetch`, so
/// converting once here beats converting at each of those.
pub struct TableWrite<'a> {
    /// The table written to.
    pub table: &'a Table,
    /// What happened to it.
    pub op: WriteOp,
    /// The row as it now is — or, for a delete, as it was.
    pub row: Json,
    /// The row as it was before an update; `None` for an insert or a delete
    /// (whose "before" *is* [`row`](TableWrite::row)).
    pub old_row: Option<Json>,
    /// Who caused the write, and which triggers led here. `None` where a caller
    /// was not supplied, which reads as an anonymous, un-chained write.
    pub caller: Option<&'a CallerContext>,
}

/// What it means to observe table writes. `sc-action`'s trigger dispatcher is the
/// implementation; the catalog holds it as `dyn`.
#[async_trait::async_trait]
pub trait TableEvents: Send + Sync {
    /// Whether anything listens for `op` on `table`.
    ///
    /// Must be **cheap and synchronous** — it is called on the write path before
    /// any work is done for the feature, and a write nobody observes must not pay
    /// for one that would be.
    fn observes(&self, table: &str, op: WriteOp) -> bool;

    /// Handle one write that has already committed.
    ///
    /// Called after the statement succeeded, so it cannot veto the write (a
    /// before-commit hook is a different contract, and a later milestone). An
    /// `Err` here means the *dispatch* failed, never the write: the caller logs
    /// it and returns the row.
    async fn emit(&self, catalog: &Catalog, write: TableWrite<'_>) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_write_ops_spell_themselves_as_their_event_kinds() {
        for (op, name) in [
            (WriteOp::Insert, "insert"),
            (WriteOp::Update, "update"),
            (WriteOp::Delete, "delete"),
        ] {
            assert_eq!(op.as_str(), name);
            assert_eq!(op.to_string(), name);
        }
    }
}
