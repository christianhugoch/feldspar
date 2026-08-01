//! The schema-change seam: how a table or field change becomes something the
//! rest of the process can react to (TODO Phase 7).
//!
//! A **mounted application** built its REST projection from its tables as they
//! were at mount time (§4, §13.2), so a field added or dropped underneath it
//! leaves it serving endpoints for a column that is gone — or missing the ones
//! for a column that arrived — until a restart. The admin handlers used to fix
//! that by calling `AppMounts::refresh_table` themselves, which worked only
//! because every schema change went through a handler. Once an *agent* can edit
//! the schema (§11.3's `manage_table_admin`), that is no longer true.
//!
//! So the notification moves to where the change is made, behind the same shape
//! the write seam already uses ([`TableEvents`](crate::TableEvents)): the mount
//! registry is `sc-server`'s and the schema editor is `sc-api`'s, neither can
//! name the other, and the catalog — which both already hold — owns the seam. A
//! server installs its observer once at boot with
//! [`Catalog::set_schema_observer`](crate::Catalog::set_schema_observer); a
//! process with none (a build tool, a test) simply changes the schema
//! unobserved.

use crate::catalog::Catalog;

/// What changed about a table's shape or configuration.
///
/// Coarse on purpose: every consumer so far reacts to "this table is not what it
/// was" by re-projecting it, and a finer enum would be states nobody branches
/// on. The table's *name* is what a consumer selects on, so that is what every
/// variant carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaChanged {
    /// A table was created.
    TableCreated(String),
    /// A table's settings, fields or access rules changed.
    TableChanged(String),
    /// A table was dropped.
    TableDropped(String),
}

impl SchemaChanged {
    /// The table this is about.
    pub fn table(&self) -> &str {
        match self {
            SchemaChanged::TableCreated(name)
            | SchemaChanged::TableChanged(name)
            | SchemaChanged::TableDropped(name) => name,
        }
    }
}

/// What it means to observe schema changes. `sc-server`'s mount registry is the
/// implementation; the catalog holds it as `dyn`.
pub trait SchemaObserver: Send + Sync {
    /// React to a change that has already been applied.
    ///
    /// Called **after** the DDL committed and the catalog reloaded, so an
    /// observer reads the new schema simply by asking the catalog. An `Err` here
    /// means the *reaction* failed, never the change: the caller reports it
    /// beside the successful result rather than pretending the schema did not
    /// move.
    fn schema_changed(&self, catalog: &Catalog, change: &SchemaChanged) -> sc_error::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_change_names_the_table_it_is_about() {
        assert_eq!(SchemaChanged::TableCreated("a".into()).table(), "a");
        assert_eq!(SchemaChanged::TableChanged("b".into()).table(), "b");
        assert_eq!(SchemaChanged::TableDropped("c".into()).table(), "c");
    }
}
