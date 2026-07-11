//! Server-side session bookkeeping (technical design §7.2: "session cookie
//! baseline").
//!
//! A login mints an opaque, high-entropy **session token**; the server sends it
//! to the browser in a cookie and hands it back here on each request to recover
//! the logged-in [`User`]. Logout drops the token. Because the MVP is a single
//! process (the message bus / cross-process cache is post-MVP), sessions live in
//! an in-memory map guarded by an `RwLock`; the trait-free concrete store is
//! enough here and a database- or bus-backed store can replace it later without
//! changing the call sites.
//!
//! The stored [`User`] is a **snapshot** taken at login. That is fine for the
//! MVP admin UI; if a user's role or fields change mid-session, callers that need
//! the very latest should re-read from the catalog by id.

use std::collections::HashMap;
use std::sync::RwLock;

use chrono::{DateTime, Duration, Utc};
use sc_error::{Error, Result};
use uuid::Uuid;

use crate::user::User;

/// Default session lifetime: one day.
pub const DEFAULT_TTL_HOURS: i64 = 24;

/// One stored session: the authenticated user and when the session lapses.
struct Entry {
    user: User,
    expires_at: DateTime<Utc>,
}

/// An in-memory store of active sessions, keyed by opaque token.
pub struct SessionStore {
    ttl: Duration,
    entries: RwLock<HashMap<String, Entry>>,
}

impl Default for SessionStore {
    fn default() -> Self {
        SessionStore::with_ttl(Duration::hours(DEFAULT_TTL_HOURS))
    }
}

impl SessionStore {
    /// A store whose sessions expire `ttl` after they are created.
    pub fn with_ttl(ttl: Duration) -> SessionStore {
        SessionStore {
            ttl,
            entries: RwLock::new(HashMap::new()),
        }
    }

    /// Start a session for `user` and return its token (to be set as a cookie).
    pub fn login(&self, user: User) -> Result<String> {
        let token = new_token();
        let entry = Entry {
            user,
            expires_at: Utc::now() + self.ttl,
        };
        self.entries
            .write()
            .map_err(|_| Error::msg("session store lock poisoned"))?
            .insert(token.clone(), entry);
        Ok(token)
    }

    /// The user for a session token, if the token is known and unexpired. An
    /// expired token is treated as absent (and eagerly purged).
    pub fn user_for(&self, token: &str) -> Result<Option<User>> {
        // Fast path: a valid session under a read lock.
        {
            let guard = self
                .entries
                .read()
                .map_err(|_| Error::msg("session store lock poisoned"))?;
            match guard.get(token) {
                None => return Ok(None),
                Some(entry) if entry.expires_at > Utc::now() => {
                    return Ok(Some(entry.user.clone()));
                }
                Some(_) => {} // expired — fall through to purge under a write lock
            }
        }
        self.entries
            .write()
            .map_err(|_| Error::msg("session store lock poisoned"))?
            .remove(token);
        Ok(None)
    }

    /// End a session (logout). Returns `true` if a session was removed.
    pub fn logout(&self, token: &str) -> Result<bool> {
        Ok(self
            .entries
            .write()
            .map_err(|_| Error::msg("session store lock poisoned"))?
            .remove(token)
            .is_some())
    }

    /// Drop every expired session. Optional housekeeping; `user_for` already
    /// purges lazily.
    pub fn sweep_expired(&self) -> Result<()> {
        let now = Utc::now();
        self.entries
            .write()
            .map_err(|_| Error::msg("session store lock poisoned"))?
            .retain(|_, e| e.expires_at > now);
        Ok(())
    }
}

/// A fresh, unguessable session token: 256 bits from two v4 UUIDs, hex-encoded.
fn new_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::users::ROLE_ADMIN;

    fn admin() -> User {
        User::new(Uuid::new_v4(), ROLE_ADMIN).unwrap()
    }

    #[test]
    fn login_returns_a_token_that_resolves_to_the_user() {
        let store = SessionStore::default();
        let user = admin();
        let token = store.login(user.clone()).unwrap();
        assert_eq!(token.len(), 64); // two hex UUIDs
        assert_eq!(store.user_for(&token).unwrap(), Some(user));
    }

    #[test]
    fn unknown_token_resolves_to_none() {
        let store = SessionStore::default();
        assert_eq!(store.user_for("nope").unwrap(), None);
    }

    #[test]
    fn logout_ends_the_session() {
        let store = SessionStore::default();
        let token = store.login(admin()).unwrap();
        assert!(store.logout(&token).unwrap());
        assert_eq!(store.user_for(&token).unwrap(), None);
        assert!(!store.logout(&token).unwrap()); // already gone
    }

    #[test]
    fn expired_session_is_not_returned() {
        // A zero-length TTL means every session is already expired on lookup.
        let store = SessionStore::with_ttl(Duration::zero());
        let token = store.login(admin()).unwrap();
        assert_eq!(store.user_for(&token).unwrap(), None);
    }

    #[test]
    fn tokens_are_distinct_across_logins() {
        let store = SessionStore::default();
        let a = store.login(admin()).unwrap();
        let b = store.login(admin()).unwrap();
        assert_ne!(a, b);
    }
}
