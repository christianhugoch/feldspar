//! What a module host is, from the outside: [`ModuleHost`], the manifest a
//! module answers a load with, and the script that runs inside it.
//!
//! A module used to run in a `node` child process behind a newline-JSON pipe.
//! It runs on a **Deno worker in this process** now ([`crate::deno`]) — the same
//! V8 the code pool already links, with `node` off the server's list of runtime
//! requirements and a permission set available to a module's worker, which
//! `node` had no way to offer. This module is what the rest of the server sees
//! of that: the same four calls, the same manifest, and none of the change.
//!
//! **Sandboxed**: a module's worker is built with the permission set on its
//! `_sc_modules` row — closed unless an admin granted something — and modules
//! are pinned to workers by that set, because a `PermissionsContainer` belongs
//! to an isolate (§2).
//!
//! **Lazily started**: a deployment with no modules never builds an isolate.
//! **Restarted on death**: a module that calls `process.exit()`, spins past its
//! JS slice or exhausts the heap ends *its own worker*; every call in flight on
//! it is failed by name, and the next call gets a fresh worker with every load
//! replayed into it, so the module set survives a crash without the caller
//! knowing there was one.
//!
//! A call is one V8 function call into [`HOST_SCRIPT`]'s entry point, carrying
//! an `id` the answer carries back — so many calls are in flight at once and a
//! slow module's action does not hold anybody else's. The modules' own
//! `console.log` goes straight into [`sc_log`], tagged with the module's name.

use std::path::{Path, PathBuf};

use sc_error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::permissions::ModulePermissions;

/// The host script, written into the modules root at every worker start.
///
/// `pub(crate)` because [`crate::deno`] is what writes and evaluates it: it is
/// the JavaScript half of the host, and the only thing that runs it is the
/// worker. Which is also why a build without that feature has nothing that
/// reads it but this module's own tests.
#[cfg_attr(
    not(any(feature = "deno-host", test)),
    expect(dead_code, reason = "no runtime to run it")
)]
pub(crate) const HOST_SCRIPT: &str = include_str!("js/module-host.mjs");

/// What the host script is called on disk.
pub const HOST_SCRIPT_NAME: &str = "module-host.mjs";

pub use crate::bounds::DEFAULT_CALL_TIMEOUT;

/// One action, as the module declared it.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ActionManifest {
    /// The name the action is registered under — v1's own, unqualified.
    pub name: String,
    /// The module's one-line description, if it gave one.
    #[serde(default)]
    pub description: String,
    /// Whether the action needs a row to act on.
    #[serde(default, rename = "requireRow")]
    pub require_row: bool,
    /// v1 `configFields`, as the module declared them — translated by
    /// [`crate::spec`], never interpreted here.
    #[serde(default, rename = "configFields")]
    pub config_fields: Vec<Json>,
}

/// One argument of a module function, as v1 declares it.
///
/// v1's own `arguments: [{ name, type }]` vocabulary, kept rather than
/// reinvented: `type` is a v1 field type name (`String`, `Integer`, `Object`),
/// which is the same vocabulary [`crate::spec`] already translates. Absent when
/// the module did not say — a v1 function may declare nothing at all, and
/// `null` is the honest answer rather than a guess.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct FunctionArg {
    /// The argument's name, as the signature shows it.
    pub name: String,
    /// The v1 type name, when the module declared one.
    #[serde(default, rename = "type")]
    pub type_name: Option<String>,
}

/// One function, as the module declared it (§4a).
///
/// A v1 plugin supplies `functions` beside `actions`, and v1 makes them
/// "available to formulas and code actions". The three shapes v1 allows — a bare
/// function, a `{ run, isAsync, description, arguments }` object, and a function
/// of the module's own configuration — are resolved in the host script; what
/// arrives here is the one shape.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct FunctionManifest {
    /// The name the function is registered under — v1's own, unqualified. Two
    /// modules may each supply the same one; nothing here disambiguates them,
    /// because which module is meant is the *caller's* question.
    pub name: String,
    /// The module's one-line description, if it gave one.
    #[serde(default)]
    pub description: String,
    /// Whether v1 itself treated this function as awaitable. It does not decide
    /// how the function is *called* — everything crosses the seam awaited — but
    /// it is what a signature in the code editor says, and it is v1's word.
    #[serde(default, rename = "isAsync")]
    pub is_async: bool,
    /// The declared signature, when the module declared one.
    #[serde(default)]
    pub arguments: Vec<FunctionArg>,
}

