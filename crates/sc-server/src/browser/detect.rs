//! Finding the headless browser `view_app` drives (TODO 6b.2).
//!
//! The `browser` setting names one outright. Otherwise the server looks on
//! `PATH` for [`BROWSER_NAMES`], in order, and **skips a snap**: Ubuntu's apt
//! `chromium-browser` is a transitional package whose binary is a shell script
//! that execs `/snap/bin/chromium`, and `/snap/bin/chromium` is a link to
//! `snap` itself — neither starts under a systemd service user with
//! `ProtectHome` and `PrivateTmp`. A skipped candidate is named in the reason,
//! so "no browser" on a machine that visibly has one explains itself.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The names looked for on `PATH`, in order.
pub const BROWSER_NAMES: [&str; 3] = ["chromium", "chromium-browser", "google-chrome"];

/// Where the browser is, or why there is none.
pub fn detect_browser(configured: Option<&Path>) -> Result<PathBuf, String> {
    detect_in(configured, std::env::var_os("PATH"))
}

/// [`detect_browser`] over a given `PATH`, for tests.
pub(crate) fn detect_in(
    configured: Option<&Path>,
    path_var: Option<OsString>,
) -> Result<PathBuf, String> {
    if let Some(path) = configured {
        if !is_executable(path) {
            return Err(format!(
                "the `browser` setting names `{}`, which is not an executable file",
                path.display()
            ));
        }
        if let Some(why) = snap(path) {
            return Err(format!(
                "the `browser` setting names `{}`, which {why}; a snap does not run under \
                 a service user, so install a non-snap Chromium (scripts/setup-host.sh does)",
                path.display()
            ));
        }
        return Ok(path.to_owned());
    }
    let dirs: Vec<PathBuf> = path_var
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let mut skipped = Vec::new();
    for name in BROWSER_NAMES {
        for dir in &dirs {
            let candidate = dir.join(name);
            if !is_executable(&candidate) {
                continue;
            }
            match snap(&candidate) {
                Some(why) => skipped.push(format!("{} {why}", candidate.display())),
                None => return Ok(candidate),
            }
        }
    }
    let mut reason = format!("no {} on PATH", BROWSER_NAMES.join(", "));
    if !skipped.is_empty() {
        reason.push_str(&format!(
            " that is not a snap (skipped: {})",
            skipped.join("; ")
        ));
    }
    reason.push_str(
        "; install Chromium (scripts/setup-host.sh does) or set `browser` in feldspar.toml",
    );
    Err(reason)
}

/// Why `path` is a snap, or `None`.
fn snap(path: &Path) -> Option<&'static str> {
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    if path.starts_with("/snap") || real.starts_with("/snap") {
        return Some("is a snap");
    }
    if real.file_name().is_some_and(|n| n == "snap") {
        return Some("is a link to snap");
    }
    let mut head = Vec::with_capacity(4096);
    let read = std::fs::File::open(&real)
        .and_then(|f| f.take(4096).read_to_end(&mut head))
        .is_ok();
    if read && head.starts_with(b"#!") && String::from_utf8_lossy(&head).contains("/snap/") {
        return Some("is a script that runs a snap");
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn executable(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sc-browser-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_path_is_searched_in_order_and_a_snap_shim_is_skipped_by_name() {
        let dir = scratch("path");
        executable(
            &dir,
            "chromium-browser",
            "#!/bin/sh\nexec /snap/bin/chromium \"$@\"\n",
        );
        let path = Some(dir.clone().into_os_string());
        let err = detect_in(None, path.clone()).unwrap_err();
        assert!(
            err.contains("chromium-browser is a script that runs a snap"),
            "{err}"
        );
        assert!(err.contains("set `browser` in feldspar.toml"), "{err}");

        let chrome = executable(&dir, "google-chrome", "\x7fELF");
        assert_eq!(detect_in(None, path.clone()), Ok(chrome));
        // `chromium` comes first.
        let chromium = executable(&dir, "chromium", "\x7fELF");
        assert_eq!(detect_in(None, path), Ok(chromium));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_setting_wins_and_is_checked() {
        let dir = scratch("setting");
        let mine = executable(&dir, "my-chrome", "\x7fELF");
        assert_eq!(detect_in(Some(&mine), None), Ok(mine.clone()));
        let missing = dir.join("nope");
        let err = detect_in(Some(&missing), None).unwrap_err();
        assert!(err.contains("not an executable file"), "{err}");
        let shim = executable(&dir, "shim", "#!/bin/sh\nexec /snap/bin/chromium\n");
        assert!(detect_in(Some(&shim), None).unwrap_err().contains("snap"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn nothing_on_the_path_says_what_was_looked_for() {
        let err = detect_in(None, None).unwrap_err();
        assert!(
            err.starts_with("no chromium, chromium-browser, google-chrome on PATH"),
            "{err}"
        );
    }
}
