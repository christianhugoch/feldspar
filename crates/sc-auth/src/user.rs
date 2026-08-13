//! The [`User`] value: an authenticated (or looked-up) row of the `users` table.
//!
//! Per technical design §7.1 the model is deliberately minimal and makes only
//! two assumptions about the table's shape — the non-deletable UUID
//! [`id`](super::COL_ID) and the [`role`](super::COL_ROLE) integer. Everything
//! else the admin has added (including [`email`](super::COL_EMAIL)) is carried
//! opaquely in [`extra`](User::extra), so code never hard-codes the presence of
//! any other field.
//!
//! The password hash is a system column, not an admin field, and is
//! **deliberately excluded** from `extra`: a `User` is the object passed around
//! the request/session, so keeping the hash out of it avoids leaking it into
//! logs, session state, or API responses.

use std::collections::BTreeMap;

use sc_db::Row;
use sc_error::{Error, Result};
use sc_query::Value;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::users::{COL_DISABLED, COL_ID, COL_PASSWORD_HASH, COL_ROLE, ROLE_ADMIN, role_in_range};

/// A user of the system: the two guaranteed fields plus an opaque bag of the
/// admin-defined columns (technical design §7.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct User {
    /// The non-deletable UUID primary key.
    pub id: Uuid,
    /// The role, `1..=100` (1 = admin, 100 = public).
    pub role: u8,
    /// Every other column of the row keyed by column name — e.g. `email` and any
    /// fields the admin has added. Excludes `id`, `role`, and the password hash.
    pub extra: BTreeMap<String, Value>,
}

impl User {
    /// A user with the two guaranteed fields and no extra columns.
    ///
    /// Returns [`Error::invalid`] if `role` is outside `1..=100`.
    pub fn new(id: Uuid, role: u8) -> Result<User> {
        if !role_in_range(role) {
            return Err(Error::invalid(format!("role {role} is not in 1..=100")));
        }
        Ok(User {
            id,
            role,
            extra: BTreeMap::new(),
        })
    }

    /// Build a user from a `users`-table [`Row`].
    ///
    /// Reads the UUID `id` and integer `role`, validates the role range, and
    /// collects every remaining column into [`extra`](User::extra) — except the
    /// password hash, which is never carried on a `User`. A missing or
    /// wrongly-typed `id`/`role` is an [`Error::invalid`].
    pub fn from_row(row: &Row) -> Result<User> {
        let id = match row.get(COL_ID) {
            Some(Value::Uuid(u)) => *u,
            Some(other) => {
                return Err(Error::invalid(format!(
                    "users.{COL_ID} should be a uuid, got {}",
                    other.kind()
                )));
            }
            None => return Err(Error::invalid(format!("row has no `{COL_ID}` column"))),
        };
        let role = match row.get(COL_ROLE) {
            Some(Value::Int(i)) => role_from_i64(*i)?,
            Some(other) => {
                return Err(Error::invalid(format!(
                    "users.{COL_ROLE} should be an integer, got {}",
                    other.kind()
                )));
            }
            None => return Err(Error::invalid(format!("row has no `{COL_ROLE}` column"))),
        };

        let mut extra = BTreeMap::new();
        for (name, value) in row.columns().iter().zip(row.values()) {
            if name == COL_ID || name == COL_ROLE || name == COL_PASSWORD_HASH {
                continue;
            }
            extra.insert(name.clone(), value.clone());
        }

        Ok(User { id, role, extra })
    }

    /// Whether this user is an administrator (`role == 1`).
    pub fn is_admin(&self) -> bool {
        self.role == ROLE_ADMIN
    }

    /// Whether this account has been disabled by an administrator.
    ///
    /// `NULL` and `false` both mean enabled — the column says something only
    /// about accounts somebody has acted on. Unlike the password hash the flag
    /// *is* carried in [`extra`](User::extra), because the two places that must
    /// ask (credential [`authenticate`](crate::authenticate)ion and session
    /// resolution) both hold a `User` rather than a row.
    pub fn is_disabled(&self) -> bool {
        matches!(self.get(COL_DISABLED), Some(Value::Bool(true)))
    }

