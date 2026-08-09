//! The `users` table: its schema and one-time bootstrap.
//!
//! Per GOALS and technical design §7.1 the users table lives in the primary
//! database and is deliberately minimal:
//!
//! - The primary key is a **UUID** and is **not deletable**. Code MUST NOT
//!   assume any other field exists.
//! - An [`email`](COL_EMAIL) field exists initially, but the admin MAY later
//!   delete it and substitute another identifier field.
//! - Passwords are stored hashed with argon2id in [`password_hash`](COL_PASSWORD_HASH).
//! - [`role`](COL_ROLE) is an integer **1–100**; `1` = admin (full access),
//!   `100` = public (not logged in). Admins MAY add arbitrary fields. It is a
//!   **foreign key onto [`_sc_roles`](crate::ROLES_TABLE)** — a role is a row
//!   that can carry a name and role-specific settings, and a user whose role
//!   names nothing would be a user whose privileges cannot be described.
//!
//! The bootstrap is driver-agnostic: it goes through the [`Catalog`] like any
//! other table creation, and it invents no database-specific defaults — the
//! UUID primary key is generated in Rust when a user is created (a later TODO
//! item), not by a Postgres `gen_random_uuid()` default.

use sc_catalog::{Catalog, DataField, Table};
use sc_error::Result;
use sc_types::{BasicType, TypeRef};

/// Name of the users table in the primary database.
pub const USERS_TABLE: &str = "users";

/// The non-deletable UUID primary-key column.
pub const COL_ID: &str = "id";
/// The role column (integer 1–100).
pub const COL_ROLE: &str = "role";
/// The argon2id password-hash column (a PHC string; nullable so password-less
/// users — e.g. future OAuth-only accounts — are representable).
pub const COL_PASSWORD_HASH: &str = "password_hash";
/// The initial email/identifier column.
pub const COL_EMAIL: &str = "email";

/// The most-privileged role: full access.
pub const ROLE_ADMIN: u8 = 1;
/// The least-privileged role: public / not logged in.
pub const ROLE_PUBLIC: u8 = 100;

/// Whether `role` is within the valid 1–100 range (technical design §7.1).
pub fn role_in_range(role: u8) -> bool {
    (ROLE_ADMIN..=ROLE_PUBLIC).contains(&role)
}

/// The fields of the users table, in declaration order.
///
/// `role` is a foreign key onto [`_sc_roles`](crate::ROLES_TABLE) (see
/// [`user_role_field`](crate::roles::user_role_field)); the 1–100 bound is
/// enforced in application code (see [`role_in_range`]) rather than as a
/// database `CHECK`, which the MVP schema layer does not yet render — but the
/// *existence* of the role is enforced by the database, because that is the
/// part concurrency can break.
fn users_fields() -> Vec<DataField> {
    let text = || TypeRef::Basic(BasicType::Text);
    let uuid = || TypeRef::Basic(BasicType::Uuid);
    vec![
        DataField::plain(COL_ID, uuid()).required().primary_key(),
        crate::roles::user_role_field(),
        DataField::plain(COL_EMAIL, text()).required().unique(),
        DataField::plain(COL_PASSWORD_HASH, text()),
    ]
}

/// Ensure the roles, users and session-grant tables exist, creating them if
/// absent, and return the users table.
///
/// **Roles first, and that order is load-bearing**: `users.role` references
/// `_sc_roles`, and a foreign key onto a table that does not exist is not a
/// constraint any database will accept. It also means the two built-in roles
/// exist before the first user can be created with one.
///
/// Idempotent: if a `users` table is already present in the catalog it is
/// returned unchanged (the MVP performs no schema reconciliation on an existing
/// table). A database that predates the roles table therefore **gains
/// `_sc_roles` but keeps its unconstrained `role` column** — per GOALS, early
/// development evolves the initial setup rather than running migrations, so the
/// constraint arrives with the next database rather than being retrofitted onto
/// this one. Call this once at startup after the [`Catalog`] is initialised.
pub async fn bootstrap(catalog: &Catalog) -> Result<Table> {
    crate::roles::bootstrap_roles(catalog).await?;
    let users = match catalog.get(USERS_TABLE)? {
        Some(existing) => existing,
        None => catalog.create_table(USERS_TABLE, &users_fields()).await?,
    };
    // Last, and that order is load-bearing too: a grant references a user.
    crate::grant::bootstrap_session_grants(catalog).await?;
    Ok(users)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_has_uuid_pk_role_email_and_password_hash() {
        let fields = users_fields();
        let names: Vec<&str> = fields.iter().map(|f| f.base.name.as_str()).collect();
        assert_eq!(names, [COL_ID, COL_ROLE, COL_EMAIL, COL_PASSWORD_HASH]);

        let id = &fields[0];
        assert!(id.primary_key && id.required);
        assert_eq!(id.base.type_, TypeRef::Basic(BasicType::Uuid));

        let email = fields.iter().find(|f| f.base.name == COL_EMAIL).unwrap();
        assert!(email.required && email.unique);

        let pw = fields
            .iter()
            .find(|f| f.base.name == COL_PASSWORD_HASH)
            .unwrap();
        assert!(!pw.required); // nullable
    }

    #[test]
    fn role_range_bounds() {
        assert!(!role_in_range(0));
        assert!(role_in_range(ROLE_ADMIN));
        assert!(role_in_range(50));
        assert!(role_in_range(ROLE_PUBLIC));
        // 101 is unrepresentable in u8-space only up to 255; 101 is out of range.
        assert!(!role_in_range(101));
    }
}
