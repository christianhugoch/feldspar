//! Primary-database connection configuration for the `saltcorn` binary.
//!
//! [`DbConfig`] gathers where the primary Postgres database lives from CLI flags
//! and, as a fallback, the environment — either a single connection URL
//! (`--database-url` / `DATABASE_URL`) or the individual `host`/`port`/`user`/
//! `password`/`db` parts (`--db-host` etc. / the conventional `PG*` variables).
//! A URL, when present, wins wholesale; otherwise the parts build a
//! [`tokio_postgres::Config`]. Either way [`DbConfig::connect`] yields a pooled
//! [`PgDriver`] the CLI hands to the catalog.
//!
//! Parsing is separated from resolution on purpose: [`DbConfig::extract`] records
//! only what was passed on the command line (and returns the arguments it did not
//! consume, so the server flags parse cleanly afterwards), while the environment
//! fallbacks and defaults are applied later in [`connect`](DbConfig::connect) /
//! [`target`](DbConfig::target). No silent failures: an unreadable value (e.g. a
//! non-numeric port) is an error, and connection failures are surfaced by the
//! caller with the redacted [`target`](DbConfig::target) for context.

use sc_db_postgres::PgDriver;
use sc_error::{Error, Result};

/// Default host when neither `--db-host` nor `PGHOST` is set.
const DEFAULT_HOST: &str = "localhost";
/// Default port when neither `--db-port` nor `PGPORT` is set.
const DEFAULT_PORT: u16 = 5432;

/// How to reach the primary database. Fields hold only what was passed on the
/// command line; environment fallbacks are applied at [`connect`](Self::connect).
#[derive(Debug, Default, Clone)]
pub struct DbConfig {
    url: Option<String>,
    host: Option<String>,
    port: Option<String>,
    user: Option<String>,
    password: Option<String>,
    dbname: Option<String>,
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
    /// `--db-password`, `--db-name`. Anything else is passed through untouched, so
    /// an unknown flag still fails loudly — in the server parser, not here.
    pub fn extract<I, S>(args: I) -> Result<(DbConfig, Vec<String>)>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut cfg = DbConfig::default();
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
                other => {
                    rest.push(other.to_owned());
                    continue;
                }
            };
            let flag = arg.as_ref().to_owned();
            *slot = Some(next_value(&mut it, &flag)?);
        }
        Ok((cfg, rest))
    }

    /// Connect to the database, returning a pooled driver. A URL (from the flag
    /// or `DATABASE_URL`) is used wholesale; otherwise the individual parts —
    /// falling back to the `PG*` environment variables and then the host/port
    /// defaults — build the connection.
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
        if let Some(user) = self.resolved(&self.user, "PGUSER") {
            config.user(user);
        }
        if let Some(password) = self.resolved(&self.password, "PGPASSWORD") {
            config.password(password);
        }
        if let Some(dbname) = self.resolved(&self.dbname, "PGDATABASE") {
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
            self.resolved(&self.dbname, "PGDATABASE")
                .unwrap_or_else(|| "<default>".to_owned()),
        )
    }

    /// The effective connection URL: the flag, else `DATABASE_URL`, else none.
    fn resolved_url(&self) -> Option<String> {
        self.url.clone().or_else(|| env_var("DATABASE_URL"))
    }

    /// The effective host: flag, else `PGHOST`, else the default.
    fn resolved_host(&self) -> String {
        self.resolved(&self.host, "PGHOST")
            .unwrap_or_else(|| DEFAULT_HOST.to_owned())
    }

    /// The effective port as a `u16`, erroring if it is not a valid number.
    fn resolved_port(&self) -> Result<u16> {
        match self.resolved(&self.port, "PGPORT") {
            Some(raw) => raw
                .parse()
                .map_err(|e| Error::config(format!("invalid database port `{raw}`: {e}"))),
            None => Ok(DEFAULT_PORT),
        }
    }

    /// The effective port rendered for [`target`](Self::target) (defaulted, never
    /// erroring — display only).
    fn port_string(&self) -> String {
        self.resolved(&self.port, "PGPORT")
            .unwrap_or_else(|| DEFAULT_PORT.to_string())
    }

    /// A flag value, falling back to the named environment variable.
    fn resolved(&self, flag: &Option<String>, env: &str) -> Option<String> {
        flag.clone().or_else(|| env_var(env))
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
}
