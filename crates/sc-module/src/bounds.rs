//! The bounds a module call is under, and the size of the pool it runs on.
//!
//! Ungated, because they are the *policy* rather than the runtime: `sc-server`
//! reads [`DEFAULT_MODULE_WORKERS`] to default its `--module-workers` flag in a
//! build that does not link `deno_runtime` at all.
//!
//! Four bounds, and they are deliberately the same four `sc_expr`'s
//! `CodeRuntime` is under rather than a second vocabulary for the same ideas:
//! a wall clock on the call, a **JS slice** on uninterrupted JavaScript, a heap
//! limit on the isolate, and the worker restart as the backstop behind all
//! three. What differs is the numbers, and each of them differs for a stated
//! reason — a module is not a code body.

use std::time::Duration;

/// How long one call may take before the caller gives up on it.
///
/// Generous, because a module's action is somebody else's network: an MQTT
/// publish is milliseconds and a Proxmox snapshot is not. Two orders of
/// magnitude above a code body's five seconds, and it stays there. The action's
/// own trigger is bounded by whatever fired it; this bound exists so a module
/// that never answers is a failed call rather than a held request forever.
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// How long a module's JavaScript may run **without yielding** before the
/// worker's watchdog stops the isolate.
///
/// Ten seconds, against a code body's one, and the reason is `require`. A code
/// body is a few lines over a scope that is already built; a module's `load`
/// synchronously pulls a whole npm dependency tree through V8's parser —
/// `async-mqtt` and what it brings is tens of thousands of lines — and that is
/// one uninterrupted slice of JavaScript with no `await` in it anywhere. A
/// one-second slice would not be a watchdog, it would be a rule that large
/// modules may not be installed.
///
/// It is still an order of magnitude inside [`DEFAULT_CALL_TIMEOUT`], so a
/// module that spins forever is stopped by this rather than waited out by the
/// caller — which is what makes a runaway cost its own worker and nobody's
/// patience.
pub const DEFAULT_MODULE_JS_SLICE: Duration = Duration::from_secs(10);

/// The V8 heap one module worker's isolate may grow to.
///
/// The same 256 MB the code pool admits against, and for a different reason:
/// there it is shared admission budget across hundreds of resident runs, here it
/// is a ceiling on however many modules an admin installed. It is a ceiling and
/// not a reservation — V8 grows into it lazily, and phase 0 measured a worker
/// with two real modules on it at 67 MB.
pub const DEFAULT_MODULE_MAX_HEAP: usize = 256 * 1024 * 1024;

/// How many worker threads the module pool runs by default.
///
/// **One**, because that is what the sidecar already is: one process holding
/// every module, not one per module. A module is long-lived state — a socket
/// with a reconnect timer and live callbacks — so the reason to add a worker is
/// blast radius, not throughput: a second worker is a second isolate for a
/// module that must not share a watchdog with a co-resident. Throughput is not
/// among the reasons, because a module action is somebody else's network and the
/// isolate is idle for all of it.
pub const DEFAULT_MODULE_WORKERS: usize = 1;
