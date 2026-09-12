//! Where Saltcorn UI's view runtime lives, and the sentence for when it does not
//! (TODO "Saltcorn UI" Phase 2, §9).
//!
//! `ui/saltcorn-ui` is the third bundle `sc-cli`'s build script builds, and its
//! `dist/` holds `view-runtime.js` — v1's view patterns, fieldviews and
//! `@saltcorn/markup`, vendored and bundled — beside the browser assets in
//! `public/`. A build with `SC_BUILD_ADMIN=0` records no directory; a packaged
//! tree can lose the file. Either way the application that needs it fails on
//! **mount**, with this module's sentence, rather than on every request.

use std::path::{Path, PathBuf};

use sc_error::{Error, Result};

/// The registered name of the Saltcorn UI framework (`FrameworkRef::name`).
pub const SALTCORN_UI_FRAMEWORK: &str = "saltcorn-ui";

/// The view runtime's file name inside the bundle directory.
pub const VIEW_RUNTIME_FILE: &str = "view-runtime.js";

/// The directory, relative to the workspace root, the bundle is built into.
pub const BUNDLE_DIR_IN_CHECKOUT: &str = "ui/saltcorn-ui/dist";

/// The view runtime inside `dir`, or the error that says why there is none.
pub fn require_view_runtime(dir: Option<&Path>) -> Result<PathBuf> {
    let Some(dir) = dir else {
        return Err(Error::config(format!(
            "this server was built without the Saltcorn UI bundle ({BUNDLE_DIR_IN_CHECKOUT}), \
             so an application whose framework is `{SALTCORN_UI_FRAMEWORK}` cannot be mounted; \
             rebuild with SC_BUILD_ADMIN unset"
        )));
    };
    let runtime = dir.join(VIEW_RUNTIME_FILE);
    if runtime.is_file() {
        Ok(runtime)
    } else {
        Err(Error::config(format!(
            "the Saltcorn UI bundle is missing its view runtime: {} does not exist \
             (run `npm ci && npm run build` in ui/saltcorn-ui, or reinstall the release tree)",
            runtime.display()
        )))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_build_without_the_bundle_names_it() {
        let msg = require_view_runtime(None).unwrap_err().to_string();
        assert!(msg.contains("ui/saltcorn-ui/dist"), "{msg}");
        assert!(msg.contains("saltcorn-ui"), "{msg}");
        assert!(msg.contains("SC_BUILD_ADMIN"), "{msg}");
    }

    #[test]
    fn a_directory_without_the_runtime_names_the_file() {
        let dir = std::env::temp_dir();
        let msg = require_view_runtime(Some(&dir.join("no-such-saltcorn-ui")))
            .unwrap_err()
            .to_string();
        assert!(msg.contains("no-such-saltcorn-ui/view-runtime.js"), "{msg}");
    }
}
