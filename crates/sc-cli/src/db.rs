//! Primary-database connection configuration for the `saltcorn` binary.
//!
//! [`DbConfig`] gathers where the primary Postgres database lives from three
//! places, in this order of authority:
//!
//! 1. **CLI flags** — either a single connection URL (`--database-url`) or the
//!    individual `host`/`port`/`user`/`password`/`db` parts (`--db-host` etc.).
//! 2. **The environment** — `DATABASE_URL`, or the conventional `PG*` variables.
//! 3. **The configuration file** — the environment selected out of
//!    `saltcorn.toml` ([`crate::config_file`]), which is where a deployment keeps
//!    production's, staging's and test's parameters side by side.
//!
//! A URL, wherever it comes from, wins wholesale over the parts; otherwise the
//! parts build a [`tokio_postgres::Config`]. Either way [`DbConfig::connect`]
//! yields a pooled [`PgDriver`] the CLI hands to the catalog.
//!
//! **Naming an environment inverts 2 and 3.** With no `--environment`, the
//! ambient variables outrank the file, which is the stated rule: the file
//! supplies what the environment does not. But `--environment staging` is an
//! instruction, and an operator who gives it on a box where `DATABASE_URL`
//! happens to point at production must not be quietly connected to production.
//! So a *named* environment whose section says anything is authoritative: the
//! `PG*`/`DATABASE_URL` variables are ignored entirely for that run, and only
//! explicit flags still override it. (Ignored *entirely*, not per field — a
//! section giving host and database, with `DATABASE_URL` still filling in the
//! URL, would be the same accident wearing a smaller hat.)
//!
//! Parsing is separated from resolution on purpose: [`DbConfig::extract`] records
//! what was passed on the command line and loads the selected environment (and
//! returns the arguments it did not consume, so the server flags parse cleanly
//! afterwards), while the environment fallbacks and defaults are applied later in
//! [`connect`](DbConfig::connect) / [`target`](DbConfig::target). No silent
//! failures: an unreadable value (e.g. a non-numeric port) is an error, and
//! connection failures are surfaced by the caller with the redacted
//! [`target`](DbConfig::target) for context.

use sc_db_postgres::PgDriver;
use sc_error::{Error, Result};

use crate::config_file::{self, Environment, SelectedEnvironment};

/// Default host when neither `--db-host` nor `PGHOST` is set.
const DEFAULT_HOST: &str = "localhost";
/// Default port when neither `--db-port` nor `PGPORT` is set.
const DEFAULT_PORT: u16 = 5432;

/// How to reach the primary database. The flag fields hold only what was passed
/// on the command line; the environment variables and the defaults are applied
/// at [`connect`](Self::connect). `selected` is the configuration file's
/// contribution, resolved at [`extract`](Self::extract) time because reading it
/// is I/O and every later accessor is infallible.
#[derive(Debug, Default, Clone)]
pub struct DbConfig {
    url: Option<String>,
    host: Option<String>,
    port: Option<String>,
    user: Option<String>,
    password: Option<String>,
    dbname: Option<String>,
    /// The environment selected out of `saltcorn.toml`, if there is one.
    selected: Option<SelectedEnvironment>,
}

impl DbConfig {
    /// A config that connects with the given URL (used by tests and callers that
    /// already hold a connection string).
    pub fn from_url(url: impl Into<String>) -> DbConfig {
        DbConfig {
            url: Some(url.into()),
            ..DbConfig::default()
        }
    }

