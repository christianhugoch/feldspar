//! What a module *is*: the record an admin installs, and where it came from.
//!
//! Deliberately small. A module's actions, its configuration spec and its
//! version-behind-the-name are all read from the **package on disk** when it is
//! loaded (TODO decision 6); what is stored is the identity, the specifier that
//! would reinstall it, and the configuration the admin typed.

use sc_error::{Error, Result};
use sc_types::Attrs;
use uuid::Uuid;

use crate::permissions::ModulePermissions;

/// A module's stable identity — the primary key of its `_fd_modules` row.
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

/// Which language a module is written in, and therefore which host loads it
/// and which package manager installs it (§8).
///
/// **One table, one tab and one set of endpoints**, because a module is a module
/// to an admin: what the language decides is whether the package comes from npm
/// or from PyPI, whether it is loaded on a Deno worker or on the embedded
/// interpreter, and whether the permission set on the screen means anything
/// (§10: for Python it does not).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModuleLanguage {
    /// A Saltcorn v1 plugin: an npm package, loaded on a module worker.
    #[default]
    JavaScript,
    /// A Python plugin: a distribution in this server's Python environment,
    /// loaded on the embedded interpreter.
    Python,
}

impl ModuleLanguage {
    /// The stored and posted spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            ModuleLanguage::JavaScript => "javascript",
            ModuleLanguage::Python => "python",
        }
    }

    /// Parse the stored spelling, naming the alternatives on a miss.
    pub fn parse(s: &str) -> Result<ModuleLanguage> {
        match s {
            "javascript" => Ok(ModuleLanguage::JavaScript),
            "python" => Ok(ModuleLanguage::Python),
            other => Err(Error::invalid(format!(
                "unknown module language `{other}`; the languages are javascript, python"
            ))),
        }
    }

    /// The sources a module in this language can be installed from — one
    /// registry each, and the two both share: the local directory, and the
    /// **bundled** catalog that ships in this server's own release
    /// ([`crate::bundled`]).
    pub fn sources(self) -> &'static [ModuleSource] {
        match self {
            ModuleLanguage::JavaScript => &[
                ModuleSource::Npm,
                ModuleSource::Bundled,
                ModuleSource::Local,
            ],
            ModuleLanguage::Python => &[
                ModuleSource::Pypi,
                ModuleSource::Bundled,
                ModuleSource::Local,
            ],
        }
    }

    /// Whether a module in this language can come from `source`.
    pub fn allows(self, source: ModuleSource) -> bool {
        self.sources().contains(&source)
    }
}

/// Every language, in the order the admin UI offers them.
pub const MODULE_LANGUAGES: [ModuleLanguage; 2] =
    [ModuleLanguage::JavaScript, ModuleLanguage::Python];

/// Where a module's package comes from.
///
/// Four kinds, and the difference is entirely in what `location` means: a
/// registry specifier (`@saltcorn/mqtt@0.2.0` for npm, `saltcorn-mqtt>=0.2` for
/// PyPI), the **id** of a module bundled with this server (`rss`), or an
/// absolute path on this server's disk. Which of the two registries applies is
/// the module's [`ModuleLanguage`], and `local` means the same thing in both
/// languages: a directory, **copied** in rather than linked, for the reasons
/// [`crate::install`] gives — so a checkout's edits reach the server when it is
/// installed again and not before.
///
/// `bundled` is `local` with the path filled in by the server rather than by the
/// admin, and that indirection is the whole reason it is its own source: the
/// directory a bundled module is installed from is `<install prefix>/plugins/<id>`,
/// which is a different string on a developer's checkout, on a host running the
/// tarball, and on the same host after an upgrade that moved the prefix. Storing
/// the id keeps the row **reinstallable**, which storing the path would not
/// ([`crate::bundled`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleSource {
    /// A package from the npm registry.
    Npm,
    /// A distribution from the Python Package Index.
    Pypi,
    /// A module that ships with this server: the id of an entry in the bundled
    /// catalog ([`crate::bundled`]), resolved to a directory at install time.
    Bundled,
    /// A directory on this server's disk.
    Local,
}

