//! The Development settings: how much this server says, whether it prints the
//! SQL it runs, and whether an external coding agent may administer it.
//!
//! Switches an admin ticks while they are working on something, and unticks
//! afterwards. They are settings rather than command-line flags for the reason
//! the certificate is (§13.5): the moment you want the SQL is the moment the
//! server is already running and doing the thing you cannot explain, and
//! restarting it with a flag is how you lose the state that was interesting.
//!
//! All of them take effect **immediately** on save. Nothing here is read once at
//! startup and cached: the two logging switches live in [`sc_log`] as
//! process-wide atomics, [`DevelopmentSettings::apply`] stores them, and the next
//! statement or the next request reads them. That is the whole reason those two
//! are globals rather than values threaded through the call graph — the Postgres
//! driver is four layers below the settings store and must not know it exists.
//! The two MCP switches are *not* globals and deliberately so ([`McpSettings`]):
//! one route reads them, and it can read them where it is.
//!
//! What each verbosity means is [`sc_log::Verbosity`]'s to say; this module
//! declares the keys, reads them back, and hands them over.

use sc_catalog::Catalog;
use sc_error::Result;
use sc_log::Verbosity;
use sc_types::{Attrs, BasicType, FormField};
use serde_json::Value as Json;

use crate::defs::{ConfigDef, ConfigSection};

/// Whether every statement sent to the database is echoed to stdout.
pub const LOG_SQL: &str = "log_sql";
/// How much this server says: one of [`Verbosity`]'s spellings.
pub const LOG_VERBOSITY: &str = "log_verbosity";
/// Whether `POST /mcp` — the administration MCP server of §13.6 — is served at
/// all.
pub const MCP_ENABLED: &str = "mcp_enabled";
/// Whether that route refuses a peer that is not on this machine.
pub const MCP_LOOPBACK_ONLY: &str = "mcp_loopback_only";

/// The development settings, as one section of the settings screen.
pub fn development_section() -> ConfigSection {
    ConfigSection {
        name: "development",
        label: "Development",
        description: "What this server prints while it runs, and whether an external coding \
                      agent may administer it. Every setting here takes effect immediately — \
                      nothing needs a restart — and every one of them is for working on an \
                      installation, not for leaving on.",
        fields: vec![
            ConfigDef::help(
                FormField::new(LOG_SQL, BasicType::Bool)
                    .label("Log SQL")
                    .default_value(false),
                "Print every statement this server sends to the database — with its bind \
                 parameters — to stdout. Those parameters are the data: password hashes, \
                 session tokens and every row that passes through. Leave it off on anything \
                 whose output is kept.",
            ),
            ConfigDef::help(
                FormField::new(LOG_VERBOSITY, BasicType::Text)
                    .label("Log verbosity")
                    .options(Verbosity::ALL.map(Verbosity::as_str))
                    .default_value(sc_log::DEFAULT_VERBOSITY.as_str()),
                "How much is printed to stderr. error is failures only; warning adds what is \
                 about to fail; info logs every server request — one line each with its \
                 method, path, status and duration — and every LLM call with its token cost; \
                 verbose also logs a request and a model call as they start, so one that hangs \
                 is visible before it finishes; trace adds the whole of what an LLM was sent \
                 and answered and every tool call's arguments and result, which is a \
                 transcript of what the people using an agent typed.",
            ),
            // §13.6. Off by default, and off means *absent*: the route answers
            // 404 and never reads the token table, so a disabled feature is not
            // distinguishable from one this build does not have.
            ConfigDef::help(
                FormField::new(MCP_ENABLED, BasicType::Bool)
                    .label("Administration MCP server")
                    .default_value(false),
                "Serve POST /mcp, so an external coding agent holding an API token can read \
                 and change this installation's schema, triggers and applications. A token is \
                 an administrator: it runs with the full authority of the admin who minted it, \
                 bounded only by the grants ticked when it was made. Off means the route \
                 answers 404 and no token is looked at. Mint and revoke tokens below.",
            ),
            ConfigDef::help(
                FormField::new(MCP_LOOPBACK_ONLY, BasicType::Bool)
                    .label("MCP from this machine only")
                    .default_value(true),
                "Refuse an MCP request whose peer is not on this machine. The usual \
                 arrangement is an agent running beside the server or reaching it down a \
                 tunnel the developer made, and an installation that will never be \
                 administered from elsewhere should be able to say so here rather than in a \
                 reverse proxy. Turn it off only if the agent genuinely runs on another host — \
                 and note that a proxy in front of this server is itself a local peer, so the \
                 check protects nothing an untrusted proxy forwards.",
            ),
        ],
    }
}

/// The development settings as the process acts on them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevelopmentSettings {
    /// Whether SQL is echoed to stdout.
    pub log_sql: bool,
    /// How much is printed to stderr.
    pub verbosity: Verbosity,
}