    /// Whether this user meets a `min_role` gate. Roles run 1–100 with **lower =
    /// more privileged** (matching [`AccessRules`](sc_catalog::AccessRules)), so a
    /// user passes when `role <= min_role`.
    pub fn meets_role(&self, min_role: u8) -> bool {
        self.role <= min_role
    }

    /// The value of an extra (admin-defined) field, if present.
    pub fn get(&self, field: &str) -> Option<&Value> {
        self.extra.get(field)
    }
}

/// Narrow a raw `role` integer to the validated `1..=100` byte.
fn role_from_i64(i: i64) -> Result<u8> {
    let role: u8 = i
        .try_into()
        .map_err(|_| Error::invalid(format!("role {i} is out of range")))?;
    if !role_in_range(role) {
        return Err(Error::invalid(format!("role {role} is not in 1..=100")));
    }
    Ok(role)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::users::COL_EMAIL;

    fn row(columns: &[&str], values: Vec<Value>) -> Row {
        let cols = Arc::new(columns.iter().map(|c| c.to_string()).collect());
        Row::new(cols, values).unwrap()
    }

    #[test]
    fn new_validates_role_range() {
        assert!(User::new(Uuid::new_v4(), 0).is_err());
        assert!(User::new(Uuid::new_v4(), 101).is_err());
        let u = User::new(Uuid::new_v4(), ROLE_ADMIN).unwrap();
        assert!(u.is_admin());
        assert!(u.extra.is_empty());
    }

    #[test]
    fn meets_role_gates_on_privilege() {
        let admin = User::new(Uuid::new_v4(), ROLE_ADMIN).unwrap();
        let editor = User::new(Uuid::new_v4(), 40).unwrap();
        // Admin (role 1) passes every gate; a lower-privilege user only passes
        // gates at or below its own role number.
        assert!(admin.meets_role(ROLE_ADMIN));
        assert!(admin.meets_role(100));
        assert!(!editor.meets_role(ROLE_ADMIN)); // 40 <= 1 is false
        assert!(editor.meets_role(40));
        assert!(editor.meets_role(100));
    }

    #[test]
    fn from_row_reads_id_role_and_extra_but_not_hash() {
        let id = Uuid::new_v4();
        let r = row(
            &[COL_ID, COL_ROLE, COL_EMAIL, COL_PASSWORD_HASH],
            vec![
                Value::Uuid(id),
                Value::Int(1),
                Value::Text("admin@example.com".into()),
                Value::Text("$argon2id$secret".into()),
            ],
        );
        let user = User::from_row(&r).unwrap();
        assert_eq!(user.id, id);
        assert_eq!(user.role, 1);
        assert!(user.is_admin());
        // email is carried in extra; the password hash is not.
        assert_eq!(
            user.get(COL_EMAIL),
            Some(&Value::Text("admin@example.com".into()))
        );
        assert!(!user.extra.contains_key(COL_PASSWORD_HASH));
        assert_eq!(user.extra.len(), 1);
    }

    #[test]
    fn disabled_is_read_from_the_row_and_null_means_enabled() {
        let id = Uuid::new_v4();
        let with = |disabled: Value| {
            User::from_row(&row(
                &[COL_ID, COL_ROLE, COL_DISABLED],
                vec![Value::Uuid(id), Value::Int(40), disabled],
            ))
            .unwrap()
        };
        assert!(with(Value::Bool(true)).is_disabled());
        assert!(!with(Value::Bool(false)).is_disabled());
        // A column nobody has ever written is an ordinary, enabled account.
        assert!(!with(Value::Null).is_disabled());
        assert!(!User::new(id, 40).unwrap().is_disabled());
    }

    #[test]
    fn from_row_rejects_bad_role_and_missing_columns() {
        let id = Uuid::new_v4();
        // role out of range
        let bad_role = row(&[COL_ID, COL_ROLE], vec![Value::Uuid(id), Value::Int(200)]);
        assert!(User::from_row(&bad_role).is_err());
        // wrong id type
        let bad_id = row(&[COL_ID, COL_ROLE], vec![Value::Int(5), Value::Int(1)]);
        assert!(User::from_row(&bad_id).is_err());
        // missing role column
        let no_role = row(&[COL_ID], vec![Value::Uuid(id)]);
        assert!(User::from_row(&no_role).is_err());
    }
}
