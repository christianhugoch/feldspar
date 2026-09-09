//! A module's **functions**, as the fifth host surface (TODO "Modules
//! in-process", §4a).
//!
//! A v1 plugin supplies `functions` beside `actions`, and v1 makes them
//! "available to formulas and code actions". [`ModuleFunctions`] is the
//! implementation of `sc-expr`'s [`ModuleFnHost`] seam for *this* server: one
//! JSON plan in, one JSON value out, routed to the worker the named module is
//! loaded on.
//!
//! # Why it is here rather than in `sc-api`
//!
//! The list this milestone was written from puts it in `sc-api`, over
//! `ModuleServices`. Neither half of that is reachable: `sc-api` does not depend
//! on this crate (they are siblings — both above `sc-catalog` and `sc-action`),
//! and `ModuleServices` lives higher still, in `sc-server`. What the seam
//! actually needs is a [`ModuleHost`] and a [`ModuleSet`], which are both *here*,
//! so the implementation is here and `sc-server` is the one line that installs
//! it on the catalog.
//!
//! # The routing is the point
//!
//! Every v1 function closes over something its module built at load time — a
//! `markdown-it`, a `Nominatim`, the module's own configuration. A module is
//! loaded **once**, on **one** worker, so the call has to go there. That is what
//! [`ModuleHost::call`] does, and it is why this is a hop at all: module state
//! is a singleton, and no arrangement of isolate pools changes it.
//!
//! # Nothing here is trusted
//!
//! The guest resolves `modfn.md_to_html` against the same list this hands it,
//! and the plan it sends names a module and a function. Both are checked again
//! here, because a bound the guest could edit is not a bound — and because the
//! set can be reloaded between a run starting and its call arriving, at which
//! point the honest answer is a sentence naming the function that went away.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sc_error::{Error, Result};
use sc_expr::{ModuleFnArg, ModuleFnHost, ModuleFunction};
use serde::Deserialize;
use serde_json::Value as Json;

use crate::host::ModuleHost;
use crate::modules::ModuleSet;

/// The module functions this server supplies, over the pool they run on.
///
/// Built whole on every module change, exactly as [`ModuleSet`] is: installing,
/// configuring or deleting a module changes what functions exist and with what
/// configuration behind them, so the answer is rebuilt rather than patched.
pub struct ModuleFunctions {
    host: Arc<ModuleHost>,
    functions: Vec<ModuleFunction>,
}

impl ModuleFunctions {
    /// The functions of every loaded module in `set`, over `host`.
    ///
    /// A module that would not load supplies none — it has no manifest, and a
    /// function nobody can call is not one to complete in an editor.
    pub fn new(host: &Arc<ModuleHost>, set: &ModuleSet) -> ModuleFunctions {
        let mut functions = Vec::new();
        for loaded in set.modules() {
            let Some(manifest) = &loaded.manifest else {
                continue;
            };
            for function in &manifest.functions {
                functions.push(ModuleFunction {
                    module: loaded.module.name.clone(),
                    name: function.name.clone(),
                    description: function.description.clone(),
                    is_async: function.is_async,
                    arguments: function
                        .arguments
                        .iter()
                        .map(|argument| ModuleFnArg {
                            name: argument.name.clone(),
                            type_name: argument.type_name.clone(),
                        })
                        .collect(),
                });
            }
        }
        ModuleFunctions {
            host: Arc::clone(host),
            functions,
        }
    }

    /// An empty set — a server with no modules, and the starting point for a
    /// test that wants the seam without a worker.
    pub fn empty(host: &Arc<ModuleHost>) -> ModuleFunctions {
        ModuleFunctions {
            host: Arc::clone(host),
            functions: Vec::new(),
        }
    }

    /// Every function, in the order the module set has them.
    pub fn functions(&self) -> &[ModuleFunction] {
        &self.functions
    }
}

/// One call, as it arrives from a guest.
#[derive(Debug, Deserialize)]
struct Request {
    /// The package that supplies the function. Always present: a function name
    /// is not unique, and which module was meant is the caller's to know.
    module: String,
    /// The function, by the name v1 registered it under.
    #[serde(rename = "function")]
    function: String,
    /// v1's positional arguments, as JSON.
    #[serde(default)]
    args: Vec<Json>,
    /// What is left of the calling body's wall clock, filled in by the op.
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[async_trait]
impl ModuleFnHost for ModuleFunctions {
    async fn call(&self, request: Json) -> Result<Json> {
        let request: Request = serde_json::from_value(request)
            .map_err(|e| Error::invalid(format!("this module function call is not one: {e}")))?;
        // Re-checked here even though the guest resolved the name against this
        // same list: the set is rebuilt on every module change, and a call in
        // flight across one should say what happened rather than reach a worker
        // that no longer has the module.
        if !self
            .functions
            .iter()
            .any(|f| f.module == request.module && f.name == request.function)
        {
            return Err(Error::invalid(format!(
                "the module `{}` supplies no function `{}`",
                request.module, request.function
            )));
        }
        // No surfaces and no schema: a module function is hoisted into a formula
        // and called from inside one, so there is no caller's authority to lend
        // it and no spare database connection to lend it with. A `Table` reached
        // from one says so by name (`CallHosts::default`).
        let call = self.host.call(
            &request.module,
            &request.function,
            request.args,
            crate::host::CallHosts::default(),
        );
        // The caller's clock, when it named one: the pool's own bound is the
        // 120 s a Proxmox snapshot needs, which is not a bound a five-second
        // code body can be held to.
        match request.timeout_ms {
            None => call.await,
            Some(ms) => match tokio::time::timeout(Duration::from_millis(ms), call).await {
                Ok(result) => result,
                Err(_) => Err(Error::invalid(format!(
                    "the module function `{}` of `{}` took longer than the {ms} ms its caller \
                     had left",
                    request.function, request.module
                ))),
            },
        }
    }

    fn functions(&self) -> Vec<ModuleFunction> {
        self.functions.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_call_naming_a_function_nothing_supplies_is_refused_before_a_worker_starts() {
        // No worker is built here — the pool is lazy — so this asserts exactly
        // what it says: the check happens on this side, and a name the set does
        // not have never reaches an isolate.
        let host = Arc::new(ModuleHost::new("/nonexistent/modules"));
        let functions = ModuleFunctions::empty(&host);
        let err = functions
            .call(serde_json::json!({
                "module": "@saltcorn/markdown", "function": "md_to_html", "args": ["x"]
            }))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("md_to_html"), "{err}");
        assert!(err.contains("@saltcorn/markdown"), "{err}");
        assert!(functions.functions().is_empty());
    }

    #[tokio::test]
    async fn a_plan_that_is_not_one_says_so_rather_than_reaching_a_module() {
        let host = Arc::new(ModuleHost::new("/nonexistent/modules"));
        let functions = ModuleFunctions::empty(&host);
        let err = functions
            .call(serde_json::json!({ "function": "md_to_html" }))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("is not one"), "{err}");
    }
}
