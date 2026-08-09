//! User, Role, sessions, authentication (layer 5).
//!
//! The MVP fills this in incrementally (TODO Phase 5). So far: the `users` table
//! schema and its one-time [`bootstrap`] into the [`Catalog`](sc_catalog::Catalog),
//! the [`User`] value read back from a row, argon2id password hashing and
//! verification ([`hash_password`], [`verify_password`]), the create-first-user
//! flow ([`create_first_user`], [`any_user_exists`]), credential
//! [`authenticate`]ion, the admin-driven [`create_user`] path, and the in-memory
//! [`SessionStore`] behind login/logout, and the one-time
//! [session grants](create_session_grant) a process holding the database uses to
//! ask the running server for a session.
//!
//! Roles live in [`_sc_roles`](ROLES_TABLE) and `users.role` is a foreign key
//! onto it (§7.1, §9): a role is a row carrying a name and role-specific
//! settings, not a bare integer with a convention attached.

mod create;
mod first_user;
mod grant;
mod login;
mod lookup;
mod password;
mod roles;
mod session;
mod user;
mod users;

pub use create::create_user;
pub use first_user::{any_user_exists, create_first_user};
pub use grant::{
    COL_EXPIRES_AT as COL_GRANT_EXPIRES_AT, COL_SECRET_HASH as COL_GRANT_SECRET_HASH,
    COL_USER as COL_GRANT_USER, GRANT_TTL_SECONDS, GRANTS_TABLE, bootstrap_session_grants,
    create_session_grant, redeem_session_grant,
};
pub use login::{authenticate, authenticate_admin};
pub use lookup::{first_user_with_role, load_user, load_user_by_email};
pub use password::{hash_password, is_valid_hash, verify_password};
pub use roles::{
    COL_ATTRIBUTES as COL_ROLE_ATTRIBUTES, COL_DESCRIPTION as COL_ROLE_DESCRIPTION,
    COL_NAME as COL_ROLE_NAME, ROLES_TABLE, Role, bootstrap_roles, delete_role, list_roles,
    load_role, load_role_by_name, save_role,
};
pub use session::{DEFAULT_TTL_HOURS, SessionStore};
pub use user::User;
pub use users::{
    COL_EMAIL, COL_ID, COL_PASSWORD_HASH, COL_ROLE, ROLE_ADMIN, ROLE_PUBLIC, USERS_TABLE,
    bootstrap, role_in_range,
};
