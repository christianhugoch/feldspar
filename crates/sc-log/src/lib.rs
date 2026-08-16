//! How much this process says, and whether it prints the SQL it issues
//! (layer 0).
//!
//! Two switches, both of them **process-wide atomics** rather than values
//! carried down through every call: the verbosity, and whether every statement
//! sent to the database is echoed. They are set from the stored Development
//! settings (`sc_config::development`) at boot and again whenever an admin
//! saves them, so turning SQL logging on is a checkbox rather than a restart.
//!
//! **Why globals.** Logging is the one concern that cuts across the whole
//! dependency graph: the thing that wants to print a statement is the Postgres
//! driver (layer 2), the thing that knows what the admin ticked is the config
//! store (layer 5), and the layers in between have no business carrying a
//! logging handle from one to the other. An `AtomicBool` read on the statement
//! path costs a relaxed load, which is less than the `format!` it guards.
//!
//! **Where it comes out.** SQL goes to **stdout**, because it is the *output*
//! an admin turned the switch on to read — pipe it to a file, grep it. Level
//! messages go to **stderr**, which is where every other `saltcorn:` line this
//! server prints already goes, so a redirected stdout is the SQL and nothing
//! else.
//!
//! ```
//! sc_log::set_verbosity(sc_log::Verbosity::Info);
//! assert!(sc_log::enabled(sc_log::Verbosity::Info));
//! assert!(!sc_log::enabled(sc_log::Verbosity::Trace));
//! sc_log::log_info!("{} {} → {}", "GET", "/health", 200);
//! ```

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use sc_error::{Error, Result};

/// How much this process says: the standard Unix ladder, least talkative first.
///
/// The order is the whole meaning — a message is printed when its level is at
/// or below the configured one — so the discriminants are deliberate and
/// [`Ord`] is derived from them rather than written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Verbosity {
    /// Only what failed.
    Error = 0,
    /// What failed, and what is about to.
    Warning = 1,
    /// The above, plus **every server request** — one line per request, with
    /// its method, path, status and duration.
    Info = 2,
    /// The above, plus each request as it *arrives*, so a request that never
    /// finishes is visible while it is hanging.
    Verbose = 3,
    /// Everything, including request headers (with credentials redacted).
    Trace = 4,
}

/// The verbosity a process runs at until something sets one.
///
/// Warning rather than Info: Info is defined to print every request, and a
/// server that logs every request by default is a server whose logs nobody
/// reads. Turning it up is a checkbox in Settings → Development.
pub const DEFAULT_VERBOSITY: Verbosity = Verbosity::Warning;

impl Verbosity {
    /// Every level, least talkative first — the option list the settings
    /// dropdown is built from, so the form and the parser cannot disagree
    /// about what a level is called.
    pub const ALL: [Verbosity; 5] = [
        Verbosity::Error,
        Verbosity::Warning,
        Verbosity::Info,
        Verbosity::Verbose,
        Verbosity::Trace,
    ];

    /// The stored spelling: lower-case, as every other option-valued setting in
    /// `_sc_config` is stored.
    pub fn as_str(self) -> &'static str {
        match self {
            Verbosity::Error => "error",
            Verbosity::Warning => "warning",
            Verbosity::Info => "info",
            Verbosity::Verbose => "verbose",
            Verbosity::Trace => "trace",
        }
    }

    /// Parse a stored `log_verbosity`, case-insensitively.
    ///
    /// Case-insensitive because these five words are the ones an operator
    /// already knows from every other daemon they run, and being refused for
    /// typing `Info` would be a rule with nothing behind it. An unrecognised
    /// value *is* an error naming what was expected — the declaration's
    /// `options` mean the form cannot produce one, so it can only come from a
    /// hand-edited row.
    pub fn parse(raw: &str) -> Result<Verbosity> {
        let lowered = raw.trim().to_ascii_lowercase();
        Verbosity::ALL
            .into_iter()
            .find(|level| level.as_str() == lowered)
            .ok_or_else(|| {
                Error::invalid(format!(
                    "unknown log verbosity `{raw}`; expected one of {}",
                    Verbosity::ALL.map(Verbosity::as_str).join(", ")
                ))
            })
    }

    /// The level a stored `u8` means, falling back to [`DEFAULT_VERBOSITY`].
    fn from_u8(raw: u8) -> Verbosity {
        Verbosity::ALL
            .into_iter()
            .find(|level| *level as u8 == raw)
            .unwrap_or(DEFAULT_VERBOSITY)
    }
}

/// Whether every statement sent to the database is echoed to stdout.
static LOG_SQL: AtomicBool = AtomicBool::new(false);

/// The current [`Verbosity`], as its discriminant.
static VERBOSITY: AtomicU8 = AtomicU8::new(DEFAULT_VERBOSITY as u8);

/// Set how much this process says. Takes effect on the next message.
pub fn set_verbosity(level: Verbosity) {
    VERBOSITY.store(level as u8, Ordering::Relaxed);
}

