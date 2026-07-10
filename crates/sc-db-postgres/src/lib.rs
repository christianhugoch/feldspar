//! Postgres backend for the database layer (layer 2).
//!
//! Implements the `sc-db` contract against a real Postgres server:
//!
//! - [`PgDialect`] — the Postgres [`SqlDialect`](sc_query::SqlDialect) that
//!   renders a [`Statement`](sc_query::Statement) to `(sql, binds)` with
//!   double-quoted identifiers and `$n` placeholders.
//! - [`PgDriver`] — a pooled connection that renders and runs a statement and
//!   streams rows back as a [`RowStream`](sc_db::RowStream).
//!
//! Introspection, schema application, and transactions (and the full
//! `DatabaseDriver` trait impl) land in the following Phase 2 items.

mod dialect;
mod driver;
mod value;

pub use dialect::PgDialect;
pub use driver::PgDriver;
