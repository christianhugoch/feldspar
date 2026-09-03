//! Server configuration (technical design §16).
//!
//! [`ServerConfig`] is the small, plain settings value the CLI builds and hands
//! to [`serve`](crate::serve): where to bind, where the built `ui/admin` bundle
//! lives, session lifetime, and whether cookies carry the `Secure` attribute.
//! [`ServerConfig::from_args`] parses the handful of flags the `feldspar serve`
//! command accepts, so the binary needs no argument-parsing dependency.

use std::net::SocketAddr;
use std::path::PathBuf;

use sc_error::{Error, Result};

use crate::tls::TlsSettings;

/// What `--python` was set to: whether this process starts the interpreter it
/// has (§7's second switch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PythonMode {
    /// The default: start the interpreter when something first needs it, and not
    /// before. A server that fires no Python body and loads no Python module
    /// never starts one, exactly as the code isolate pool is never built.
    #[default]
    Auto,
    /// Do not start one at all. For an operator who has a Python-capable binary
    /// and wants this deployment not to run Python — the Python trigger then
    /// fails naming the flag.
    Off,
}

/// Default address the server binds when `--bind` is not given.
pub const DEFAULT_BIND: &str = "127.0.0.1:3032";

/// Runtime configuration for the HTTP server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// The socket address to bind.
    pub addr: SocketAddr,
    /// Directory holding the built `ui/admin` SPA bundle, if any. When unset the
    /// server still serves the minimal bootstrap document so the SPA can boot
    /// once built.
    pub static_dir: Option<PathBuf>,
    /// Directory holding the built `ui/ide` bundle — the file-store IDE, served
    /// under `/ide/` (design §12.1).
    ///
    /// **Not a command-line setting.** The IDE is part of the admin UI as far as
    /// an operator is concerned: it is built with it and served with it, so there
    /// is nothing to decide and no flag to forget. The binary fills this in from
    /// the bundle it was built with, or from the checkout it was built in (see
    /// `sc-cli`); a test points it at a directory of its own.
    pub ide_dir: Option<PathBuf>,
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
    /// How many V8 isolates the **code** pool runs (`--code-workers`), and how
    /// many runs each of them keeps resident at once (`--code-max-inflight`).
    ///
    /// A `run_js_code` body's run costs a pending promise rather than a thread,
    /// so the worker count buys CPU parallelism and the admission bound buys
    /// occupancy: the server serves `code_workers × code_max_inflight` bodies at
    /// once (512 by default) and queues the rest, with the queue time still
    /// inside each run's own deadline. Past that the ceiling is the database
    /// connection pool, which is where it belongs (design §10.1).
    ///
    /// Flags rather than stored settings because both are properties of *this
    /// process's* machine — its cores and its memory — not of the application,
    /// and a node with more of either should be able to say so without every
    /// other node against the same database hearing it.
    pub code_workers: usize,
    /// Runs each code isolate admits at once — see [`code_workers`].
    ///
    /// [`code_workers`]: ServerConfig::code_workers
    pub code_max_inflight: usize,
    /// How many Deno workers the **module** pool runs (`--module-workers`).
    ///
    /// One by default, because that is what the `node` sidecar it replaces
    /// already is: one runtime holding every module, not one per module. A
    /// module is pinned to a worker for its lifetime — its `require` cache and
    /// its module-level state (an MQTT client with a reconnect timer, a
    /// configured geocoder) live there — so the reason to run a second worker is
    /// **blast radius** and not throughput: the JS-slice watchdog stops an
    /// isolate and everything resident on it, so a module that must not share a
    /// runaway's fate wants a worker to itself.
    ///
    /// A flag rather than a stored setting, for the reason `--code-workers` is
    /// one: it is a property of *this process's* machine and not of the
    /// installation every node shares.
    ///
    pub module_workers: usize,
    /// Where installed **modules** live: the npm project the server installs
    /// packages into and runs the module host in (TODO "Modules", §1).
    ///
    /// `None` means the platform's data directory
    /// (`sc_module::default_modules_root`). It is a flag rather than a stored
    /// setting for the reason `--code-workers` is one: it is a property of
    /// *this machine* — which disk has room, which directory the service
    /// account may write — and not of the installation every node shares.
    pub modules_dir: Option<PathBuf>,
    /// Whether this process may **start** the Python interpreter it was built
    /// with (`--python off|auto`, default `auto`).
    ///
    /// Not whether it *has* one: that is a Cargo feature and a rebuild (§7). A
    /// binary built without `python` has no interpreter and no flag adds one; a
    /// binary built with it registers `run_python_code` either way, so what
    /// `--python off` changes is the sentence a Python trigger fails with — the
    /// flag, rather than a missing action.
    pub python: PythonMode,
    /// How many Python runs may be resident at once (`--python-max-inflight`),
    /// and how many stuck threads are tolerated (`--python-max-stuck`).
    ///
    /// One admission bound over one interpreter, so this is the Python
    /// counterpart of `--code-max-inflight` and not of `--code-workers`: there
    /// is no second interpreter to run. A resident run costs a thread (about
    /// 40 KB), which is why the default is small and raising it is cheap.
    pub python_max_inflight: usize,
    /// Stuck run threads tolerated before new runs are refused — see
    /// [`python_max_inflight`].
    ///
    /// [`python_max_inflight`]: ServerConfig::python_max_inflight
    pub python_max_stuck: usize,
    /// The virtual environment Python modules are installed into
    /// (`--python-dir`), and the external interpreter `pip` runs under
    /// (`--python-bin`) — §9's two halves, which are a machine's properties for
    /// the reason `--modules-dir` is.
    ///
    /// `None` in either is the default: the platform data directory beside the
    /// modules root, and `python3` on the path.
    pub python_env: sc_python::PythonEnv,
    /// How this server obtains the certificate it serves HTTPS with (§13.5).
    ///
    /// **Not a command-line setting**, deliberately: certificates are edited in
    /// the admin UI and stored in `_sc_config`, so every node against one
    /// database serves the same thing and a renewal is not a deploy. The boot
    /// path reads the settings and fills this in
    /// ([`TlsSettings::from_ssl`](crate::tls::TlsSettings::from_ssl)); the
    /// default is [`Off`](TlsSettings::Off), which is plain HTTP.
    pub tls: TlsSettings,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            // `DEFAULT_BIND` is a valid literal, so parsing it cannot fail.
            addr: DEFAULT_BIND
                .parse()
                .unwrap_or_else(|_| SocketAddr::from(([127, 0, 0, 1], 3032))),
            static_dir: None,
            ide_dir: None,
            session_ttl_hours: sc_auth::DEFAULT_TTL_HOURS,
            secure_cookies: false,
            base_domain: None,
            code_workers: sc_expr::DEFAULT_CODE_WORKERS,
            code_max_inflight: sc_expr::DEFAULT_MAX_INFLIGHT,
            module_workers: sc_module::DEFAULT_MODULE_WORKERS,
            modules_dir: None,
            python: PythonMode::Auto,
            python_max_inflight: sc_python::DEFAULT_MAX_INFLIGHT,
            python_max_stuck: sc_python::DEFAULT_MAX_STUCK,
            python_env: sc_python::PythonEnv::default(),
            tls: TlsSettings::Off,
        }
    }
}

