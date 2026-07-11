//! HTTP server (axum): mounts the typed API, serves the `ui/admin` React SPA
//! bundle, sessions + strict CSP (layer 9; technical design §12, §16).
//!
//! Skeleton crate for the Saltcorn v2 workspace. The admin UI is a React SPA over
//! a typed JSON API — there is no server-rendered admin HTML. Functionality is
//! filled in by later TODO items.

/// Returns this crate's name. Placeholder so the skeleton has something to test
/// until the real API lands.
pub fn crate_name() -> &'static str {
    "sc-server"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_is_set() {
        assert_eq!(crate_name(), "sc-server");
    }
}
