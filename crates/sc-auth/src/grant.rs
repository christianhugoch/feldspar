//! One-time **session grants**: how a process holding the database asks the
//! running server for a browser session (technical design §7.2).
//!
//! Sessions live in the server's own memory ([`SessionStore`](crate::SessionStore)),
//! so no other process can mint one — which is the property that makes a session
//! cookie worth trusting, and which a command line asking for a session must not
//! break. A grant is the way through that does not break it: a row in the primary
//! database saying *"the next caller who presents this secret is this user"*,
//! which the server — and only the server — turns into a session.
//!
//! **The authority is the database, not the row.** Writing here requires the
//! primary database's credentials, and whoever has those can already read every
//! password hash, change any of them, and grant themselves any role; a grant
//! gives them nothing they did not have, it only gives it to them *without a
//! password*. That is the whole point: `saltcorn auth token` runs where the
//! server runs, from a shell that already holds the connection string, and
//! demanding a user's password there was asking a secret of somebody who did not
//! need one.
//!
//! What keeps it narrow:
//!
//! - **Single use.** Redemption deletes the row before it verifies anything, so
//!   a replayed grant finds nothing.
//! - **Short-lived.** [`GRANT_TTL_SECONDS`] from creation, and expired rows are
//!   swept whenever one is created.
//! - **Hashed at rest.** The secret half is stored argon2id-hashed exactly as a
//!   password is, so a database dump — or a stray `SELECT` in a log — is not a
//!   live credential.
//! - **No more than the user.** The session it becomes is an ordinary session
//!   for that user: an agent given a low-privilege account still has a
//!   low-privilege session.
//!
//! The grant string is `<id>.<secret>`: the id addresses the row, the secret is
//! what is checked against the hash. Splitting the two is what keeps redemption a
//! single indexed lookup and a single argon2 verification, rather than a scan
//! that hashes every outstanding row.

use chrono::{DateTime, Duration, Utc};
use sc_catalog::{Catalog, DataField, DataFieldKind, FieldId, Table, TableId};
use sc_db::Row;
use sc_error::{Error, Result};
use sc_query::{Delete, Expr, Insert, Select, Source, Statement, Value};
use sc_types::{BasicType, TypeRef};
use uuid::Uuid;

use crate::password::{hash_password, verify_password};
use crate::user::User;
use crate::users::{COL_ID as USER_COL_ID, USERS_TABLE};

/// Name of the session-grant table in the primary database.
pub const GRANTS_TABLE: &str = "_sc_session_grants";

/// The UUID primary key — the public half of a grant string, which addresses
/// the row.
pub const COL_ID: &str = "id";
/// The argon2id hash of the grant's secret half.
pub const COL_SECRET_HASH: &str = "secret_hash";
/// The user this grant is for: a foreign key onto [`users`](crate::USERS_TABLE).
pub const COL_USER: &str = "user_id";
/// When the grant stops being redeemable.
pub const COL_EXPIRES_AT: &str = "expires_at";

/// How long a grant lives. Long enough for the two requests that follow it (a
/// primer for the CSRF cookie, then the redemption), short enough that one
/// written down and forgotten is worthless by the time anybody finds it.
pub const GRANT_TTL_SECONDS: i64 = 120;

/// The separator between a grant's id and its secret.
const SEPARATOR: char = '.';

/// The fields of the grants table, in declaration order.
///
/// `user_id` is a real foreign key: a grant naming a user who no longer exists
/// is not a state to discover at redemption time, and deleting a user must take
/// their outstanding grants with them.
fn grant_fields() -> Vec<DataField> {
    let text = || TypeRef::Basic(BasicType::Text);
    let uuid = || TypeRef::Basic(BasicType::Uuid);
    let timestamp = || TypeRef::Basic(BasicType::Timestamp);
    vec![
        DataField::plain(COL_ID, uuid()).required().primary_key(),
        DataField {
            kind: DataFieldKind::Key {
                target_table: TableId(USERS_TABLE.to_owned()),
                target_field: FieldId(USER_COL_ID.to_owned()),
                summary_field: None,
            },
            ..DataField::plain(COL_USER, uuid()).required()
        },
        DataField::plain(COL_SECRET_HASH, text()).required(),
        DataField::plain(COL_EXPIRES_AT, timestamp()).required(),
    ]
}