    /// Pull the database flags out of `args`, returning the parsed config and the
    /// arguments that were **not** consumed (for the server config to parse).
    ///
    /// Recognised flags: `--database-url`, `--db-host`, `--db-port`, `--db-user`,
    /// `--db-password`, `--db-name`, plus `--environment` (which environment of
    /// the configuration file to use) and `--config` (which configuration file).
    /// Anything else is passed through untouched, so an unknown flag still fails
    /// loudly — in the server parser, not here.
    ///
    /// This also **loads the configuration file**, so that every command that
    /// takes database flags gets the file for free and none can forget to ask for
    /// it. A file that does not parse, or a named environment that does not
    /// exist, fails here — before anything connects.
    pub fn extract<I, S>(args: I) -> Result<(DbConfig, Vec<String>)>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut cfg = DbConfig::default();
        let mut environment: Option<String> = None;
        let mut config_path: Option<String> = None;
        let mut rest = Vec::new();
        let mut it = args.into_iter();
        while let Some(arg) = it.next() {
            let slot = match arg.as_ref() {
                "--database-url" => &mut cfg.url,
                "--db-host" => &mut cfg.host,
                "--db-port" => &mut cfg.port,
                "--db-user" => &mut cfg.user,
                "--db-password" => &mut cfg.password,
                "--db-name" => &mut cfg.dbname,
                "--environment" | "--env" => &mut environment,
                "--config" => &mut config_path,
                other => {
                    rest.push(other.to_owned());
                    continue;
                }
            };
            let flag = arg.as_ref().to_owned();
            *slot = Some(next_value(&mut it, &flag)?);
        }
        cfg.selected = config_file::select(config_path.as_deref(), environment.as_deref())?;
        Ok((cfg, rest))
    }

    /// Where the configuration file put us, for the startup log: `None` when no
    /// configuration file took part in this connection.
    pub fn source(&self) -> Option<String> {
        self.selected.as_ref().map(SelectedEnvironment::describe)
    }

    /// The name of the selected environment, if a configuration file was used.
    pub fn environment(&self) -> Option<&str> {
        self.selected.as_ref().map(|s| s.name.as_str())
    }

    /// Connect to the database, returning a pooled driver. A URL (from the flag,
    /// `DATABASE_URL` or the selected environment) is used wholesale; otherwise
    /// the individual parts — falling back to the `PG*` environment variables,
    /// the selected environment and then the host/port defaults — build the
    /// connection.
    ///
    /// This only builds the pool; the first real connection (and thus the first
    /// chance to observe an unreachable/misconfigured database) happens when the
    /// catalog introspects, so the caller wraps that with [`target`](Self::target).
    pub async fn connect(&self) -> Result<PgDriver> {
        if let Some(url) = self.resolved_url() {
            return PgDriver::connect(&url).await;
        }
        let mut config = tokio_postgres::Config::new();
        config.host(self.resolved_host());
        config.port(self.resolved_port()?);
        if let Some(user) = self.resolved(&self.user, "PGUSER", |e| e.user.clone()) {
            config.user(user);
        }
        if let Some(password) = self.resolved(&self.password, "PGPASSWORD", |e| e.password.clone())
        {
            config.password(password);
        }
        if let Some(dbname) = self.resolved(&self.dbname, "PGDATABASE", |e| e.database.clone()) {
            config.dbname(dbname);
        }
        PgDriver::from_config(&config)
    }

    /// A human-readable, **password-free** description of the target, for error
    /// messages. Never includes credentials.
    pub fn target(&self) -> String {
        match self.resolved_url() {
            Some(url) => redact(&url),
            None => self.target_from_parts(),
        }
    }

    /// The `host:port/db` description used when no connection URL is in play.
    fn target_from_parts(&self) -> String {
        format!(
            "{}:{}/{}",
            self.resolved_host(),
            self.port_string(),
            self.resolved(&self.dbname, "PGDATABASE", |e| e.database.clone())
                .unwrap_or_else(|| "<default>".to_owned()),
        )
    }

    /// The effective connection URL: the flag, then — in whichever order
    /// [`file_wins`](Self::file_wins) dictates — `DATABASE_URL` and the selected
    /// environment's `url`.
    fn resolved_url(&self) -> Option<String> {
        if let Some(url) = &self.url {
            return Some(url.clone());
        }
        let from_file = self.section().and_then(|e| e.url.clone());
        if self.file_wins() {
            return from_file;
        }
        env_var("DATABASE_URL").or(from_file)
    }

    /// The effective host: flag, else `PGHOST`/the file, else the default.
    fn resolved_host(&self) -> String {
        self.resolved(&self.host, "PGHOST", |e| e.host.clone())
            .unwrap_or_else(|| DEFAULT_HOST.to_owned())
    }

    /// The effective port as a `u16`, erroring if it is not a valid number.
    fn resolved_port(&self) -> Result<u16> {
        match self.resolved(&self.port, "PGPORT", |e| e.port.map(|p| p.to_string())) {
            Some(raw) => raw
                .parse()
                .map_err(|e| Error::config(format!("invalid database port `{raw}`: {e}"))),
            None => Ok(DEFAULT_PORT),
        }
    }

    /// The effective port rendered for [`target`](Self::target) (defaulted, never
    /// erroring — display only).
    fn port_string(&self) -> String {
        self.resolved(&self.port, "PGPORT", |e| e.port.map(|p| p.to_string()))
            .unwrap_or_else(|| DEFAULT_PORT.to_string())
    }

    /// One setting, resolved: the flag first, then the environment variable and
    /// the configuration file in whichever order [`file_wins`](Self::file_wins)
    /// dictates. `pick` reads the setting out of the file's section.
    fn resolved(
        &self,
        flag: &Option<String>,
        env: &str,
        pick: impl Fn(&Environment) -> Option<String>,
    ) -> Option<String> {
        if let Some(value) = flag {
            return Some(value.clone());
        }
        let from_file = self.section().and_then(pick);
        if self.file_wins() {
            return from_file;
        }
        env_var(env).or(from_file)
    }

    /// The selected environment's connection parameters, if any.
    fn section(&self) -> Option<&Environment> {
        self.selected.as_ref().map(|s| &s.section)
    }

    /// Whether the configuration file outranks the ambient `PG*`/`DATABASE_URL`
    /// variables: true exactly when the operator **named** an environment that
    /// says something. See the module docs for why naming one inverts the order.
    fn file_wins(&self) -> bool {
        self.selected
            .as_ref()
            .is_some_and(|s| s.explicit && !s.section.is_empty())
    }
}

