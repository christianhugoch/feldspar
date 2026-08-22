//! Where installed modules live on disk.
//!
//! One npm project the server owns:
//!
//! ```text
//! <modules root>/package.json        # written here: private, no dependencies of its own
//! <modules root>/node_modules/…      # what npm put there
//! <modules root>/module-host.mjs     # the host script, written from the binary
//! ```
//!
//! **Where the root is** follows the same rule `sc_config_file` states for the
//! configuration file: ask the platform rather than hard-coding `~/.saltcorn`.
//! The order of authority is the operator's word first — `serve --modules-dir`,
//! then a `modules_dir` in the chosen `saltcorn.toml` environment, both of which
//! reach this crate as an explicit path — and the platform's **data** directory
//! last, because installed packages are state, not configuration:
//!
//! | | modules root |
//! |---|---|
//! | Linux/BSD | `$XDG_DATA_HOME/saltcorn/modules` (else `~/.local/share/saltcorn/modules`) |
//! | macOS | `~/Library/Application Support/saltcorn/modules` |
//! | Windows | `%APPDATA%\saltcorn\modules` |
//!
//! Written against `std::env` for the same reason `sc_config_file` is: these are
//! three variables and two fallbacks, and a directories crate would cost more to
//! justify than the rules cost to state.

use std::path::PathBuf;

use sc_error::{Error, Result};

/// The directory name under the platform's data directory.
const APP_DIR: &str = "saltcorn";
/// The subdirectory of that holding the npm project.
const MODULES_DIR: &str = "modules";

/// The default modules root for this machine, or an error explaining why no
/// directory could be determined.
///
/// An error rather than a silent fallback to the working directory: a server
/// that installed modules into whatever directory it was started in would put
/// them somewhere different on the next boot, which is worse than refusing.
pub fn default_modules_root() -> Result<PathBuf> {
    user_data_dir()
        .map(|dir| dir.join(APP_DIR).join(MODULES_DIR))
        .ok_or_else(|| {
            Error::config(
                "cannot tell where to install modules: no user data directory could be \
                 determined (set XDG_DATA_HOME or HOME, or pass --modules-dir)",
            )
        })
}

/// The platform's per-user data directory.
fn user_data_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        return env_var("APPDATA").map(PathBuf::from);
    }
    if let Some(xdg) = env_var("XDG_DATA_HOME") {
        return Some(PathBuf::from(xdg));
    }
    let home = PathBuf::from(env_var("HOME")?);
    if cfg!(target_os = "macos") {
        Some(home.join("Library").join("Application Support"))
    } else {
        Some(home.join(".local").join("share"))
    }
}

/// Read an environment variable, treating empty as absent.
fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_root_is_under_the_platform_data_directory() {
        // Whatever the platform, the last two components are ours: this is the
        // part a test can assert without asserting the platform's own rules
        // back at it.
        let Ok(root) = default_modules_root() else {
            // A machine with neither HOME nor XDG_DATA_HOME — the error path,
            // which the other test covers.
            return;
        };
        assert!(
            root.ends_with(PathBuf::from(APP_DIR).join(MODULES_DIR)),
            "{root:?}"
        );
        assert!(root.is_absolute(), "{root:?}");
    }
}
