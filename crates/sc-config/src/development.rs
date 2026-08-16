//! The Development settings: how much this server says, and whether it prints
//! the SQL it runs.
//!
//! Two switches an admin ticks while they are debugging something, and unticks
//! afterwards. They are settings rather than command-line flags for the reason
//! the certificate is (§13.5): the moment you want the SQL is the moment the
//! server is already running and doing the thing you cannot explain, and
//! restarting it with a flag is how you lose the state that was interesting.
//!
//! Both take effect **immediately** on save. Nothing here is read once at
//! startup and cached: the switches live in [`sc_log`] as process-wide atomics,
//! [`DevelopmentSettings::apply`] stores them, and the next statement or the
//! next request reads them. That is the whole reason the logging switches are
//! globals rather than values threaded through the call graph — the Postgres
//! driver is four layers below the settings store and must not know it exists.
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

/// The development settings, as one section of the settings screen.
pub fn development_section() -> ConfigSection {
    ConfigSection {
        name: "development",
        label: "Development",
        description: "What this server prints while it runs. Both settings take effect \
                      immediately — nothing here needs a restart — and both are for finding out \
                      what a running installation is doing, not for leaving on.",
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
                 about to fail; info logs every server request, one line each with its method, \
                 path, status and duration; verbose also logs a request as it arrives, so one \
                 that hangs is visible before it finishes; trace adds its headers, with \
                 cookies and authorization redacted.",
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

/// Read the development settings out of `_sc_config`.
pub async fn development_settings(catalog: &Catalog) -> Result<DevelopmentSettings> {
    development_settings_from(&crate::store::all_config(catalog).await?)
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