impl Default for DevelopmentSettings {
    fn default() -> Self {
        DevelopmentSettings {
            log_sql: false,
            verbosity: sc_log::DEFAULT_VERBOSITY,
        }
    }
}

impl DevelopmentSettings {
    /// Make these settings the ones this process runs under.
    ///
    /// Idempotent, and the only way the switches move: everything that changes
    /// them — the boot path, a save in the admin UI — comes through here, so
    /// there is one place where "the stored setting" becomes "what the process
    /// does".
    pub fn apply(self) {
        sc_log::set_log_sql(self.log_sql);
        sc_log::set_verbosity(self.verbosity);
    }
}

/// Read the development settings out of a settings bag (stored values over
/// declared defaults).
pub fn development_settings_from(config: &Attrs) -> Result<DevelopmentSettings> {
    let defaults = DevelopmentSettings::default();
    Ok(DevelopmentSettings {
        log_sql: match config.get(LOG_SQL) {
            Some(Json::Bool(b)) => *b,
            _ => defaults.log_sql,
        },
        verbosity: match config.get(LOG_VERBOSITY) {
            // An unrecognised level is an error rather than a silent fallback,
            // for the reason an unrecognised `ssl_mode` is: a server that says
            // less than the admin asked it to, without saying so, is a server
            // whose logs cannot be trusted to be complete.
            Some(Json::String(s)) => Verbosity::parse(s)?,
            _ => defaults.verbosity,
        },
    })
}

/// Read the development settings out of `_fd_config`.
pub async fn development_settings(catalog: &Catalog) -> Result<DevelopmentSettings> {
    development_settings_from(&crate::store::all_config(catalog).await?)
}

/// Whether the administration MCP server is served, and to whom (§13.6).
///
/// Deliberately **not** part of [`DevelopmentSettings`], which is the pair of
/// switches a process *applies* to itself: these two are read where they are
/// enforced, by the one route that enforces them, on the request that asks.
/// There is nothing to cache in a global and nothing to apply at boot — a route
/// that read a copy taken at startup would be a switch that needs a restart,
/// which is the thing this section exists not to need.
///
/// The cost is two values read out of `_fd_config` per MCP request. A request
/// here is an agent's tool call rather than a page load, and the alternative — a
/// cached copy invalidated on save — is a second thing to keep in step for a
/// query that is already smaller than the work the call is about to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpSettings {
    /// Whether `POST /mcp` exists at all. Off means `404`.
    pub enabled: bool,
    /// Whether a peer that is not on this machine is refused.
    pub loopback_only: bool,
}

impl Default for McpSettings {
    /// Off, and local-only when it is turned on — the two defaults of §13.6, in
    /// the one place either of them is written down for the reader rather than
    /// for the form.
    fn default() -> Self {
        McpSettings {
            enabled: false,
            loopback_only: true,
        }
    }
}

/// Read the MCP settings out of a settings bag (stored values over declared
/// defaults).
///
/// A value that is not a boolean reads as the default rather than as an error,
/// which is the opposite of [`development_settings_from`]'s treatment of an
/// unrecognised verbosity — and for the same underlying rule: fall back to the
/// *safer* answer. A junk verbosity would mean saying less than was asked, so it
/// is refused; a junk `mcp_enabled` means an administrative surface nobody
/// deliberately opened, so it stays shut.
pub fn mcp_settings_from(config: &Attrs) -> McpSettings {
    let defaults = McpSettings::default();
    let flag = |key: &str, default: bool| match config.get(key) {
        Some(Json::Bool(b)) => *b,
        _ => default,
    };
    McpSettings {
        enabled: flag(MCP_ENABLED, defaults.enabled),
        loopback_only: flag(MCP_LOOPBACK_ONLY, defaults.loopback_only),
    }
}

/// Read the MCP settings out of `_fd_config`.
pub async fn mcp_settings(catalog: &Catalog) -> Result<McpSettings> {
    Ok(mcp_settings_from(&crate::store::all_config(catalog).await?))
}

