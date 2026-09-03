//! A Python module's **functions**, as the fifth host surface.
//!
//! [`sc_module::ModuleFunctions`]' sibling in every respect that matters: built
//! whole on every module change, checking every name again on this side because
//! the set can be rebuilt between a run starting and its call arriving, and
//! answering the same `{module, function, args}` plan a JavaScript module's
//! function answers. A formula that hoists `md_to_html` and a body that writes
//! `modfn.md_to_html(x)` cannot tell which language is under it, which is §8's
//! whole requirement.
//!
//! # A function reaches no host surfaces
//!
//! Deliberate, and worth saying rather than discovering. A module function is
//! called from two places — a formula's hoisted call, and a code body's `modfn`
//! — and *both* of them are already inside something: a query being prepared, or
//! a run holding its own budgets. There is no caller's authority to lend a
//! function, and lending it the admin's would make `db` inside a formula's
//! helper a way around every ownership rule the formula was being evaluated for.
//! So `saltcorn.db` inside a function raises the same sentence it raises
//! anywhere outside a run that holds it, and a plugin that needs the database
//! does its work in an **action**, which has a caller.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sc_error::{Error, Result};
use sc_expr::{ModuleFnArg, ModuleFnHost, ModuleFunction};
use sc_module::LoadedModule;
use serde::Deserialize;
use serde_json::Value as Json;

use super::host::PyModuleHost;

/// The functions this server's Python modules supply.
pub struct PyModuleFunctions {
    host: Arc<PyModuleHost>,
    functions: Vec<ModuleFunction>,
}

impl PyModuleFunctions {
    /// The functions of every loaded Python module, over `host`.
    ///
    /// A module that would not load supplies none — it has no manifest, and a
    /// function nobody can call is not one to complete in an editor.
    #[must_use]
    pub fn new(host: &Arc<PyModuleHost>, modules: &[LoadedModule]) -> PyModuleFunctions {
        let mut functions = Vec::new();
        for loaded in modules {
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
        PyModuleFunctions {
            host: Arc::clone(host),
            functions,
        }
    }

    /// An empty set — a server with no Python modules, and the starting point
    /// for a test that wants the seam without an interpreter.
    #[must_use]
    pub fn empty(host: &Arc<PyModuleHost>) -> PyModuleFunctions {
        PyModuleFunctions {
            host: Arc::clone(host),
            functions: Vec::new(),
        }
    }

    /// Every function, in the order the module set has them.
    #[must_use]
    pub fn functions(&self) -> &[ModuleFunction] {
        &self.functions
    }
}

/// One call, as it arrives from a guest.
#[derive(Debug, Deserialize)]
struct Request {
    /// The distribution that supplies the function.
    module: String,
    function: String,
    #[serde(default)]
    args: Vec<Json>,
    /// What is left of the calling body's wall clock, filled in by the bridge.
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[async_trait]
impl ModuleFnHost for PyModuleFunctions {
    async fn call(&self, request: Json) -> Result<Json> {
        let request: Request = serde_json::from_value(request)
            .map_err(|e| Error::invalid(format!("this module function call is not one: {e}")))?;
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
        let call = self
            .host
            .call(&request.module, &request.function, request.args);
        // The caller's clock where it named one, exactly as the other language's
        // host does it: the module host's own bound is two minutes, which is not
        // a bound a five-second code body can be held to.
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
    use crate::PythonRuntime;

    #[tokio::test]
    async fn a_call_naming_a_function_nothing_supplies_is_refused_before_the_interpreter() {
        // Nothing is started here — the check is on this side — so this asserts
        // exactly what it says, in a build with no interpreter as well as one
        // with.
        let host = Arc::new(super::PyModuleHost::new(Arc::new(PythonRuntime::new())));
        let functions = PyModuleFunctions::empty(&host);
        let err = functions
            .call(serde_json::json!({
                "module": "sc-fixture", "function": "shout", "args": ["x"]
            }))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("shout") && err.contains("sc-fixture"), "{err}");
        assert!(functions.functions().is_empty());
    }

    #[tokio::test]
    async fn a_plan_that_is_not_one_says_so_rather_than_reaching_a_module() {
        let host = Arc::new(super::PyModuleHost::new(Arc::new(PythonRuntime::new())));
        let functions = PyModuleFunctions::empty(&host);
        let err = functions
            .call(serde_json::json!({ "function": "shout" }))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("is not one"), "{err}");
    }
}
