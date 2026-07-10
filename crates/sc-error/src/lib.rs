//! Single `Error` enum and `Result` alias for the Saltcorn v2 workspace (layer 0).
//!
//! This crate sits below everything else in the dependency graph (technical
//! design §2/§16) and encodes principle 5 — **no silent failures**. Fallible
//! code returns [`Result<T>`]; errors either carry context and propagate, or the
//! program crashes with a clear message. Library code must not `unwrap()` or
//! `expect()` on fallible paths — the workspace clippy config (`unwrap_used` /
//! `expect_used`) enforces this mechanically.
//!
//! # Adding context
//!
//! Use the [`Context`] extension trait to attach a human-readable message while
//! preserving the underlying error as a [`std::error::Error`] source chain:
//!
//! ```
//! use sc_error::{Context, Result};
//!
//! fn read_port(raw: &str) -> Result<u16> {
//!     let port = raw
//!         .parse::<u16>()
//!         .with_context(|| format!("invalid port {raw:?}"))?;
//!     Ok(port)
//! }
//!
//! assert!(read_port("nope").is_err());
//! ```
//!
//! The [`bail!`] and [`ensure!`] macros cover the common early-return cases.

use std::fmt;

/// Workspace-wide result alias. Defaults to [`Error`] but keeps the error
/// parameter open so call sites can use a more specific error where useful.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The single error type shared across every Saltcorn crate.
///
/// Variants are intentionally coarse: they classify *where* something went wrong
/// (query, database, auth, …) rather than enumerate every failure. Detail lives
/// in the message string, and the underlying cause — when there is one — is kept
/// in [`Error::Context`] so the full chain is available via
/// [`std::error::Error::source`].
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A requested resource (table, row, user, file, …) does not exist.
    NotFound(String),
    /// Input failed validation or a precondition was not met.
    Invalid(String),
    /// Misconfiguration (missing/invalid settings, bad connection args, …).
    Config(String),
    /// A database driver or connection error.
    Database(String),
    /// Building or rendering a query failed.
    Query(String),
    /// Authentication or authorization failure.
    Auth(String),
    /// A file store / filesystem error.
    File(String),
    /// Serialization or deserialization failure.
    Serde(String),
    /// A catch-all internal error that does not fit the categories above.
    Internal(String),
    /// A lower-level error wrapped with a human-readable context message.
    ///
    /// Prefer constructing this via the [`Context`] trait rather than by hand.
    Context {
        /// The context message describing what was being attempted.
        context: String,
        /// The underlying error, preserved for the source chain.
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
}

impl Error {
    /// A [`Error::NotFound`] from any displayable message.
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }

    /// A [`Error::Invalid`] from any displayable message.
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::Invalid(msg.into())
    }

    /// A [`Error::Config`] from any displayable message.
    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }

    /// A [`Error::Database`] from any displayable message.
    pub fn database(msg: impl Into<String>) -> Self {
        Self::Database(msg.into())
    }

    /// A [`Error::Query`] from any displayable message.
    pub fn query(msg: impl Into<String>) -> Self {
        Self::Query(msg.into())
    }

    /// A [`Error::Auth`] from any displayable message.
    pub fn auth(msg: impl Into<String>) -> Self {
        Self::Auth(msg.into())
    }

    /// A [`Error::File`] from any displayable message.
    pub fn file(msg: impl Into<String>) -> Self {
        Self::File(msg.into())
    }

    /// A [`Error::Serde`] from any displayable message.
    pub fn serde(msg: impl Into<String>) -> Self {
        Self::Serde(msg.into())
    }

    /// A free-form [`Error::Internal`]. Use for genuinely unexpected states.
    pub fn msg(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotFound(m) => write!(f, "not found: {m}"),
            Error::Invalid(m) => write!(f, "invalid: {m}"),
            Error::Config(m) => write!(f, "configuration error: {m}"),
            Error::Database(m) => write!(f, "database error: {m}"),
            Error::Query(m) => write!(f, "query error: {m}"),
            Error::Auth(m) => write!(f, "auth error: {m}"),
            Error::File(m) => write!(f, "file error: {m}"),
            Error::Serde(m) => write!(f, "serialization error: {m}"),
            Error::Internal(m) => write!(f, "internal error: {m}"),
            // Only the top-level context is shown here; the wrapped cause is
            // reachable via `source()` so callers can print the whole chain.
            Error::Context { context, .. } => write!(f, "{context}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Context { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Context {
            context: "I/O error".to_string(),
            source: Box::new(e),
        }
    }
}

/// Attach a context message to a [`Result`] or [`Option`], converting it into a
/// workspace [`Result`].
///
/// For `Result`, the original error is preserved as the source of an
/// [`Error::Context`]. For `Option`, a `None` becomes an [`Error::Internal`]
/// carrying the message.
pub trait Context<T> {
    /// Attach an eagerly-evaluated context message.
    fn context<C>(self, context: C) -> Result<T>
    where
        C: fmt::Display;

    /// Attach a context message computed only on the error path.
    fn with_context<C, F>(self, f: F) -> Result<T>
    where
        C: fmt::Display,
        F: FnOnce() -> C;
}