/// Read an environment variable, treating empty as absent.
fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Take the value following a flag, erroring if it is missing.
fn next_value<I, S>(it: &mut I, flag: &str) -> Result<String>
where
    I: Iterator<Item = S>,
    S: AsRef<str>,
{
    it.next()
        .map(|s| s.as_ref().to_owned())
        .ok_or_else(|| Error::config(format!("{flag} requires a value")))
}

/// Remove any password from a `scheme://user:password@host/...` URL so it is safe
/// to print. Returns the input unchanged when there is no `user:password@` part.
fn redact(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_owned();
    };
    let after_scheme = scheme_end + 3;
    let rest = &url[after_scheme..];
    // The authority ends at the first '/', '?' or '#'.
    let authority_len = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_len);
    let Some(at) = authority.rfind('@') else {
        return url.to_owned();
    };
    let userinfo = &authority[..at];
    let host = &authority[at..]; // includes the '@'
    let user = userinfo.split(':').next().unwrap_or(userinfo);
    format!("{}{}:***{}{}", &url[..after_scheme], user, host, tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_pulls_db_flags_and_leaves_the_rest() {
        let (cfg, rest) = DbConfig::extract([
            "--bind",
            "0.0.0.0:80",
            "--db-host",
            "db.internal",
            "--db-port",
            "6543",
            "--db-user",
            "sc",
            "--db-password",
            "secret",
            "--db-name",
            "app",
            "--secure-cookies",
        ])
        .expect("extract");

        assert_eq!(cfg.host.as_deref(), Some("db.internal"));
        assert_eq!(cfg.port.as_deref(), Some("6543"));
        assert_eq!(cfg.user.as_deref(), Some("sc"));
        assert_eq!(cfg.password.as_deref(), Some("secret"));
        assert_eq!(cfg.dbname.as_deref(), Some("app"));
        // Non-database flags are handed back for the server parser, in order.
        assert_eq!(rest, ["--bind", "0.0.0.0:80", "--secure-cookies"]);
    }

    #[test]
    fn extract_takes_a_full_url() {
        let (cfg, rest) =
            DbConfig::extract(["--database-url", "postgres://u:p@h:5/db"]).expect("extract");
        assert_eq!(cfg.url.as_deref(), Some("postgres://u:p@h:5/db"));
        assert!(rest.is_empty());
    }

    #[test]
    fn extract_errors_on_a_flag_without_a_value() {
        assert!(DbConfig::extract(["--db-host"]).is_err());
    }

    #[test]
    fn a_bad_port_is_an_error_not_a_default() {
        let cfg = DbConfig {
            port: Some("not-a-port".to_owned()),
            ..DbConfig::default()
        };
        assert!(cfg.resolved_port().is_err());
    }

    #[test]
    fn target_hides_the_password() {
        let cfg = DbConfig::from_url("postgres://user:hunter2@host:5432/appdb");
        let target = cfg.target();
        assert!(!target.contains("hunter2"), "leaked password: {target}");
        assert!(target.contains("user"));
        assert!(target.contains("host:5432/appdb"));
    }

    #[test]
    fn target_from_parts_is_host_port_db_without_credentials() {
        let cfg = DbConfig {
            host: Some("h".to_owned()),
            port: Some("6000".to_owned()),
            dbname: Some("mydb".to_owned()),
            password: Some("secret".to_owned()),
            ..DbConfig::default()
        };
        // Call the parts formatter directly: whether `target()` takes the parts
        // branch depends on an ambient `DATABASE_URL`, which is set when running
        // the integration suite. The formatting and password omission are what
        // this test is about.
        let target = cfg.target_from_parts();
        assert_eq!(target, "h:6000/mydb");
        assert!(!target.contains("secret"));
    }

    #[test]
    fn redact_leaves_a_url_without_userinfo_unchanged() {
        assert_eq!(redact("postgres://host:5432/db"), "postgres://host:5432/db");
    }

    // --- The configuration file -------------------------------------------
    //
    // These drive `extract` with an explicit `--config`, never the search path,
    // so they cannot pick up (or be broken by) a real `saltcorn.toml` on the
    // machine running them. Nothing here sets an environment variable: doing so
    // is racy across the test threads, so the assertions are written to hold
    // whatever `DATABASE_URL`/`PG*` the suite happens to be run with — which is
    // itself the property most of them are about.

    const FIXTURE: &str = r#"
default_environment = "production"

[environments.production]
host = "prod.internal"
port = 5432
user = "sc"
password = "prod-pw"
database = "saltcorn"

[environments.staging]
url = "postgres://sc:staging-pw@staging.internal:5432/saltcorn"

[environments.test]
host = "localhost"
database = "saltcorn_test"

[environments.blank]
"#;

    /// A configuration file on disk for the duration of one test, removed when
    /// the handle drops.
    struct Fixture(std::path::PathBuf);

    impl Fixture {
        fn new(name: &str, contents: &str) -> Fixture {
            let path =
                std::env::temp_dir().join(format!("sc-cli-{}-{name}.toml", std::process::id()));
            std::fs::write(&path, contents).expect("write fixture");
            // 0600 both because the file holds passwords and so the loader's
            // world-readable warning does not fire on every test run.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                    .expect("chmod fixture");
            }
            Fixture(path)
        }

        fn path(&self) -> &str {
            self.0.to_str().expect("utf-8 fixture path")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn a_named_environment_supplies_the_connection_parts() {
        let file = Fixture::new("parts", FIXTURE);
        let (cfg, rest) = DbConfig::extract([
            "--environment",
            "test",
            "--config",
            file.path(),
            "--bind",
            "x",
        ])
        .expect("extract");

        assert_eq!(rest, ["--bind", "x"]);
        assert_eq!(cfg.environment(), Some("test"));
        // Named → the file is authoritative, so this holds even when the suite
        // is run with DATABASE_URL and PG* pointing elsewhere.
        assert_eq!(cfg.resolved_url(), None);
        assert_eq!(cfg.resolved_host(), "localhost");
        assert_eq!(cfg.resolved_port().expect("port"), 5432);
        assert_eq!(cfg.target(), "localhost:5432/saltcorn_test");
    }

    #[test]
    fn a_named_environment_may_carry_a_url_and_outranks_the_environment() {
        let file = Fixture::new("url", FIXTURE);
        let (cfg, _) = DbConfig::extract(["--environment", "staging", "--config", file.path()])
            .expect("extract");

        assert_eq!(
            cfg.resolved_url().as_deref(),
            Some("postgres://sc:staging-pw@staging.internal:5432/saltcorn"),
            "naming an environment must beat an ambient DATABASE_URL"
        );
        // ...and the startup line says which one, without the password.
        let source = cfg.source().expect("a source");
        assert!(source.contains("staging"), "{source}");
        assert!(!cfg.target().contains("staging-pw"), "{}", cfg.target());
    }

    #[test]
    fn a_flag_still_beats_a_named_environment() {
        let file = Fixture::new("flag", FIXTURE);
        let (cfg, _) = DbConfig::extract([
            "--environment",
            "staging",
            "--config",
            file.path(),
            "--database-url",
            "postgres://flag/db",
        ])
        .expect("extract");
        assert_eq!(cfg.resolved_url().as_deref(), Some("postgres://flag/db"));
    }

    #[test]
    fn without_a_flag_the_files_default_environment_is_used_and_yields_to_the_environment() {
        let file = Fixture::new("default", FIXTURE);
        let (cfg, _) = DbConfig::extract(["--config", file.path()]).expect("extract");

        assert_eq!(cfg.environment(), Some("production"));
        // Not *named*, so the ambient variables still come first: the file is
        // the fallback, which is the whole point of it.
        assert!(!cfg.file_wins());
        if std::env::var_os("PGHOST").is_none() {
            assert_eq!(cfg.resolved_host(), "prod.internal");
        }
        if std::env::var_os("DATABASE_URL").is_none() {
            assert_eq!(cfg.resolved_url(), None);
        }
    }

    #[test]
    fn an_empty_named_section_does_not_take_over_from_the_environment() {
        let file = Fixture::new("blank", FIXTURE);
        let (cfg, _) = DbConfig::extract(["--environment", "blank", "--config", file.path()])
            .expect("extract");
        assert_eq!(cfg.environment(), Some("blank"));
        assert!(
            !cfg.file_wins(),
            "a section that says nothing must not silence DATABASE_URL"
        );
    }

    #[test]
    fn an_undefined_environment_is_an_error() {
        let file = Fixture::new("undefined", FIXTURE);
        let err = DbConfig::extract(["--environment", "prod", "--config", file.path()])
            .expect_err("unknown environment must fail");
        assert!(err.to_string().contains("production, staging"), "{err}");
    }

    #[test]
    fn a_config_path_that_does_not_exist_is_an_error() {
        let missing = std::env::temp_dir().join("sc-cli-absent-config-4c1e.toml");
        let err = DbConfig::extract(["--config", missing.to_str().expect("path")])
            .expect_err("a named file that is absent must fail");
        assert!(err.to_string().contains("does not exist"), "{err}");
    }

    #[test]
    fn a_malformed_config_file_fails_at_parse_time_not_at_connect_time() {
        let file = Fixture::new("broken", "[environments.production\nhost = 'x'\n");
        assert!(DbConfig::extract(["--config", file.path()]).is_err());
    }
}