/// Read the stored development settings and make them this process's own — the
/// one call the boot path makes.
pub async fn apply_development_settings(catalog: &Catalog) -> Result<DevelopmentSettings> {
    let settings = development_settings(catalog).await?;
    settings.apply();
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn attrs(pairs: &[(&str, Json)]) -> Attrs {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    /// An installation nobody has been debugging prints failures and no SQL.
    #[test]
    fn an_empty_configuration_is_quiet() {
        let settings = development_settings_from(&Attrs::new()).unwrap();
        assert!(!settings.log_sql);
        assert_eq!(settings.verbosity, Verbosity::Warning);
    }

    #[test]
    fn the_stored_values_are_what_the_process_acts_on() {
        let settings = development_settings_from(&attrs(&[
            (LOG_SQL, json!(true)),
            (LOG_VERBOSITY, json!("info")),
        ]))
        .unwrap();
        assert!(settings.log_sql);
        assert_eq!(settings.verbosity, Verbosity::Info);
    }

    #[test]
    fn a_level_that_is_not_a_level_is_refused() {
        let err = development_settings_from(&attrs(&[(LOG_VERBOSITY, json!("loud"))]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("loud"), "{err}");
    }

    /// The dropdown offers exactly the levels the parser accepts, and the
    /// checkbox is a checkbox — what the settings screen renders comes from
    /// here and nowhere else.
    #[test]
    fn the_section_declares_a_checkbox_and_the_five_levels() {
        let section = development_section();
        assert_eq!(section.name, "development");
        assert_eq!(section.label, "Development");

        let log_sql = section
            .fields
            .iter()
            .find(|def| def.key() == LOG_SQL)
            .unwrap();
        assert_eq!(
            log_sql.field.base.type_,
            sc_types::TypeRef::Basic(BasicType::Bool)
        );
        assert_eq!(log_sql.field.base.label, "Log SQL");

        let verbosity = section
            .fields
            .iter()
            .find(|def| def.key() == LOG_VERBOSITY)
            .unwrap();
        let options: Vec<String> = verbosity
            .field
            .static_options()
            .iter()
            .map(|o| o.as_str().unwrap_or_default().to_owned())
            .collect();
        assert_eq!(options, ["error", "warning", "info", "verbose", "trace"]);
        for option in &options {
            Verbosity::parse(option).unwrap();
        }
    }

    /// The two switches of §13.6, as the settings screen renders them: off, and
    /// local-only when it is on. Four fields rather than the three the plan
    /// counted — §13.6 argues for the enable switch and then for the loopback
    /// one, and both are declared here.
    #[test]
    fn the_section_declares_the_two_mcp_switches() {
        let section = development_section();
        let keys: Vec<&str> = section.fields.iter().map(|def| def.key()).collect();
        assert_eq!(
            keys,
            [LOG_SQL, LOG_VERBOSITY, MCP_ENABLED, MCP_LOOPBACK_ONLY]
        );

        for (key, default) in [(MCP_ENABLED, false), (MCP_LOOPBACK_ONLY, true)] {
            let def = section.fields.iter().find(|def| def.key() == key).unwrap();
            assert_eq!(
                def.field.base.type_,
                sc_types::TypeRef::Basic(BasicType::Bool),
                "{key}"
            );
            assert_eq!(def.field.default, Some(json!(default)), "{key}");
            // The help is what an admin makes the decision from, and this one is
            // a decision about who may administer the installation.
            assert!(!def.help.is_empty(), "{key}");
        }
    }

    /// An installation nobody has turned it on for serves no MCP — and would
    /// refuse a remote peer if it did.
    #[test]
    fn an_untouched_installation_serves_no_mcp() {
        let settings = mcp_settings_from(&Attrs::new());
        assert!(!settings.enabled);
        assert!(settings.loopback_only);
        assert_eq!(settings, McpSettings::default());
    }

    #[test]
    fn the_stored_mcp_switches_are_what_the_route_acts_on() {
        let settings = mcp_settings_from(&attrs(&[
            (MCP_ENABLED, json!(true)),
            (MCP_LOOPBACK_ONLY, json!(false)),
        ]));
        assert!(settings.enabled);
        assert!(!settings.loopback_only);
    }

    /// A value that is not a boolean falls back to the *shut* answer rather than
    /// to an error: a malformed row must not be a way to open an administrative
    /// surface, and must not be a way to stop the server booting either.
    #[test]
    fn a_flag_that_is_not_a_flag_leaves_the_server_shut() {
        let settings = mcp_settings_from(&attrs(&[
            (MCP_ENABLED, json!("yes")),
            (MCP_LOOPBACK_ONLY, json!(0)),
        ]));
        assert!(!settings.enabled);
        assert!(settings.loopback_only);
    }

    /// Applying is what a save does, and the process is what changes.
    #[test]
    fn applying_moves_the_process_switches() {
        DevelopmentSettings {
            log_sql: true,
            verbosity: Verbosity::Trace,
        }
        .apply();
        assert!(sc_log::log_sql_enabled());
        assert_eq!(sc_log::verbosity(), Verbosity::Trace);
        assert!(sc_log::enabled(Verbosity::Info));

        DevelopmentSettings::default().apply();
        assert!(!sc_log::log_sql_enabled());
        assert_eq!(sc_log::verbosity(), sc_log::DEFAULT_VERBOSITY);
        assert!(!sc_log::enabled(Verbosity::Info));
    }
}
