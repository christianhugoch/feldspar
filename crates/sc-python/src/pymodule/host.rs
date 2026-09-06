//! The Python module host: what the rest of the server asks of an installed
//! plugin, and nothing else.
//!
//! [`sc_module::ModuleHost`]'s counterpart, call for call — `load`, `unload`,
//! `run`, `call`, and the table provider's six — so that everything above it can
//! be written once for both languages. What is underneath is entirely different
//! and entirely uninteresting from here: a JavaScript module is a package on a
//! Deno worker, a Python module is a distribution in this server's environment
//! imported into the one interpreter, and both answer the same
//! [`ModuleManifest`].
//!
//! # Every call is a run
//!
//! Not "a call into a worker". A module's action is a [`PythonRuntime`] run like
//! a code body is: it takes an admission slot, it gets a thread, it is under a
//! deadline, and it is stopped by the same four instruments (§1, §4). That is
//! what makes the split JavaScript needs — code bodies on one pool, modules on
//! another, because V8's watchdog kills a whole isolate — unnecessary here:
//! CPython's instrument targets one thread, so a runaway body cannot take a
//! module's state with it.
//!
//! # What a call may reach
//!
//! An **action** reaches the five surfaces its caller has, which is §2's whole
//! point: a Python plugin has no v1 to be compatible with, so it is handed the
//! plans directly with the same authority rules a code body gets. Everything
//! else — `on_load`, a function, a table provider — reaches none, and says so at
//! the call site rather than silently doing nothing. That is not an oversight:
//! a function is hoisted into a formula and a provider is called from inside a
//! query, and neither of those has a caller's authority to lend or a spare
//! database connection to lend it with.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use sc_error::{Error, Result};
use sc_expr::CodeHosts;
use sc_module::ModuleManifest;
use serde_json::{Value as Json, json};

use crate::{PluginCall, PythonEnvironment, PythonRuntime};

/// How long one call into a Python module may take.
///
/// `sc_module`'s own bound for the other language, imported rather than chosen
/// again: what a module call is allowed is a property of module calls, and two
/// numbers would be two answers to "why did my action stop after two minutes".
/// It is **not** `MAX_CODE_TIMEOUT`, which is the ceiling on a *body* — that one
/// bounds a hold an admin typed into a trigger form, and this one bounds
/// somebody's `requests.post`.
pub const DEFAULT_CALL_TIMEOUT: Duration = sc_module::DEFAULT_CALL_TIMEOUT;

/// The Python modules this server has loaded, and the calls into them.
pub struct PyModuleHost {
    python: Arc<PythonRuntime>,
    timeout: Duration,
}

impl PyModuleHost {
    /// A host over `python` — the same runtime the dispatcher took as a code
    /// adapter, because there is one interpreter in a process and a Python
    /// module and a Python body share it.
    #[must_use]
    pub fn new(python: Arc<PythonRuntime>) -> PyModuleHost {
        PyModuleHost {
            python,
            timeout: DEFAULT_CALL_TIMEOUT,
        }
    }

    /// A host whose calls are bounded by `timeout` rather than
    /// [`DEFAULT_CALL_TIMEOUT`].
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> PyModuleHost {
        self.timeout = timeout;
        self
    }

    /// The runtime these modules run on.
    #[must_use]
    pub fn python(&self) -> &Arc<PythonRuntime> {
        &self.python
    }

    /// The environment a Python module's distribution is installed in, as far as
    /// it can be determined without starting an interpreter.
    ///
    /// `None` only on a machine with no `--python-dir` and no data directory to
    /// default to, which is [`crate::default_python_dir`]'s own refusal.
    #[must_use]
    pub fn environment(&self) -> Option<PythonEnvironment> {
        let embedded = self
            .python
            .state()
            .version()
            .and_then(crate::env::parse_version);
        PythonEnvironment::new(self.python.env(), embedded).ok()
    }

    /// Where a distribution lands on the disk, for the loader's own check and
    /// for the `sys.path` entry a load carries.
    fn site_packages(&self) -> Option<PathBuf> {
        self.environment()
            .as_ref()
            .and_then(PythonEnvironment::site_packages)
    }