/// Ensure the session-grant table exists.
///
/// **Must run after the users table**, which it references.
/// [`bootstrap`](crate::bootstrap) does them in that order. Idempotent, like
/// every other bootstrap.
pub async fn bootstrap_session_grants(catalog: &Catalog) -> Result<Table> {
    if let Some(existing) = catalog.get(GRANTS_TABLE)? {
        return Ok(existing);
    }
    catalog.create_table(GRANTS_TABLE, &grant_fields()).await
}

/// Mint a grant for `user_id`, returning the string to present to the server.
///
/// The returned string is the **only** copy of the secret: what is stored is its
/// hash. Expired grants are swept on the way through, which is all the
/// housekeeping this table needs — nothing else writes to it, and a grant that
/// is never redeemed is cleaned up by the next one that is minted.
pub async fn create_session_grant(catalog: &Catalog, user_id: Uuid) -> Result<String> {
    sweep_expired(catalog).await?;

    let id = Uuid::new_v4();
    let secret = new_secret();
    // argon2id, exactly as a password is hashed: the row is not a credential.
    let secret_hash = hash_password(&secret)?;
    let expires_at = Utc::now() + Duration::seconds(GRANT_TTL_SECONDS);

    let insert = Insert::row(
        GRANTS_TABLE,
        vec![
            COL_ID.to_owned(),
            COL_USER.to_owned(),
            COL_SECRET_HASH.to_owned(),
            COL_EXPIRES_AT.to_owned(),
        ],
        vec![
            Expr::lit(id),
            Expr::lit(user_id),
            Expr::lit(secret_hash),
            Expr::Lit(Value::Timestamp(expires_at)),
        ],
    );
    run(catalog, Statement::from(insert)).await?;
    Ok(format!("{}{SEPARATOR}{secret}", id.simple()))
}

/// Redeem `grant`, returning the user it was minted for.
///
/// `Ok(None)` for every ordinary failure — a malformed string, an unknown id, a
/// wrong secret, an expired grant, a user deleted since it was minted — because
/// the caller has no business being told which. [`Err`] stays reserved for
/// infrastructure faults, as it is in [`authenticate`](crate::authenticate).
///
/// **The row is deleted before the secret is checked.** A grant is single-use
/// whether or not the use succeeds: an attacker who guesses an id gets one
/// attempt at its secret, not an unlimited supply, and a legitimate grant that
/// is replayed finds nothing there.
pub async fn redeem_session_grant(catalog: &Catalog, grant: &str) -> Result<Option<User>> {
    let Some((id, secret)) = split_grant(grant) else {
        return Ok(None);
    };

    let select = Select::from(Source::table(GRANTS_TABLE))
        .filter(Expr::col(COL_ID).eq(Expr::lit(id)))
        .limit(1);
    let rows = rows(catalog, select).await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };

    let delete = Delete::from(GRANTS_TABLE).filter(Expr::col(COL_ID).eq(Expr::lit(id)));
    run(catalog, Statement::from(delete)).await?;

    let (user_id, secret_hash, expires_at) = grant_from_row(row)?;
    if expires_at <= Utc::now() {
        return Ok(None);
    }
    if !verify_password(&secret_hash, secret)? {
        return Ok(None);
    }
    crate::lookup::load_user(catalog, user_id).await
}

/// Delete every grant that has lapsed.
async fn sweep_expired(catalog: &Catalog) -> Result<()> {
    let delete = Delete::from(GRANTS_TABLE).filter(Expr::binary(
        sc_query::BinOp::Le,
        Expr::col(COL_EXPIRES_AT),
        Expr::Lit(Value::Timestamp(Utc::now())),
    ));
    run(catalog, Statement::from(delete)).await
}