/// One **table provider** a module supplies (§8.3).
///
/// v1's `table_providers` key: a virtual table whose rows the module produces.
/// What arrives here is its name and the fields of its own
/// `configuration_workflow`, flattened by the host script exactly as a module's
/// own settings are — the provider that serves the rows stays in the worker, and
/// what crosses is what the admin has to fill in.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct TableProviderManifest {
    /// The provider's own name — `RSS feed`. v1's, unqualified; the module it
    /// came from is what disambiguates it.
    pub name: String,
    /// v1 `configFields`, as the provider's configuration workflow declared
    /// them — translated by [`crate::spec`], never interpreted here.
    #[serde(default)]
    pub config_fields: Vec<Json>,
}

/// An entity type the module exports and this version does not load.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UnsupportedEntity {
    /// The plugin key — `viewtemplates`, `table_providers`, `eventTypes`.
    pub key: String,
    /// How many of them, when that can be told without running the module's
    /// code.
    #[serde(default)]
    pub count: Option<u64>,
}

/// What a module turned out to supply.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModuleManifest {
    /// The package name it was loaded under.
    pub name: String,
    /// v1's `sc_plugin_api_version`, when it declares one.
    #[serde(default)]
    pub api_version: Option<u64>,
    /// v1's `plugin_name`, when it declares one.
    #[serde(default)]
    pub plugin_name: Option<String>,
    /// The actions it supplies.
    #[serde(default)]
    pub actions: Vec<ActionManifest>,
    /// The functions it supplies (§4a) — what a code body calls through
    /// `modfn` and what a formula hoists.
    #[serde(default)]
    pub functions: Vec<FunctionManifest>,
    /// The table providers it supplies (§8.3) — what the "new table" screen
    /// offers as a source beside a database.
    #[serde(default)]
    pub table_providers: Vec<TableProviderManifest>,
    /// The fields of its `configuration_workflow`'s forms, flattened (§5).
    #[serde(default)]
    pub config_fields: Vec<Json>,
    /// What it also supplies and this version does not load (§6).
    #[serde(default)]
    pub unsupported: Vec<UnsupportedEntity>,
    /// What went wrong that was not fatal — a step's form that would not build,
    /// an action whose `configFields` threw.
    #[serde(default)]
    pub issues: Vec<String>,
}

/// A module's own failure, as this system's error.
///
/// An **Application** error (§16): the fault is in the module or in how it was
/// configured, not in Saltcorn, and the admin who installed it is the one who
/// can act.
///
/// `pub(crate)`: [`crate::deno`] is where a module's throw arrives.
#[cfg_attr(
    not(any(feature = "deno-host", test)),
    expect(dead_code, reason = "no runtime for a module to throw on")
)]
pub(crate) fn module_error(message: &str) -> Error {
    Error::config(if message.is_empty() {
        "the module failed without saying why".to_owned()
    } else {
        message.to_owned()
    })
}

/// The module host: the worker pool a module's JavaScript runs on, and the four
/// calls the rest of the server makes of it.
///
/// A façade over [`crate::deno::DenoModuleHost`], and deliberately a thin one —
/// what it exists for is that `ModuleServices`, [`ModuleAction`](crate::action),
/// the five module endpoints and the four-step reload name a *module host* and
/// not a runtime. It is also where a build without the `deno-host` feature
/// arrives: the crate goes on building and testing without `deno_runtime` (which
/// is what keeps 444 lock-file packages and a `libclang` build requirement out
/// of every other crate's test link), and a call on such a build fails saying
/// exactly that rather than pretending.
pub struct ModuleHost {
    root: PathBuf,
    #[cfg(feature = "deno-host")]
    pool: crate::deno::DenoModuleHost,
}

impl ModuleHost {
    /// A host over the modules root, with the default number of workers.
    /// Nothing is built until the first call.
    pub fn new(root: impl Into<PathBuf>) -> ModuleHost {
        ModuleHost::with_workers(root, crate::bounds::DEFAULT_MODULE_WORKERS)
    }

    /// A host of `workers` workers (at least one) over the modules root.
    pub fn with_workers(root: impl Into<PathBuf>, workers: usize) -> ModuleHost {
        let root = root.into();
        #[cfg(not(feature = "deno-host"))]
        let _ = workers;
        ModuleHost {
            #[cfg(feature = "deno-host")]
            pool: crate::deno::DenoModuleHost::with_workers(&root, workers),
            root,
        }
    }

    /// A host whose calls are bounded by `timeout` rather than
    /// [`DEFAULT_CALL_TIMEOUT`].
    #[must_use]
    pub fn with_timeout(self, timeout: std::time::Duration) -> ModuleHost {
        #[cfg(feature = "deno-host")]
        {
            ModuleHost {
                root: self.root,
                pool: self.pool.with_timeout(timeout),
            }
        }
        #[cfg(not(feature = "deno-host"))]
        {
            let _ = timeout;
            self
        }
    }

