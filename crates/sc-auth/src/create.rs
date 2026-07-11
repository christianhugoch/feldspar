//! Creating an ordinary user (technical design §7.1).
//!
//! This is the admin-driven counterpart to [`create_first_user`](crate::create_first_user):
//! it mints a user with a caller-supplied [`role`](crate::COL_ROLE) rather than
//! forcing [admin](crate::ROLE_ADMIN), and it does **not** require the users
//! table to be empty. The UUID id is generated in Rust (not by a database
//! default) and the password is hashed with argon2id before it is stored.

use sc_catalog::Catalog;
use sc_error::{Error, Result};
use sc_query::{Expr, Insert, Statement, Value};
use uuid::Uuid;

use crate::password::hash_password;
use crate::user::User;
use crate::users::USERS_TABLE;
use crate::users::{COL_EMAIL, COL_ID, COL_PASSWORD_HASH, COL_ROLE, role_in_range};

/// Create a user with the given email, plaintext password, and role, returning
/// the resulting [`User`].
///
/// Fails with [`Error::invalid`] if the email or password is blank or the role
/// is outside `1..=100`. A duplicate email violates the table's `UNIQUE`
/// constraint and surfaces as a database error. Unlike
/// [`create_first_user`](crate::create_first_user) this imposes no
/// empty-table precondition — it is the general "admin adds a user" path.
pub async fn create_user(catalog: &Catalog, email: &str, password: &str, role: u8) -> Result<User> {
    let email = email.trim();
    if email.is_empty() {
        return Err(Error::invalid("email is required"));
    }
    if password.is_empty() {
        return Err(Error::invalid("password is required"));
    }
    if !role_in_range(role) {
        return Err(Error::invalid(format!("role {role} is not in 1..=100")));
    }

    // Hash before touching the database — argon2id is deliberately slow.
    let password_hash = hash_password(password)?;
    let id = Uuid::new_v4();

    let insert = Insert::row(
        USERS_TABLE,
        vec![
            COL_ID.into(),
            COL_ROLE.into(),
            COL_EMAIL.into(),
            COL_PASSWORD_HASH.into(),
        ],
        vec![
            Expr::lit(id),
            Expr::lit(i64::from(role)),
            Expr::lit(email),
            Expr::lit(password_hash),
        ],
    );
    catalog
        .primary()
        .query(&Statement::from(insert))
        .await?
        .try_collect()
        .await?;

    let mut user = User::new(id, role)?;
    user.extra
        .insert(COL_EMAIL.to_owned(), Value::Text(email.to_owned()));
    Ok(user)
}
