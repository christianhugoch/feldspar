//! `feldspar app list [--json]`: the stored applications, and where each one's
//! project directory is on this machine.
//!
//! The project directory is the one an external coding agent is started in
//! (`claude mcp add …`, then `claude`, there), so a script that sets up agents
//! needs it per application. It is **derived**, not stored: the framework's
//! settings name a file store and a directory inside it, and the store's
//! definition says where it is on disk. This resolves it exactly as a build
//! does — `app_source_from_config` for a framework with a build step,
//! `none_source_dir` for `none` — and asks the connected store for the local
//! path, so the answer is the directory a build would run in.
//!
//! An application whose directory cannot be resolved — a framework with no
//! source tree, a store that is not connected, a store with no local disk —
//! is still listed, with `project_dir: null` and the reason in `error`: a list
//! that left it out would read as "there is no such application".

use sc_app::Application;
use sc_catalog::Catalog;
use sc_error::{Error, Result};
use serde_json::{Value as Json, json};

/// Parse `app list`'s flags: `--json`, or nothing for the plain listing.
pub fn parse_list(args: &[String]) -> Result<bool> {
    match args {
        [] => Ok(false),
        [flag] if flag == "--json" => Ok(true),
        [other, ..] => Err(Error::config(format!(
            "unknown app list argument `{other}`; it takes --json"
        ))),
    }
}

pub use sc_app::app_source_location as source_location;
pub use sc_app::store_dir as project_dir;

/// One application as `app list --json` reports it.
pub fn app_json(catalog: &Catalog, app: &Application) -> Json {
    let located = source_location(app)
        .and_then(|(store, dir)| project_dir(catalog, &store, &dir).map(|p| (store, dir, p)));
    let mut out = json!({
        "id": app.id.0,
        "name": app.name,
        "subdomain": app.subdomain,
        "framework": app.framework.name,
    });
    match located {
        Ok((store, dir, path)) => {
            out["file_store"] = json!(store);
            out["source_dir"] = json!(dir);
            out["project_dir"] = json!(path.display().to_string());
        }
        Err(e) => {
            out["project_dir"] = Json::Null;
            out["error"] = json!(e.to_string());
        }
    }
    out
}
