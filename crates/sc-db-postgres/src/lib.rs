//! Postgres backend for the database layer (layer 2).
//!
//! Implements the `sc-db` contract against a real Postgres server. The first
//! piece is [`PgDialect`], the Postgres [`SqlDialect`](sc_query::SqlDialect) that
//! renders a [`Statement`](sc_query::Statement) to `(sql, binds)` with
//! double-quoted identifiers and `$n` placeholders. Connection/pooling,
//! introspection, schema application, and transactions land in the following
//! Phase 2 items.

mod dialect;

pub use dialect::PgDialect;
