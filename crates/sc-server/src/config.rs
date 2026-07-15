//! Server configuration (technical design §16).
//!
//! [`ServerConfig`] is the small, plain settings value the CLI builds and hands
//! to [`serve`](crate::serve): where to bind, where the built `ui/admin` bundle
//! lives, session lifetime, and whether cookies carry the `Secure` attribute.
//! [`ServerConfig::from_args`] parses the handful of flags the `saltcorn serve`
//! command accepts, so the binary needs no argument-parsing dependency.

use std::net::SocketAddr;
use std::path::PathBuf;

use sc_error::{Error, Result};

/// Default address the server binds when `--bind` is not given.
pub const DEFAULT_BIND: &str = "127.0.0.1:3000";

/// Runtime configuration for the HTTP server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// The socket address to bind.
    pub addr: SocketAddr,
    /// Directory holding the built `ui/admin` SPA bundle, if any. When unset the
    /// server still serves the minimal bootstrap document so the SPA can boot
    /// once built.
    pub static_dir: Option<PathBuf>,
    /// Session lifetime in hours.
    pub session_ttl_hours: i64,
    /// Whether the session/CSRF cookies carry the `Secure` attribute (set behind
    /// TLS; off for plain-HTTP local development).
    pub secure_cookies: bool,
    /// The domain applications are served under: an app with subdomain `blog` is
    /// served at `blog.<base_domain>` (design §13.2).
    ///
    /// `None` (the default) disables subdomain app routing entirely, so every
    /// request reaches the admin. App routing is opt-in because without a base
    /// domain to anchor it, a request's own `Host` header would choose its app.
    pub base_domain: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            // `DEFAULT_BIND` is a valid literal, so parsing it cannot fail.
            addr: DEFAULT_BIND
                .parse()
                .unwrap_or_else(|_| SocketAddr::from(([127, 0, 0, 1], 3000))),
            static_dir: None,
            session_ttl_hours: sc_auth::DEFAULT_TTL_HOURS,
            secure_cookies: false,
            base_domain: None,
        }
    }
}

impl ServerConfig {
    /// Parse configuration from CLI arguments (everything after the subcommand).
    ///
    /// Recognised flags: `--bind <addr>`, `--static-dir <path>`,
    /// `--session-ttl-hours <n>`, `--secure-cookies`, and `--base-domain
    /// <domain>`. Unknown flags are an [`Error::Config`], so a typo fails loudly
    /// rather than being ignored.
    pub fn from_args<I, S>(args: I) -> Result<ServerConfig>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut cfg = ServerConfig::default();
        let mut it = args.into_iter();
        while let Some(arg) = it.next() {
            match arg.as_ref() {
                "--bind" => {
                    let raw = next_value(&mut it, "--bind")?;
                    cfg.addr = raw
                        .parse()
                        .map_err(|e| Error::config(format!("invalid --bind `{raw}`: {e}")))?;
                }
                "--static-dir" => {
                    cfg.static_dir = Some(PathBuf::from(next_value(&mut it, "--static-dir")?));
                }
                "--session-ttl-hours" => {
                    let raw = next_value(&mut it, "--session-ttl-hours")?;
                    cfg.session_ttl_hours = raw.parse().map_err(|e| {
                        Error::config(format!("invalid --session-ttl-hours `{raw}`: {e}"))
                    })?;
                }
                "--secure-cookies" => cfg.secure_cookies = true,
                "--base-domain" => {
                    cfg.base_domain = Some(next_value(&mut it, "--base-domain")?);
                }
                other => {
                    return Err(Error::config(format!("unknown server argument `{other}`")));
                }
            }
        }
        Ok(cfg)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_bind_to_localhost_3000() {
        let cfg = ServerConfig::default();
        assert_eq!(cfg.addr.to_string(), "127.0.0.1:3000");
        assert!(cfg.static_dir.is_none());
        assert!(!cfg.secure_cookies);
        // App subdomain routing is opt-in.
        assert!(cfg.base_domain.is_none());
    }

    #[test]
    fn parses_all_flags() {
        let cfg = ServerConfig::from_args([
            "--bind",
            "0.0.0.0:8080",
            "--static-dir",
            "/srv/admin",
            "--session-ttl-hours",
            "12",
            "--secure-cookies",
            "--base-domain",
            "example.com",
        ])
        .expect("parse");
        assert_eq!(cfg.addr.to_string(), "0.0.0.0:8080");
        assert_eq!(
            cfg.static_dir.as_deref(),
            Some(std::path::Path::new("/srv/admin"))
        );
        assert_eq!(cfg.session_ttl_hours, 12);
        assert!(cfg.secure_cookies);
        assert_eq!(cfg.base_domain.as_deref(), Some("example.com"));
    }

    #[test]
    fn rejects_bad_bind_and_unknown_flags() {
        assert!(ServerConfig::from_args(["--bind", "not-an-addr"]).is_err());
        assert!(ServerConfig::from_args(["--bind"]).is_err()); // missing value
        assert!(ServerConfig::from_args(["--nope"]).is_err()); // unknown flag
    }
}
