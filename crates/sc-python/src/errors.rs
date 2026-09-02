//! The exception hierarchy a Python body sees, defined **in Rust**.
//!
//! ```text
//! saltcorn.SaltcornError(Exception)
//! ├── DbError · FetchError · FileError · TriggerError · ModuleError
//! saltcorn.Timeout(BaseException)
//! ```
//!
//! Here rather than in the Python half for one reason: nothing that runs in the
//! interpreter can redefine them. A body that rebinds `saltcorn.Timeout` rebinds
//! a name in its own globals; the type the host raises, and the type the host
//! checks against when it decides whether a run was stopped, is this one.
//!
//! **`Timeout` derives from `BaseException` on purpose.** A bare
//! `except Exception:` in somebody's retry loop must not swallow the run's
//! deadline — the same rule `KeyboardInterrupt` and `SystemExit` are under, and
//! for the same reason: it is not the body's error to handle.
//!
//! Everything else is an ordinary catchable exception raised **at the call
//! site**, so a body may try a delegated write, catch the refusal and fall back.
//! Which surface refused is part of the type, because "the file was not there"
//! and "the trigger declined" are different recoveries.
//!
//! The module name given to `create_exception!` is `saltcorn`, so an unhandled
//! one prints as `saltcorn.DbError` however it was reached — the `saltcorn`
//! package re-exports these rather than defining anything of its own.

use pyo3::create_exception;
use pyo3::exceptions::{PyBaseException, PyException};

create_exception!(
    saltcorn,
    SaltcornError,
    PyException,
    "The base of every error this host raises into a body."
);
create_exception!(
    saltcorn,
    DbError,
    SaltcornError,
    "A table operation the host refused or could not complete."
);
create_exception!(
    saltcorn,
    FetchError,
    SaltcornError,
    "An outbound HTTP request that failed in transport. A status the endpoint \
     did not like is not this: it is an ordinary response whose `ok` is false."
);
create_exception!(
    saltcorn,
    FileError,
    SaltcornError,
    "A file operation the host refused or could not complete."
);
create_exception!(
    saltcorn,
    TriggerError,
    SaltcornError,
    "Another trigger that could not be run, or that failed."
);
create_exception!(
    saltcorn,
    ModuleError,
    SaltcornError,
    "A module function that could not be called, or that failed."
);
create_exception!(
    saltcorn,
    Timeout,
    PyBaseException,
    "This run is past its deadline. From `BaseException`, so an `except \
     Exception:` cannot swallow it."
);
