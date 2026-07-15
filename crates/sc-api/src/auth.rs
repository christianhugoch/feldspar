//! The authentication vocabulary shared by every API surface (design §7.2,
//! §13.4).
//!
//! Logging in looks the same whichever API is asked: the same credentials go in,
//! the same public view of a user comes back, and the same [`SessionAction`]
//! describes what happened. The admin API's `login` and an application's `login`
//! are the same operation on the same `sc-auth`, so the schemas, the wire shape,
//! and the body parsing live here once rather than once per surface.
//!
//! [`SessionAction`]: crate::SessionAction

use sc_auth::{COL_EMAIL, User};
use sc_error::{Error, Result};
use sc_query::Value;
use serde_json::{Value as Json, json};

use crate::rows::require_object;
use crate::schema::{StructField, TypeSchema};

/// Email + password, the body of a login (and of first-user creation).
pub fn credentials_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("email", TypeSchema::text()),
        StructField::new("password", TypeSchema::text()),
    ])
}

/// The public view of a user — **never** the password hash.
pub fn user_summary_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("id", TypeSchema::uuid()),
        StructField::new("email", TypeSchema::text()),
        StructField::new("role", TypeSchema::int()),
    ])
}

/// A user rendered for the wire, matching [`user_summary_schema`].
///
/// Only the id, the identifying email, and the role: a user row may carry any
/// admin-added column (§7.1) and the password hash, and none of that belongs in
/// an API response.
pub fn user_summary_json(user: &User) -> Json {
    json!({
        "id": user.id.to_string(),
        "email": user.get(COL_EMAIL).and_then(Value::as_text).unwrap_or_default(),
        "role": i64::from(user.role),
    })
}

/// Email + password out of a login body, matching [`credentials_schema`].
///
/// Both must be present and non-blank; an empty password is a malformed request,
/// not a login attempt to be checked against the hash.
pub fn credentials(body: &Json) -> Result<(String, String)> {
    let obj = require_object(body)?;
    let field = |key: &str| -> Result<String> {
        let value = obj
            .get(key)
            .and_then(Json::as_str)
            .ok_or_else(|| Error::invalid(format!("missing or non-string field `{key}`")))?;
        if value.trim().is_empty() {
            return Err(Error::invalid(format!("field `{key}` must not be empty")));
        }
        Ok(value.to_owned())
    };
    Ok((field("email")?, field("password")?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_require_both_fields_non_blank() {
        let (email, password) = credentials(&json!({"email": "a@b.c", "password": "pw"})).unwrap();
        assert_eq!(email, "a@b.c");
        assert_eq!(password, "pw");

        // Missing, non-string, and blank are each rejected rather than treated
        // as an empty credential.
        assert!(credentials(&json!({"email": "a@b.c"})).is_err());
        assert!(credentials(&json!({"email": 1, "password": "pw"})).is_err());
        assert!(credentials(&json!({"email": "a@b.c", "password": "  "})).is_err());
        assert!(credentials(&json!("not an object")).is_err());
    }

    #[test]
    fn user_summary_never_leaks_extra_columns() {
        let mut user = User::new(uuid::Uuid::new_v4(), 1).unwrap();
        user.extra
            .insert(COL_EMAIL.to_owned(), Value::Text("a@b.c".to_owned()));
        // An admin-added column, and something that must never travel.
        user.extra
            .insert("password_hash".to_owned(), Value::Text("$argon2id$".to_owned()));

        let json = user_summary_json(&user);
        assert_eq!(json["email"], json!("a@b.c"));
        assert_eq!(json["role"], json!(1));
        // Only the three declared fields, whatever else the row carries.
        assert_eq!(json.as_object().unwrap().len(), 3);
        assert!(json.get("password_hash").is_none());
    }
}
