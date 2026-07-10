//! Single Error enum and Result alias; no-silent-failure helpers (layer 0)
//!
//! Skeleton crate for the Saltcorn v2 workspace. Functionality is filled in by
//! later TODO items.

/// Returns this crate's name. Placeholder so the skeleton has something to test
/// until the real API lands.
pub fn crate_name() -> &'static str {
    "sc-error"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_is_set() {
        assert_eq!(crate_name(), "sc-error");
    }
}