/// How much this process is currently saying.
pub fn verbosity() -> Verbosity {
    Verbosity::from_u8(VERBOSITY.load(Ordering::Relaxed))
}

/// Whether a message at `level` would be printed.
///
/// Call this before building a message that costs something to build: the
/// macros already do, and the request middleware uses it to avoid cloning a URI
/// nobody will read.
pub fn enabled(level: Verbosity) -> bool {
    level <= verbosity()
}

/// Turn the SQL echo on or off. Takes effect on the next statement.
pub fn set_log_sql(on: bool) {
    LOG_SQL.store(on, Ordering::Relaxed);
}

/// Whether the SQL echo is on.
///
/// Independent of the verbosity on purpose: SQL is a firehose that an admin
/// turns on to debug one thing, not a rung of the same ladder that decides
/// whether requests are logged.
pub fn log_sql_enabled() -> bool {
    LOG_SQL.load(Ordering::Relaxed)
}

/// Print a message at `level`, if the configured verbosity includes it.
///
/// The macros ([`log_error!`], [`log_warn!`], [`log_info!`], [`log_verbose!`],
/// [`log_trace!`]) are the way to call this: they defer formatting until the
/// level check has passed.
pub fn log(level: Verbosity, args: fmt::Arguments<'_>) {
    if !enabled(level) {
        return;
    }
    emit(Stream::Err, format!("saltcorn: {}: {args}", level.as_str()));
}

/// Echo one statement, if the SQL log is on.
///
/// `binds` are the values that will be sent with it. They are printed —
/// unlike in an error message, which is *returned to a client* and so carries
/// only their count — because a statement without its parameters does not tell
/// you what ran, and the person who ticked this box is reading their own
/// server's stdout. That includes password hashes and session tokens, which is
/// why the setting says so.
pub fn log_sql<T: fmt::Debug>(sql: &str, binds: &[T]) {
    if !log_sql_enabled() {
        return;
    }
    emit(Stream::Out, sql_line(sql, binds));
}

/// How long a single bind's rendering may get before it is cut short: enough
/// for a row's worth of text, short of a megabyte of `bytea` scrolling past.
const MAX_BIND_CHARS: usize = 120;

/// One line of SQL log: the statement, and the binds it goes out with.
///
/// Separate from [`log_sql`] so the rendering can be tested without capturing
/// stdout, and so a caller that has already decided to log (a batch of DDL, a
/// transaction verb) can build the line itself.
pub fn sql_line<T: fmt::Debug>(sql: &str, binds: &[T]) -> String {
    let sql = collapse_whitespace(sql);
    if binds.is_empty() {
        return format!("saltcorn: sql: {sql}");
    }
    let rendered: Vec<String> = binds
        .iter()
        .map(|bind| truncate(&format!("{bind:?}"), MAX_BIND_CHARS))
        .collect();
    format!("saltcorn: sql: {sql} -- binds: [{}]", rendered.join(", "))
}

/// One statement on one line: a multi-line rendering (DDL, a policy batch) is
/// still one thing that ran, and a log that can be `grep`ed line-by-line is
/// worth more than the original layout.
fn collapse_whitespace(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Cut `text` to `max` characters, marking that it was cut.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}…")
}

/// Which stream a line goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stream {
    /// Standard output: the SQL echo, which is what somebody redirects.
    Out,
    /// Standard error: level messages, beside every other `saltcorn:` line.
    Err,
}

/// Write one finished line.
///
/// Under `cfg(test)` the line is captured instead of printed, which is what
/// lets this crate's own tests assert that a switch actually silences its
/// output — the behaviour worth testing, and not one that can be checked by
/// inspecting a formatter.
fn emit(stream: Stream, line: String) {
    #[cfg(test)]
    {
        capture::push(stream, line);
    }
    #[cfg(not(test))]
    match stream {
        Stream::Out => println!("{line}"),
        Stream::Err => eprintln!("{line}"),
    }
}

/// Print at [`Verbosity::Error`].
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {
        $crate::log($crate::Verbosity::Error, ::std::format_args!($($arg)*))
    };
}

/// Print at [`Verbosity::Warning`].
#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {
        $crate::log($crate::Verbosity::Warning, ::std::format_args!($($arg)*))
    };
}

/// Print at [`Verbosity::Info`] — where every server request is logged.
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        $crate::log($crate::Verbosity::Info, ::std::format_args!($($arg)*))
    };
}

/// Print at [`Verbosity::Verbose`].
#[macro_export]
macro_rules! log_verbose {
    ($($arg:tt)*) => {
        $crate::log($crate::Verbosity::Verbose, ::std::format_args!($($arg)*))
    };
}

/// Print at [`Verbosity::Trace`].
#[macro_export]
macro_rules! log_trace {
    ($($arg:tt)*) => {
        $crate::log($crate::Verbosity::Trace, ::std::format_args!($($arg)*))
    };
}