    /// The modules root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Load (or reload) a module from `dir`, with `configuration` as the object
    /// handed to v1's `actions(cfg)` and `permissions` as what its worker may
    /// reach (§2).
    ///
    /// Idempotent, and idempotent **on the same worker**: a reload after a
    /// configuration change replaces the module where its state already is,
    /// rather than leaving a second copy of it somewhere else. A change to the
    /// *permissions* is the exception, and has to be: the set belongs to the
    /// isolate, so the module moves to a worker that grants it — losing whatever
    /// it was holding, exactly as a restart would.
    pub async fn load(
        &self,
        name: &str,
        dir: &Path,
        configuration: &Json,
        permissions: &ModulePermissions,
    ) -> Result<ModuleManifest> {
        #[cfg(feature = "deno-host")]
        {
            self.pool.load(name, dir, configuration, permissions).await
        }
        #[cfg(not(feature = "deno-host"))]
        {
            let _ = (name, dir, configuration, permissions);
            Err(no_runtime())
        }
    }

    /// Forget a module — after an uninstall, so a restarted worker does not
    /// reload a package that is no longer there.
    pub async fn unload(&self, name: &str) {
        #[cfg(feature = "deno-host")]
        {
            self.pool.unload(name).await;
        }
        #[cfg(not(feature = "deno-host"))]
        {
            let _ = name;
        }
    }

    /// Run one action of one module with v1's argument object.
    pub async fn run(&self, module: &str, action: &str, args: Json) -> Result<Json> {
        #[cfg(feature = "deno-host")]
        {
            self.pool.run(module, action, args).await
        }
        #[cfg(not(feature = "deno-host"))]
        {
            let _ = (module, action, args);
            Err(no_runtime())
        }
    }

    /// Call one **function** of one module with v1's positional arguments
    /// (§4a).
    ///
    /// The fifth host surface, and routed exactly as [`run`](ModuleHost::run)
    /// is: to the worker the module was loaded on, because a v1 function closes
    /// over what its module built at load time — a `markdown-it`, a
    /// `Nominatim`, the module's own configuration — and that lives in one
    /// place because a module is loaded once.
    pub async fn call(&self, module: &str, function: &str, args: Vec<Json>) -> Result<Json> {
        #[cfg(feature = "deno-host")]
        {
            self.pool.call(module, function, args).await
        }
        #[cfg(not(feature = "deno-host"))]
        {
            let _ = (module, function, args);
            Err(no_runtime())
        }
    }

    /// The fields one **table provider** presents for one configuration (§8.3).
    ///
    /// Asked on every catalog reload rather than stored: the columns are the
    /// module's answer, so an upgraded package that presents a new column
    /// presents it. Routed like [`run`](ModuleHost::run) and for the same
    /// reason — `fields(cfg)` is a closure the module built at load time.
    pub async fn provider_fields(
        &self,
        module: &str,
        provider: &str,
        configuration: &Json,
    ) -> Result<Vec<Json>> {
        #[cfg(feature = "deno-host")]
        {
            self.pool
                .provider_fields(module, provider, configuration)
                .await
        }
        #[cfg(not(feature = "deno-host"))]
        {
            let _ = (module, provider, configuration);
            Err(no_runtime())
        }
    }

    /// One table provider's rows, for v1's `where`/`options` pair.
    ///
    /// The pair is a hint the provider may honour or ignore; the caller applies
    /// the query to the answer either way (`sc_catalog::inmem`).
    pub async fn provider_rows(
        &self,
        module: &str,
        provider: &str,
        configuration: &Json,
        table: &str,
        filter: &Json,
        options: &Json,
    ) -> Result<Vec<Json>> {
        #[cfg(feature = "deno-host")]
        {
            self.pool
                .provider_rows(module, provider, configuration, table, filter, options)
                .await
        }
        #[cfg(not(feature = "deno-host"))]
        {
            let _ = (module, provider, configuration, table, filter, options);
            Err(no_runtime())
        }
    }

    /// Which of v1's three write methods `get_table(configuration)` answers.
    ///
    /// v1 has no declaration of writability: the object `get_table` returns
    /// carries `insertRow`/`updateRow`/`deleteRows` or it does not, which is how
    /// `@saltcorn/postgres-tables`'s `read_only` flag works. So this is a
    /// property of the *configuration*, asked once per catalog reload.
    pub async fn provider_writes(
        &self,
        module: &str,
        provider: &str,
        configuration: &Json,
        table: &str,
    ) -> Result<Json> {
        #[cfg(feature = "deno-host")]
        {
            self.pool
                .provider_writes(module, provider, configuration, table)
                .await
        }
        #[cfg(not(feature = "deno-host"))]
        {
            let _ = (module, provider, configuration, table);
            Err(no_runtime())
        }
    }

