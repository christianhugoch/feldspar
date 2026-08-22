//! What a module *is*: the record an admin installs, and where it came from.
//!
//! Deliberately small. A module's actions, its configuration spec and its
//! version-behind-the-name are all read from the **package on disk** when it is
//! loaded (TODO decision 6); what is stored is the identity, the specifier that
//! would reinstall it, and the configuration the admin typed.

use sc_error::{Error, Result};
use sc_types::Attrs;
use uuid::Uuid;

/// A module's stable identity — the primary key of its `_sc_modules` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModuleId(pub Uuid);

impl ModuleId {
    /// A fresh identity for a module being installed.
    pub fn new() -> ModuleId {
        ModuleId(Uuid::new_v4())
    }
}

impl Default for ModuleId {
    fn default() -> ModuleId {
        ModuleId::new()
    }
}

impl std::fmt::Display for ModuleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Where a module's package comes from.
///
/// Two kinds, and the difference is entirely in what `location` means: a
/// registry specifier (`@saltcorn/mqtt`, `@saltcorn/mqtt@0.2.0`) or an absolute
/// path on this server's disk. npm installs both; a local directory is
/// **copied** in rather than symlinked, for the two reasons
/// [`crate::install`] gives, so a checkout's edits reach the server when it is
/// installed again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleSource {
    /// A package from the npm registry.
    Npm,
    /// A directory on this server's disk.
    Local,
}

impl ModuleSource {
    /// The stored and posted spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            ModuleSource::Npm => "npm",
            ModuleSource::Local => "local",
        }
    }

    /// Parse the stored spelling. An unrecognised one is an error naming it and
    /// the alternatives: a module whose source nothing understands must not
    /// silently become one that can never be reinstalled.
    pub fn parse(s: &str) -> Result<ModuleSource> {
        match s {
            "npm" => Ok(ModuleSource::Npm),
            "local" => Ok(ModuleSource::Local),
            other => Err(Error::invalid(format!(
                "unknown module source `{other}`; the sources are npm, local"
            ))),
        }
    }
}

/// Every source, in the order the admin UI offers them.
pub const MODULE_SOURCES: [ModuleSource; 2] = [ModuleSource::Npm, ModuleSource::Local];

/// An installed module: the row, and nothing the package could contradict.
#[derive(Debug, Clone)]
pub struct Module {
    /// Stable identity.
    pub id: ModuleId,
    /// The package name — `@saltcorn/mqtt`. The key everything resolves
    /// through, and what `node_modules/<name>` is called on disk.
    pub name: String,
    /// Where the package came from.
    pub source: ModuleSource,
    /// The npm specifier or the local directory, as the admin gave it. Kept so
    /// a reinstall asks for the same thing rather than for whatever the name
    /// happens to resolve to on the registry today.
    pub location: String,
    /// The version actually installed, read back from the package's own
    /// `package.json`. `None` until an install has succeeded.
    pub version: Option<String>,
    /// The module's own configuration — v1's plugin configuration, the object
    /// handed to `actions(cfg)` (§5).
    pub configuration: Attrs,
    /// The sparse per-module values column (§9).
    pub attributes: Attrs,
}

impl Module {
    /// A module about to be installed from `location`.
    ///
    /// The name is provisional for an npm install (the specifier may carry a
    /// version, `@saltcorn/mqtt@0.2.0`) and unknown for a local one until the
    /// package's `package.json` has been read — [`crate::install`] fills both in
    /// from the installed package, which is the only authority on either.
    pub fn new(
        name: impl Into<String>,
        source: ModuleSource,
        location: impl Into<String>,
    ) -> Module {
        Module {
            id: ModuleId::new(),
            name: name.into(),
            source,
            location: location.into(),
            version: None,
            configuration: Attrs::new(),
            attributes: Attrs::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_round_trips_through_its_stored_spelling() {
        for source in MODULE_SOURCES {
            assert_eq!(ModuleSource::parse(source.as_str()).unwrap(), source);
        }
    }

    #[test]
    fn an_unknown_source_names_itself_and_the_alternatives() {
        let msg = ModuleSource::parse("github").unwrap_err().to_string();
        assert!(msg.contains("github"), "{msg}");
        assert!(msg.contains("npm") && msg.contains("local"), "{msg}");
    }

    #[test]
    fn a_new_module_has_no_version_until_it_is_installed() {
        let module = Module::new("@saltcorn/mqtt", ModuleSource::Npm, "@saltcorn/mqtt");
        assert!(module.version.is_none());
        assert!(module.configuration.is_empty());
    }
}