/// Lines this crate's tests collected instead of printing.
#[cfg(test)]
mod capture {
    use std::cell::RefCell;

    use super::Stream;

    thread_local! {
        /// Per-thread, because the switches are per-process: a test holds the
        /// switch lock and reads back only what its own thread emitted.
        static LINES: RefCell<Vec<(Stream, String)>> = const { RefCell::new(Vec::new()) };
    }

    /// Record one line.
    pub(super) fn push(stream: Stream, line: String) {
        LINES.with(|lines| lines.borrow_mut().push((stream, line)));
    }

    /// Take everything recorded on this thread so far.
    pub(super) fn take() -> Vec<(Stream, String)> {
        LINES.with(|lines| std::mem::take(&mut *lines.borrow_mut()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::{Mutex, MutexGuard, OnceLock};

    use super::*;

    /// The switches are process-wide, so the tests that move them take turns.
    fn switches() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let guard = LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        set_verbosity(DEFAULT_VERBOSITY);
        set_log_sql(false);
        let _ = capture::take();
        guard
    }

    /// The levels a test reads back, as plain strings.
    fn lines() -> Vec<String> {
        capture::take().into_iter().map(|(_, line)| line).collect()
    }

    #[test]
    fn the_ladder_runs_from_error_to_trace() {
        assert!(Verbosity::Error < Verbosity::Warning);
        assert!(Verbosity::Warning < Verbosity::Info);
        assert!(Verbosity::Info < Verbosity::Verbose);
        assert!(Verbosity::Verbose < Verbosity::Trace);
        assert_eq!(
            Verbosity::ALL.map(Verbosity::as_str),
            ["error", "warning", "info", "verbose", "trace"]
        );
    }

    #[test]
    fn a_level_is_parsed_however_it_is_capitalised_and_a_typo_is_not() {
        assert_eq!(Verbosity::parse("info").unwrap(), Verbosity::Info);
        assert_eq!(Verbosity::parse(" Trace ").unwrap(), Verbosity::Trace);
        let err = Verbosity::parse("chatty").unwrap_err().to_string();
        assert!(err.contains("chatty"), "{err}");
        assert!(err.contains("verbose"), "{err}");
    }

    /// The point of the whole ladder: a message is printed when its level is at
    /// or below the configured one, and silent otherwise.
    #[test]
    fn a_message_above_the_configured_level_is_not_printed() {
        let _guard = switches();
        set_verbosity(Verbosity::Info);
        assert!(enabled(Verbosity::Error) && enabled(Verbosity::Info));
        assert!(!enabled(Verbosity::Verbose) && !enabled(Verbosity::Trace));

        log_error!("boom");
        log_info!("GET /health → {}", 200);
        log_verbose!("not this one");
        log_trace!("nor this");
        assert_eq!(
            lines(),
            ["saltcorn: error: boom", "saltcorn: info: GET /health → 200"]
        );
    }

    /// The default is quiet enough that a server does not log every request
    /// until somebody asks it to.
    #[test]
    fn requests_are_not_logged_until_the_level_reaches_info() {
        let _guard = switches();
        assert_eq!(verbosity(), Verbosity::Warning);
        assert!(!enabled(Verbosity::Info));
        set_verbosity(Verbosity::Info);
        assert!(enabled(Verbosity::Info));
    }

    #[test]
    fn sql_is_printed_to_stdout_only_while_the_switch_is_on() {
        let _guard = switches();
        log_sql("SELECT 1", &[] as &[String]);
        assert!(capture::take().is_empty(), "the switch was off");

        set_log_sql(true);
        log_sql("SELECT * FROM \"books\" WHERE \"id\" = $1", &["Int(3)"]);
        let logged = capture::take();
        assert_eq!(logged.len(), 1);
        // Stdout, because it is the output somebody redirects to a file.
        assert_eq!(logged[0].0, Stream::Out);
        assert!(logged[0].1.contains("SELECT * FROM \"books\""));
        assert!(logged[0].1.contains("Int(3)"), "{}", logged[0].1);
    }

    #[test]
    fn a_logged_statement_is_one_line_with_its_binds() {
        assert_eq!(
            sql_line("SELECT 1", &[] as &[i32]),
            "saltcorn: sql: SELECT 1"
        );
        assert_eq!(
            sql_line("INSERT INTO t\n  VALUES ($1, $2)", &["a", "b"]),
            "saltcorn: sql: INSERT INTO t VALUES ($1, $2) -- binds: [\"a\", \"b\"]"
        );
    }

    /// A `bytea` bind is a megabyte of digits printed one row at a time unless
    /// something cuts it off.
    #[test]
    fn an_enormous_bind_is_cut_short() {
        let big = "x".repeat(5_000);
        let line = sql_line("SELECT $1", &[big]);
        assert!(line.chars().count() < MAX_BIND_CHARS + 60, "{}", line.len());
        assert!(line.ends_with("…]"), "{line}");
    }
}
