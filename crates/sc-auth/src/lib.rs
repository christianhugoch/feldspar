//! User, Role, sessions, authentication (layer 5).
//!
//! The MVP fills this in incrementally (TODO Phase 5). So far: the `users` table
//! schema and its one-time [`bootstrap`] into the [`Catalog`](sc_catalog::Catalog),
//! the [`User`] value read back from a row, argon2id password hashing and
//! verification ([`hash_password`], [`verify_password`]), the create-first-user
//! flow ([`create_first_user`], [`any_user_exists`]), credential
//! [`authenticate`]ion, the admin-driven [`create_user`] path, and the
//! [`SessionStore`] behind login/logout — rows in
//! [`_sc_sessions`](SESSIONS_TABLE) behind a per-node cache, so a second
//! application server is a deployment decision rather than a rewrite.
//!
//! Roles live in [`_sc_roles`](ROLES_TABLE) and `users.role` is a foreign key
//! onto it (§7.1, §9): a role is a row carrying a name and role-specific
//! settings, not a bare integer with a convention attached.

mod create;
mod first_user;
mod login;
mod lookup;
mod manage;
mod password;
mod roles;
mod session;
mod user;
mod users;

pub use create::{CreatedUser, NewUser, create_user, create_user_with};
pub use first_user::{any_user_exists, create_first_user};
pub use login::{authenticate, authenticate_admin};
pub use lookup::{first_user_with_role, load_user, load_user_by_email};
pub use manage::{UserUpdate, delete_user, set_user_disabled, set_user_password, update_user};
pub use password::{
    RANDOM_PASSWORD_LENGTH, hash_password, is_valid_hash, random_password, verify_password,
};
pub use roles::{
    COL_ATTRIBUTES as COL_ROLE_ATTRIBUTES, COL_DESCRIPTION as COL_ROLE_DESCRIPTION,
    COL_NAME as COL_ROLE_NAME, ROLES_TABLE, Role, bootstrap_roles, delete_role, list_roles,
    load_role, load_role_by_name, save_role,
};
pub use session::{
    CACHE_CAPACITY, CACHE_TTL_SECONDS, COL_EXPIRES_AT as COL_SESSION_EXPIRES_AT,
    COL_TOKEN_HASH as COL_SESSION_TOKEN_HASH, COL_USER as COL_SESSION_USER, DEFAULT_TTL_HOURS,
    SESSIONS_TABLE, SessionStore, bootstrap_sessions, create_session, delete_sessions_for_user,
};
pub use user::User;
pub use users::{
    COL_DISABLED, COL_EMAIL, COL_ID, COL_PASSWORD_HASH, COL_ROLE, ROLE_ADMIN, ROLE_PUBLIC,
    SYSTEM_USER_COLUMNS, USERS_TABLE, bootstrap, is_system_user_column, role_in_range,
};