impl<T, E> Context<T> for std::result::Result<T, E>
where
    E: std::error::Error + Send + Sync + 'static,
{
    fn context<C>(self, context: C) -> Result<T>
    where
        C: fmt::Display,
    {
        self.map_err(|e| Error::Context {
            context: context.to_string(),
            source: Box::new(e),
        })
    }

    fn with_context<C, F>(self, f: F) -> Result<T>
    where
        C: fmt::Display,
        F: FnOnce() -> C,
    {
        self.map_err(|e| Error::Context {
            context: f().to_string(),
            source: Box::new(e),
        })
    }
}

impl<T> Context<T> for Option<T> {
    fn context<C>(self, context: C) -> Result<T>
    where
        C: fmt::Display,
    {
        self.ok_or_else(|| Error::Internal(context.to_string()))
    }

    fn with_context<C, F>(self, f: F) -> Result<T>
    where
        C: fmt::Display,
        F: FnOnce() -> C,
    {
        self.ok_or_else(|| Error::Internal(f().to_string()))
    }
}

/// Return early with an [`Error::Internal`] built from a format string.
///
/// ```
/// use sc_error::{bail, Result};
///
/// fn check(ok: bool) -> Result<()> {
///     if !ok {
///         bail!("something went wrong: {}", 42);
///     }
///     Ok(())
/// }
/// assert!(check(false).is_err());
/// ```
#[macro_export]
macro_rules! bail {
    ($($arg:tt)*) => {
        return ::core::result::Result::Err($crate::Error::Internal(::std::format!($($arg)*)))
    };
}

/// Return early with an [`Error::Internal`] unless a condition holds.
///
/// ```
/// use sc_error::{ensure, Result};
///
/// fn check(n: i32) -> Result<()> {
///     ensure!(n > 0, "n must be positive, got {n}");
///     Ok(())
/// }
/// assert!(check(0).is_err());
/// assert!(check(1).is_ok());
/// ```
#[macro_export]
macro_rules! ensure {
    ($cond:expr, $($arg:tt)*) => {
        if !($cond) {
            $crate::bail!($($arg)*);
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    #[test]
    fn constructors_map_to_variants() {
        assert!(matches!(Error::not_found("t"), Error::NotFound(_)));
        assert!(matches!(Error::invalid("t"), Error::Invalid(_)));
        assert!(matches!(Error::config("t"), Error::Config(_)));
        assert!(matches!(Error::database("t"), Error::Database(_)));
        assert!(matches!(Error::query("t"), Error::Query(_)));
        assert!(matches!(Error::auth("t"), Error::Auth(_)));
        assert!(matches!(Error::file("t"), Error::File(_)));
        assert!(matches!(Error::serde("t"), Error::Serde(_)));
        assert!(matches!(Error::msg("t"), Error::Internal(_)));
    }

    #[test]
    fn display_includes_category_and_message() {
        assert_eq!(Error::not_found("users").to_string(), "not found: users");
        assert_eq!(Error::query("bad sql").to_string(), "query error: bad sql");
    }

    #[test]
    fn context_preserves_source_chain() {
        let inner: std::result::Result<(), std::num::ParseIntError> =
            "x".parse::<i32>().map(|_| ());
        let err = inner.context("parsing the answer").unwrap_err();
        // Top-level Display is just the context message.
        assert_eq!(err.to_string(), "parsing the answer");
        // The original error is reachable as the source.
        let src = err.source().expect("should have a source");
        assert!(src.to_string().contains("invalid digit"));
    }

    #[test]
    fn with_context_is_only_evaluated_on_error() {
        let ok: std::result::Result<i32, std::num::ParseIntError> = "5".parse();
        let mut called = false;
        let out = ok.with_context(|| {
            called = true;
            "unused"
        });
        assert_eq!(out.unwrap(), 5);
        assert!(!called, "context closure must not run on the Ok path");
    }

    #[test]
    fn option_context_produces_internal_error() {
        let none: Option<i32> = None;
        let err = none.context("missing config value").unwrap_err();
        assert!(matches!(err, Error::Internal(_)));
        assert_eq!(err.to_string(), "internal error: missing config value");
    }

    #[test]
    fn io_error_conversion_keeps_source() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "no file");
        let err: Error = io.into();
        assert!(matches!(err, Error::Context { .. }));
        assert!(err.source().is_some());
    }

    #[test]
    fn bail_returns_internal_error() {
        fn f() -> Result<()> {
            bail!("boom {}", 1);
        }
        let err = f().unwrap_err();
        assert_eq!(err.to_string(), "internal error: boom 1");
    }

    #[test]
    fn ensure_checks_condition() {
        fn f(n: i32) -> Result<()> {
            ensure!(n > 0, "must be positive: {n}");
            Ok(())
        }
        assert!(f(1).is_ok());
        assert_eq!(
            f(-1).unwrap_err().to_string(),
            "internal error: must be positive: -1"
        );
    }

    #[test]
    fn error_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Error>();
    }
}