    /// Import a module's distribution, read what it declares, and call its
    /// `on_load` with `configuration` (§11).
    ///
    /// Idempotent, and a **reload** rather than a no-op the second time: the
    /// package's modules are dropped from `sys.modules` and imported again, so a
    /// decorator an upgrade removed stops being an action. Best-effort for
    /// anything with a C extension in it, which is §11's whole paragraph and the
    /// reason the Modules tab says a version change takes full effect at the
    /// next restart.
    pub async fn load(&self, module: &str, configuration: &Json) -> Result<ModuleManifest> {
        let payload = json!({
            "module": module,
            "configuration": configuration,
            // The interpreter fixed its `sys.path` at start, and an install is a
            // subprocess that may have happened since — including the one that
            // *created* the environment. So the load carries where its packages
            // are, and the Python half appends it if it is not already there.
            "site_packages": self.site_packages().map(|p| p.to_string_lossy().into_owned()),
        });
        let answer = self.call_op("load", payload, CodeHosts::default()).await?;
        serde_json::from_value(answer).map_err(|e| {
            Error::msg(format!(
                "the Python module `{module}` answered a manifest this server could not \
                 read: {e}"
            ))
        })
    }

    /// Forget a module — after an uninstall, so nothing here answers for a
    /// distribution that is no longer on the disk.
    ///
    /// The **import** stays behind. Python has no unload, §11 says so, and this
    /// is the one place where saying it is the whole of what can be done.
    pub async fn unload(&self, module: &str) {
        let _ = self
            .call_op("unload", json!({ "module": module }), CodeHosts::default())
            .await;
    }

    /// Run one action of one module, over the five surfaces its caller has.
    pub async fn run(
        &self,
        module: &str,
        action: &str,
        args: Json,
        hosts: CodeHosts<'_>,
    ) -> Result<Json> {
        self.call_op(
            "action",
            json!({ "module": module, "action": action, "args": args }),
            hosts,
        )
        .await
    }

    /// Call one **function** of one module with its positional arguments.
    pub async fn call(&self, module: &str, function: &str, args: Vec<Json>) -> Result<Json> {
        self.call_op(
            "function",
            json!({ "module": module, "function": function, "args": args }),
            CodeHosts::default(),
        )
        .await
    }

    /// The fields one table provider presents for one configuration.
    pub async fn provider_fields(
        &self,
        module: &str,
        provider: &str,
        configuration: &Json,
    ) -> Result<Vec<Json>> {
        let answer = self
            .call_op(
                "provider_fields",
                json!({
                    "module": module, "provider": provider, "configuration": configuration
                }),
                CodeHosts::default(),
            )
            .await?;
        list(answer, "fields")
    }

    /// One table provider's rows, for the `where`/`options` pair.
    pub async fn provider_rows(
        &self,
        module: &str,
        provider: &str,
        configuration: &Json,
        table: &str,
        filter: &Json,
        options: &Json,
    ) -> Result<Vec<Json>> {
        let answer = self
            .call_op(
                "provider_rows",
                json!({
                    "module": module, "provider": provider, "configuration": configuration,
                    "table": table, "where": filter, "options": options
                }),
                CodeHosts::default(),
            )
            .await?;
        list(answer, "rows")
    }

    /// Which of the three write methods the provider **defines** — which is what
    /// decides whether a table backed by it is writable, exactly as it is in v1.
    pub async fn provider_writes(
        &self,
        module: &str,
        provider: &str,
        configuration: &Json,
    ) -> Result<Json> {
        self.call_op(
            "provider_writes",
            json!({ "module": module, "provider": provider, "configuration": configuration }),
            CodeHosts::default(),
        )
        .await
    }

    /// `insert_row(configuration, record)`: `{ key }`, the new row's primary key
    /// or null.
    pub async fn provider_insert(
        &self,
        module: &str,
        provider: &str,
        configuration: &Json,
        record: &Json,
    ) -> Result<Json> {
        self.call_op(
            "provider_insert",
            json!({
                "module": module, "provider": provider, "configuration": configuration,
                "record": record
            }),
            CodeHosts::default(),
        )
        .await
    }