/// A grant string split into the id that addresses the row and the secret that
/// proves it. `None` for anything that is not one.
fn split_grant(grant: &str) -> Option<(Uuid, &str)> {
    let (id, secret) = grant.trim().split_once(SEPARATOR)?;
    if secret.is_empty() {
        return None;
    }
    Some((Uuid::parse_str(id).ok()?, secret))
}

/// A fresh, unguessable secret: 256 bits from two v4 UUIDs, hex-encoded — the
/// same shape, and the same entropy, as a session token.
fn new_secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

/// The three columns redemption needs, read strictly: a missing or ill-typed
/// column in an `_sc_*` table is an error naming it, never a silent default.
fn grant_from_row(row: &Row) -> Result<(Uuid, String, DateTime<Utc>)> {
    let user_id = match row.get(COL_USER) {
        Some(Value::Uuid(u)) => *u,
        other => return Err(bad_column(COL_USER, "a uuid", other)),
    };
    let secret_hash = match row.get(COL_SECRET_HASH) {
        Some(Value::Text(t)) => t.clone(),
        other => return Err(bad_column(COL_SECRET_HASH, "text", other)),
    };
    let expires_at = match row.get(COL_EXPIRES_AT) {
        Some(Value::Timestamp(ts)) => *ts,
        other => return Err(bad_column(COL_EXPIRES_AT, "a timestamp", other)),
    };
    Ok((user_id, secret_hash, expires_at))
}

fn bad_column(column: &str, expected: &str, got: Option<&Value>) -> Error {
    match got {
        Some(value) => Error::invalid(format!(
            "{GRANTS_TABLE}.{column} should be {expected}, got {}",
            value.kind()
        )),
        None => Error::invalid(format!("row has no `{column}` column")),
    }
}

/// Run a statement that returns no rows of interest.
async fn run(catalog: &Catalog, statement: Statement) -> Result<()> {
    catalog
        .primary()
        .query(&statement)
        .await?
        .try_collect()
        .await?;
    Ok(())
}

/// Run a select and collect its rows.
async fn rows(catalog: &Catalog, select: Select) -> Result<Vec<Row>> {
    catalog
        .primary()
        .query(&Statement::from(select))
        .await?
        .try_collect()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_names_the_user_and_the_expiry() {
        let fields = grant_fields();
        let by_name = |n: &str| fields.iter().find(|f| f.base.name == n).expect(n);

        let id = by_name(COL_ID);
        assert!(id.primary_key && id.required);
        // The reference is a database constraint: a grant for a user who is gone
        // must not be a row anybody can read.
        assert!(matches!(
            by_name(COL_USER).kind,
            DataFieldKind::Key {
                ref target_table,
                ..
            } if target_table.0 == USERS_TABLE
        ));
        assert!(by_name(COL_SECRET_HASH).required);
        assert!(by_name(COL_EXPIRES_AT).required);
    }

    #[test]
    fn a_grant_string_is_an_id_and_a_secret() {
        let id = Uuid::new_v4();
        let grant = format!("{}.{}", id.simple(), "s3cret");
        assert_eq!(split_grant(&grant), Some((id, "s3cret")));
        // Surrounding whitespace is what a shell adds, not part of the grant.
        assert_eq!(split_grant(&format!(" {grant}\n")), Some((id, "s3cret")));

        // Anything else is not a grant, and says so by being no grant at all
        // rather than by erroring: an attacker learns nothing either way.
        assert_eq!(split_grant("no-separator"), None);
        assert_eq!(split_grant(&format!("{}.", id.simple())), None);
        assert_eq!(split_grant("not-a-uuid.secret"), None);
        assert_eq!(split_grant(""), None);
    }

    #[test]
    fn a_secret_is_256_bits_of_hex() {
        let secret = new_secret();
        assert_eq!(secret.len(), 64);
        assert!(secret.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(secret, new_secret());
    }
}
