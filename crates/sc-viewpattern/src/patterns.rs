//! The view-pattern registry: which pattern names a view may be saved with
//! (TODO "Saltcorn UI" §1, §6).
//!
//! v1's six built-in patterns are always registered. A module may declare more
//! (`viewtemplates`, Phase 11); they are installed here whole on every module
//! change, the way the frameworks a module declares are installed in `sc-app`.
//! Rendering is not this registry's business — at this layer a pattern is a name
//! and whether it needs a table, which is everything save-time validation asks.

use std::sync::RwLock;

use sc_error::{Error, Result};

/// v1's built-in patterns, spelled as v1's `viewtemplates` name them. Not `Room`
/// and not `WorkflowRoom`: both are realtime, and out of this milestone.
pub const BUILTIN_PATTERNS: [&str; 6] = ["List", "Show", "Edit", "Feed", "Filter", "ListShowList"];

/// One registered view pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternInfo {
    /// The name a view's `viewpattern` holds.
    pub name: String,
    /// Whether a view of this pattern has no table (v1's `tableless`).
    pub tableless: bool,
    /// The module that declared it; `None` for a built-in.
    pub module: Option<String>,
}

/// The installed module patterns — empty until a module declares one.
static INSTALLED: RwLock<Vec<PatternInfo>> = RwLock::new(Vec::new());

/// v1's six built-in patterns. Every one is over a table.
pub fn builtin_patterns() -> Vec<PatternInfo> {
    BUILTIN_PATTERNS
        .iter()
        .map(|name| PatternInfo {
            name: (*name).to_owned(),
            tableless: false,
            module: None,
        })
        .collect()
}

/// Install the patterns the modules declare, replacing whatever was installed.
///
/// A poisoned lock is reported rather than panicked on, and leaves the previous
/// set in place.
pub fn install_patterns(patterns: Vec<PatternInfo>) -> Result<()> {
    let mut guard = INSTALLED
        .write()
        .map_err(|_| Error::msg("the view pattern registry lock is poisoned"))?;
    *guard = patterns;
    Ok(())
}

/// Every registered pattern: the built-ins first, then the installed ones.
pub fn registered_patterns() -> Vec<PatternInfo> {
    let installed = match INSTALLED.read() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    builtin_patterns().into_iter().chain(installed).collect()
}

/// The pattern named `name` in `patterns`, if any.
pub fn find_pattern<'a>(patterns: &'a [PatternInfo], name: &str) -> Option<&'a PatternInfo> {
    patterns.iter().find(|p| p.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_six_built_in_patterns_are_registered_and_need_a_table() {
        let registered = registered_patterns();
        for name in BUILTIN_PATTERNS {
            let pattern = find_pattern(&registered, name);
            assert!(
                pattern.is_some_and(|p| !p.tableless && p.module.is_none()),
                "{name}"
            );
        }
        // Realtime patterns are out of this milestone.
        assert!(find_pattern(&registered, "Room").is_none());
    }
}
