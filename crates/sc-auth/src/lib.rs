//! User, Role, sessions, authentication (layer 5).
//!
//! The MVP fills this in incrementally (TODO Phase 5). So far: the `users` table
//! schema and its one-time [`bootstrap`] into the [`Catalog`](sc_catalog::Catalog),
//! and the [`User`] value read back from a row.

mod user;
mod users;

pub use user::User;
pub use users::{
    COL_EMAIL, COL_ID, COL_PASSWORD_HASH, COL_ROLE, ROLE_ADMIN, ROLE_PUBLIC, USERS_TABLE,
    bootstrap, role_in_range,
};
