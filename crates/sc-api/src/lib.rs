//! ApiProvider trait and REST provider (layer 8)
//!
//! Skeleton crate for the Saltcorn v2 workspace. Functionality is filled in by
//! later TODO items.

/// Returns this crate's name. Placeholder so the skeleton has something to test
/// until the real API lands.
pub fn crate_name() -> &'static str {
    "sc-api"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_is_set() {
        assert_eq!(crate_name(), "sc-api");
    }
}