impl ModuleSource {
    /// The stored and posted spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            ModuleSource::Npm => "npm",
            ModuleSource::Pypi => "pypi",
            ModuleSource::Bundled => "bundled",
            ModuleSource::Local => "local",
        }
    }

    /// Parse the stored spelling. An unrecognised one is an error naming it and
    /// the alternatives: a module whose source nothing understands must not
    /// silently become one that can never be reinstalled.
    pub fn parse(s: &str) -> Result<ModuleSource> {
        match s {
            "npm" => Ok(ModuleSource::Npm),
            "pypi" => Ok(ModuleSource::Pypi),
            "bundled" => Ok(ModuleSource::Bundled),
            "local" => Ok(ModuleSource::Local),
            other => Err(Error::invalid(format!(
                "unknown module source `{other}`; the sources are npm, pypi, bundled, local"
            ))),
        }
    }
}

/// Every source, in the order the admin UI offers them.
pub const MODULE_SOURCES: [ModuleSource; 4] = [
    ModuleSource::Npm,
    ModuleSource::Pypi,
    ModuleSource::Bundled,
    ModuleSource::Local,
];

/// An installed module: the row, and nothing the package could contradict.
#[derive(Debug, Clone)]
pub struct Module {
    /// Stable identity.
    pub id: ModuleId,
    /// The package name — `@saltcorn/mqtt`. The key everything resolves
    /// through, and what `node_modules/<name>` is called on disk.
    pub name: String,
    /// Which language it is written in (§8), and therefore which host loads it
    /// and which package manager installed it.
    pub language: ModuleLanguage,
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
    /// What the module's worker may reach: a net allow-list, readable and
    /// writable paths, and the environment variables it may see (§2).
    ///
    /// **Closed on install** and widened only by an admin. It is stored on the
    /// row rather than read from the package because it is the *server's*
    /// decision and not the module's: what a package declares is a request, and
    /// a request that granted itself would be no permission model at all.
    pub permissions: ModulePermissions,
    /// The sparse per-module values column (§9).
    pub attributes: Attrs,
}

impl Module {
    /// The same module, in `language` — the one thing about a module that is
    /// decided before its package has been read, because it decides which
    /// package manager reads it.
    #[must_use]
    pub fn in_language(mut self, language: ModuleLanguage) -> Module {
        self.language = language;
        self
    }

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
            language: ModuleLanguage::JavaScript,
            source,
            location: location.into(),
            version: None,
            configuration: Attrs::new(),
            permissions: ModulePermissions::closed(),
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
    fn a_language_round_trips_and_owns_its_registry() {
        for language in MODULE_LANGUAGES {
            assert_eq!(ModuleLanguage::parse(language.as_str()).unwrap(), language);
            // Every language has the local directory and the bundled catalog,
            // and exactly one registry.
            assert!(language.allows(ModuleSource::Local));
            assert!(language.allows(ModuleSource::Bundled));
        }
        assert!(ModuleLanguage::JavaScript.allows(ModuleSource::Npm));
        assert!(!ModuleLanguage::JavaScript.allows(ModuleSource::Pypi));
        assert!(ModuleLanguage::Python.allows(ModuleSource::Pypi));
        assert!(!ModuleLanguage::Python.allows(ModuleSource::Npm));
        let msg = ModuleLanguage::parse("ruby").unwrap_err().to_string();
        assert!(msg.contains("ruby") && msg.contains("python"), "{msg}");
    }

    #[test]
    fn a_new_module_has_no_version_until_it_is_installed() {
        let module = Module::new("@saltcorn/mqtt", ModuleSource::Npm, "@saltcorn/mqtt");
        assert!(module.version.is_none());
        // JavaScript unless somebody says otherwise: the language nearly every
        // module is written in should not have to be named at every call site.
        assert_eq!(module.language, ModuleLanguage::JavaScript);
        assert!(module.configuration.is_empty());
        // And it reaches nothing until an admin says otherwise (§2).
        assert!(module.permissions.is_closed());
    }
}