    /// v1's `insertRow(record)`: `{ key }`, the new row's primary key or null.
    pub async fn provider_insert(
        &self,
        module: &str,
        provider: &str,
        configuration: &Json,
        table: &str,
        record: &Json,
    ) -> Result<Json> {
        #[cfg(feature = "deno-host")]
        {
            self.pool
                .provider_insert(module, provider, configuration, table, record)
                .await
        }
        #[cfg(not(feature = "deno-host"))]
        {
            let _ = (module, provider, configuration, table, record);
            Err(no_runtime())
        }
    }

    /// v1's `updateRow(record, id)`.
    pub async fn provider_update(
        &self,
        module: &str,
        provider: &str,
        configuration: &Json,
        table: &str,
        id: &Json,
        record: &Json,
    ) -> Result<Json> {
        #[cfg(feature = "deno-host")]
        {
            self.pool
                .provider_update(module, provider, configuration, table, id, record)
                .await
        }
        #[cfg(not(feature = "deno-host"))]
        {
            let _ = (module, provider, configuration, table, id, record);
            Err(no_runtime())
        }
    }

    /// v1's `deleteRows(where)`.
    pub async fn provider_delete(
        &self,
        module: &str,
        provider: &str,
        configuration: &Json,
        table: &str,
        filter: &Json,
    ) -> Result<Json> {
        #[cfg(feature = "deno-host")]
        {
            self.pool
                .provider_delete(module, provider, configuration, table, filter)
                .await
        }
        #[cfg(not(feature = "deno-host"))]
        {
            let _ = (module, provider, configuration, table, filter);
            Err(no_runtime())
        }
    }

    /// Ask the host to say hello — what a test and a diagnostics screen use to
    /// find out whether the pool starts at all.
    pub async fn ping(&self) -> Result<Json> {
        #[cfg(feature = "deno-host")]
        {
            self.pool.ping().await
        }
        #[cfg(not(feature = "deno-host"))]
        {
            Err(no_runtime())
        }
    }

    /// Which worker a module lives on, or `None` if it has never been loaded.
    /// The Modules tab's answer to "where is this thing running".
    pub async fn worker_of(&self, module: &str) -> Option<usize> {
        #[cfg(feature = "deno-host")]
        {
            self.pool.worker_of(module).await
        }
        #[cfg(not(feature = "deno-host"))]
        {
            let _ = module;
            None
        }
    }

    /// Stop every worker and wait for its thread.
    pub async fn shutdown(&self) {
        #[cfg(feature = "deno-host")]
        {
            self.pool.shutdown().await;
        }
    }
}

/// What a build with no module runtime answers, rather than a silence or a
/// timeout.
#[cfg(not(feature = "deno-host"))]
fn no_runtime() -> Error {
    Error::config(
        "this build of Saltcorn has no module runtime: it was compiled without sc-module's \
         `deno-host` feature, which is what the server binary turns on",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_modules_throw_is_the_admins_problem_and_not_saltcorns() {
        let err = module_error("connect ECONNREFUSED");
        assert!(err.to_string().contains("ECONNREFUSED"), "{err}");
        // The module's fault, not the server's: it is the admin who installed it
        // who can fix it (§16's split).
        assert_eq!(err.kind(), sc_error::ErrorKind::Application);
    }

    #[test]
    fn a_throw_with_nothing_to_say_still_says_something() {
        assert!(module_error("").to_string().contains("without saying why"));
    }

    #[test]
    fn the_host_script_is_carried_in_the_binary() {
        // The script is written from here at every worker start, so a checkout
        // whose `js/` directory moved would be a host that cannot start — worth
        // one assertion that the file is really compiled in.
        assert!(
            HOST_SCRIPT.contains("module-host"),
            "the script looks wrong"
        );
        assert!(HOST_SCRIPT.contains("@saltcorn/"), "the stubs are missing");
    }

    #[test]
    fn the_host_script_speaks_the_seam_and_not_a_pipe() {
        // The three functions [`crate::deno`] installs, and the entry point it
        // calls. If either side is renamed without the other, a worker starts
        // and never answers — so the pair is asserted here, where a rename is
        // one grep away from both.
        for name in ["__scModuleHost", "__scDone", "__scFail", "__scLog"] {
            assert!(HOST_SCRIPT.contains(name), "{name} is missing");
        }
        // And nothing is left of the transport: no framing, no stdout, no
        // readline.
        assert!(
            !HOST_SCRIPT.contains("readline"),
            "the newline-JSON loop is still there"
        );
        assert!(
            !HOST_SCRIPT.contains("process.stdout"),
            "the host script still writes to stdout"
        );
    }
}