impl ServerConfig {
    /// Parse configuration from CLI arguments (everything after the subcommand).
    ///
    /// Recognised flags: `--bind <addr>`, `--static-dir <path>`,
    /// `--session-ttl-hours <n>`, `--secure-cookies`, `--base-domain <domain>`,
    /// `--code-workers <n>`, `--code-max-inflight <n>`, `--module-workers <n>`,
    /// `--modules-dir <path>`, `--python <auto|off>`,
    /// `--python-max-inflight <n>`, `--python-max-stuck <n>`,
    /// `--python-dir <path>` and `--python-bin <path>`. Unknown flags are an
    /// [`Error::Config`], so a typo fails loudly rather than being ignored.
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
                "--code-workers" => {
                    cfg.code_workers =
                        positive(&next_value(&mut it, "--code-workers")?, "--code-workers")?;
                }
                "--code-max-inflight" => {
                    cfg.code_max_inflight = positive(
                        &next_value(&mut it, "--code-max-inflight")?,
                        "--code-max-inflight",
                    )?;
                }
                "--module-workers" => {
                    cfg.module_workers = positive(
                        &next_value(&mut it, "--module-workers")?,
                        "--module-workers",
                    )?;
                }
                "--modules-dir" => {
                    cfg.modules_dir = Some(PathBuf::from(next_value(&mut it, "--modules-dir")?));
                }
                "--python" => {
                    let raw = next_value(&mut it, "--python")?;
                    cfg.python = match raw.as_str() {
                        "auto" => PythonMode::Auto,
                        "off" => PythonMode::Off,
                        other => {
                            return Err(Error::config(format!(
                                "invalid --python `{other}`: expected `auto` or `off`. \
                                 Whether this server *has* Python is a build-time feature, \
                                 not a flag."
                            )));
                        }
                    };
                }
                "--python-max-inflight" => {
                    cfg.python_max_inflight = positive(
                        &next_value(&mut it, "--python-max-inflight")?,
                        "--python-max-inflight",
                    )?;
                }
                "--python-max-stuck" => {
                    cfg.python_max_stuck = positive(
                        &next_value(&mut it, "--python-max-stuck")?,
                        "--python-max-stuck",
                    )?;
                }
                "--python-dir" => {
                    cfg.python_env.dir = Some(PathBuf::from(next_value(&mut it, "--python-dir")?));
                }
                "--python-bin" => {
                    cfg.python_env.bin = Some(PathBuf::from(next_value(&mut it, "--python-bin")?));
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

/// Parse a count that must be at least one. Zero isolates or zero resident runs
/// would serve nothing at all, and the pool silently clamps both — so a `0` on
/// the command line is refused here rather than quietly meaning `1`.
fn positive(raw: &str, flag: &str) -> Result<usize> {
    match raw.parse::<usize>() {
        Ok(n) if n > 0 => Ok(n),
        Ok(_) => Err(Error::config(format!("{flag} must be at least 1"))),
        Err(e) => Err(Error::config(format!("invalid {flag} `{raw}`: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_bind_to_localhost_3032() {
        let cfg = ServerConfig::default();
        assert_eq!(cfg.addr.to_string(), "127.0.0.1:3032");
        assert!(cfg.static_dir.is_none());
        assert!(cfg.ide_dir.is_none());
        assert!(!cfg.secure_cookies);
        // App subdomain routing is opt-in.
        assert!(cfg.base_domain.is_none());
        // The code pool's defaults are the engine's own (design §10.1).
        assert_eq!(cfg.code_workers, sc_expr::DEFAULT_CODE_WORKERS);
        assert_eq!(cfg.code_max_inflight, sc_expr::DEFAULT_MAX_INFLIGHT);
        // One module worker: the sidecar this replaced was one process holding
        // every module.
        assert_eq!(cfg.module_workers, sc_module::DEFAULT_MODULE_WORKERS);
        // Modules land in the platform's data directory unless this machine
        // says otherwise.
        assert!(cfg.modules_dir.is_none());
        // Python: started when something needs it, with the runtime's own
        // bounds and its own environment defaults.
        assert_eq!(cfg.python, PythonMode::Auto);
        assert_eq!(cfg.python_max_inflight, sc_python::DEFAULT_MAX_INFLIGHT);
        assert_eq!(cfg.python_max_stuck, sc_python::DEFAULT_MAX_STUCK);
        assert_eq!(cfg.python_env, sc_python::PythonEnv::default());
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
            "--code-workers",
            "4",
            "--code-max-inflight",
            "64",
            "--module-workers",
            "3",
            "--modules-dir",
            "/srv/modules",
            "--python",
            "off",
            "--python-max-inflight",
            "8",
            "--python-max-stuck",
            "2",
            "--python-dir",
            "/srv/python",
            "--python-bin",
            "/usr/bin/python3.12",
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
        assert_eq!(cfg.code_workers, 4);
        assert_eq!(cfg.code_max_inflight, 64);
        assert_eq!(cfg.module_workers, 3);
        assert_eq!(
            cfg.modules_dir.as_deref(),
            Some(std::path::Path::new("/srv/modules"))
        );
        assert_eq!(cfg.python, PythonMode::Off);
        assert_eq!(cfg.python_max_inflight, 8);
        assert_eq!(cfg.python_max_stuck, 2);
        assert_eq!(
            cfg.python_env.dir.as_deref(),
            Some(std::path::Path::new("/srv/python"))
        );
        assert_eq!(
            cfg.python_env.bin.as_deref(),
            Some(std::path::Path::new("/usr/bin/python3.12"))
        );
    }

    /// `--python` takes one of two words, and neither of them is the *build*
    /// decision: an operator who writes `--python on` against a binary with no
    /// interpreter in it must be told that here rather than at the first
    /// trigger.
    #[test]
    fn the_python_switch_is_auto_or_off_and_says_so() {
        assert_eq!(
            ServerConfig::from_args(["--python", "auto"])
                .expect("parse")
                .python,
            PythonMode::Auto
        );
        let said = ServerConfig::from_args(["--python", "on"])
            .expect_err("`on` is not one of the two")
            .to_string();
        assert!(said.contains("auto"), "{said}");
        assert!(said.contains("build-time feature"), "{said}");
        assert!(ServerConfig::from_args(["--python"]).is_err());
        // Zero resident runs would serve nothing, exactly as zero isolates
        // would; the runtime clamps it, so a `0` here is refused.
        assert!(ServerConfig::from_args(["--python-max-inflight", "0"]).is_err());
        assert!(ServerConfig::from_args(["--python-max-stuck", "lots"]).is_err());
    }

    /// Both code-pool counts are clamped to at least one by the pool itself, so
    /// a `0` here is a mistake that would be silently rewritten. Refuse it.
    #[test]
    fn rejects_a_zero_or_unparseable_code_pool() {
        assert!(ServerConfig::from_args(["--code-workers", "0"]).is_err());
        assert!(ServerConfig::from_args(["--code-max-inflight", "0"]).is_err());
        assert!(ServerConfig::from_args(["--code-workers", "lots"]).is_err());
        assert!(ServerConfig::from_args(["--code-max-inflight"]).is_err());
        assert!(ServerConfig::from_args(["--module-workers", "0"]).is_err());
    }

    /// The IDE is not configurable, and asking for it is a typo like any other.
    #[test]
    fn the_ide_bundle_is_not_a_flag() {
        assert!(ServerConfig::from_args(["--ide-dir", "/srv/ide"]).is_err());
    }

    #[test]
    fn rejects_bad_bind_and_unknown_flags() {
        assert!(ServerConfig::from_args(["--bind", "not-an-addr"]).is_err());
        assert!(ServerConfig::from_args(["--bind"]).is_err()); // missing value
        assert!(ServerConfig::from_args(["--nope"]).is_err()); // unknown flag
    }
}