    /// `update_row(configuration, record, id)`.
    pub async fn provider_update(
        &self,
        module: &str,
        provider: &str,
        configuration: &Json,
        id: &Json,
        record: &Json,
    ) -> Result<Json> {
        self.call_op(
            "provider_update",
            json!({
                "module": module, "provider": provider, "configuration": configuration,
                "id": id, "record": record
            }),
            CodeHosts::default(),
        )
        .await
    }

    /// `delete_rows(configuration, where)`.
    pub async fn provider_delete(
        &self,
        module: &str,
        provider: &str,
        configuration: &Json,
        filter: &Json,
    ) -> Result<Json> {
        self.call_op(
            "provider_delete",
            json!({
                "module": module, "provider": provider, "configuration": configuration,
                "where": filter
            }),
            CodeHosts::default(),
        )
        .await
    }

    /// **Fit** one of a module's model providers, over a columnar frame.
    ///
    /// The frame crosses as columns, which is what lets it land on the Python
    /// side as something `numpy.asarray` takes directly — and what keeps a
    /// 50 000 × 12 dataset twelve arrays rather than 50 000 objects with the
    /// same twelve keys repeated.
    pub async fn model_fit(
        &self,
        module: &str,
        provider: &str,
        frame: &Json,
        configuration: &Json,
        hyperparameters: &Json,
    ) -> Result<Json> {
        self.call_op(
            "model_fit",
            json!({
                "module": module, "provider": provider, "frame": frame,
                "configuration": configuration, "hyperparameters": hyperparameters
            }),
            CodeHosts::default(),
        )
        .await
    }

    /// **Predict** with one, over a frame of any height.
    pub async fn model_predict(
        &self,
        module: &str,
        provider: &str,
        state: &Json,
        frame: &Json,
    ) -> Result<Json> {
        self.call_op(
            "model_predict",
            json!({
                "module": module, "provider": provider, "state": state, "frame": frame
            }),
            CodeHosts::default(),
        )
        .await
    }

    /// One op, as a run.
    async fn call_op(&self, op: &'static str, payload: Json, hosts: CodeHosts<'_>) -> Result<Json> {
        self.python
            .call_plugin(PluginCall::new(op, payload, self.timeout).with_hosts(hosts))
            .await
    }
}

/// A Python answer that has to be a list, as one.
///
/// A plugin that answers a dict where rows were asked for is a plugin with a
/// bug, and it is named here rather than turning into an empty table.
fn list(answer: Json, what: &str) -> Result<Vec<Json>> {
    match answer {
        Json::Array(values) => Ok(values),
        Json::Null => Ok(Vec::new()),
        other => Err(Error::config(format!(
            "this Python table provider answered its {what} with {other}, which is not a list"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_host_over_a_runtime_that_will_not_start_says_so_rather_than_hanging() {
        // No interpreter is started here in either build: a process with
        // `--python off` refuses before it would, and a build without the
        // feature has none to start. Both answer with a sentence naming the
        // reason, which is the whole of what a module host owes a caller that
        // cannot be served.
        let python = Arc::new(PythonRuntime::new().with_enabled(false));
        let host = PyModuleHost::new(python);
        let err = host
            .load("sc-fixture", &json!({}))
            .await
            .expect_err("nothing can be loaded");
        let message = err.to_string();
        assert!(
            message.contains("--python off") || message.contains("without Python support"),
            "{message}"
        );
        // And unloading is best effort, so it cannot fail a delete.
        host.unload("sc-fixture").await;
    }

    #[test]
    fn a_provider_that_answers_the_wrong_shape_is_named_rather_than_read_as_empty() {
        assert!(list(json!([{ "id": 1 }]), "rows").expect("a list").len() == 1);
        assert!(list(Json::Null, "rows").expect("nothing").is_empty());
        let err = list(json!({ "id": 1 }), "rows").expect_err("not a list");
        assert!(err.to_string().contains("not a list"), "{err}");
    }
}
