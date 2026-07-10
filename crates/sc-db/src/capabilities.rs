//! What a connected database can do — advertised by
//! [`DatabaseDriver::capabilities`](crate::DatabaseDriver::capabilities).
//!
//! Capabilities are queried, never assumed. The authorization layer (§7) picks
//! its enforcement strategy from these flags (e.g. row-level security only when
//! the backend supports it), and the message bus only wires up a pg-notify
//! driver when `listen_notify` is advertised (§16).

use serde::{Deserialize, Serialize};

/// Feature flags describing what a particular [`DatabaseDriver`] backend
/// supports (technical design §5).
///
/// The set is intentionally small and grows only as a real decision hangs off a
/// flag. Construct via [`DbCapabilities::none`] and enable the relevant fields,
/// so adding a field never silently flips existing drivers to "supported".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DbCapabilities {
    /// The backend enforces row-level security policies. Drives the authz
    /// strategy in §7.
    pub row_level_security: bool,
    /// Tables may have a primary key spanning more than one column. Required by
    /// the goals; Postgres supports it.
    pub composite_pk: bool,
    /// The backend offers a `LISTEN`/`NOTIFY`-style channel, enabling the
    /// pg-notify message-bus driver (§16).
    pub listen_notify: bool,
    /// Data-modifying statements can return affected rows (`RETURNING`), so an
    /// insert/update/delete yields the resulting row without a follow-up query.
    pub returning: bool,
}

impl DbCapabilities {
    /// A capability set with every feature disabled — the safe starting point a
    /// driver enables fields from.
    pub const fn none() -> Self {
        DbCapabilities {
            row_level_security: false,
            composite_pk: false,
            listen_notify: false,
            returning: false,
        }
    }
}

impl Default for DbCapabilities {
    fn default() -> Self {
        DbCapabilities::none()
    }
}
