//! User, Role, sessions, authentication (layer 5).
//!
//! The MVP fills this in incrementally (TODO Phase 5). So far: the `users` table
//! schema and its one-time [`bootstrap`] into the [`Catalog`](sc_catalog::Catalog),
//! the [`User`] value read back from a row, argon2id password hashing
//! ([`hash_password`]), and the create-first-user flow ([`create_first_user`],
//! [`any_user_exists`]).

mod first_user;
mod password;
mod user;
mod users;

pub use first_user::{any_user_exists, create_first_user};
pub use password::{hash_password, is_valid_hash};
pub use user::User;
pub use users::{
    COL_EMAIL, COL_ID, COL_PASSWORD_HASH, COL_ROLE, ROLE_ADMIN, ROLE_PUBLIC, USERS_TABLE,
    bootstrap, role_in_range,
};
