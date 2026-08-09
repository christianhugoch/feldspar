//! Finding a user without authenticating as one (technical design §7.1).
//!
//! [`authenticate`](crate::authenticate) answers "is this person who they say
//! they are?"; these answer "which row is that?" — the question a caller who has
//! already established its authority some other way needs, and the only question
//! [`redeem_session_grant`](crate::redeem_session_grant) and `saltcorn auth
//! token` ask. Nothing here checks a password, and nothing here is reachable
//! from the wire: the callers are the server redeeming a grant it was handed and
//! a command line holding the database's own credentials.
//!
//! "First" means **lowest email**, in [`first_user_with_role`]. The users table
//! records no creation time (§7.1 keeps it to the two guaranteed columns), so
//! there is no order of arrival to sort by; sorting by the identifier is the
//! next best thing, and unlike "whatever the database returns first" it gives
//! the same answer every time — which matters, because a screenshot script that
//! signed in as a different user on Tuesday is a bug that looks like a flake.

use sc_catalog::Catalog;
use sc_db::Row;
use sc_error::Result;
use sc_query::{Expr, OrderBy, Select, Source, Statement};
use uuid::Uuid;

use crate::user::User;
use crate::users::{COL_EMAIL, COL_ID, COL_ROLE, USERS_TABLE};

/// The user with this id, if the row is still there.
pub async fn load_user(catalog: &Catalog, id: Uuid) -> Result<Option<User>> {
    let select = Select::from(Source::table(USERS_TABLE))
        .filter(Expr::col(COL_ID).eq(Expr::lit(id)))
        .limit(1);
    one(catalog, select).await
}

/// The user with this email identifier, if any. Matched exactly, as
/// [`authenticate`](crate::authenticate) matches it.
pub async fn load_user_by_email(catalog: &Catalog, email: &str) -> Result<Option<User>> {
    let email = email.trim();
    if email.is_empty() {
        return Ok(None);
    }
    let select = Select::from(Source::table(USERS_TABLE))
        .filter(Expr::col(COL_EMAIL).eq(Expr::lit(email)))
        .limit(1);
    one(catalog, select).await
}

/// The first user holding `role` — lowest email — or `None` when nobody does.
pub async fn first_user_with_role(catalog: &Catalog, role: u8) -> Result<Option<User>> {
    let mut select = Select::from(Source::table(USERS_TABLE))
        .filter(Expr::col(COL_ROLE).eq(Expr::lit(i64::from(role))))
        .limit(1);
    select.order = vec![OrderBy::asc(Expr::col(COL_EMAIL))];
    one(catalog, select).await
}

/// Run a select expected to return at most one user row.
async fn one(catalog: &Catalog, select: Select) -> Result<Option<User>> {
    let rows: Vec<Row> = catalog
        .primary()
        .query(&Statement::from(select))
        .await?
        .try_collect()
        .await?;
    match rows.first() {
        Some(row) => Ok(Some(User::from_row(row)?)),
        None => Ok(None),
    }
}
