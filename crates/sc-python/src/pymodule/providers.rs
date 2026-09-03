//! A Python module's **table providers**, as `sc-catalog`'s
//! [`TableProviderHost`].
//!
//! [`sc_module::ModuleTableProviders`]' sibling, and the same shape for the same
//! reasons: built whole on every module change, every name checked again on this
//! side because a module can be deleted between a catalog reload and a query,
//! and the narrowing from this system's `Statement` into the three write
//! signatures left where it already is (`sc_catalog`), so what crosses here is a
//! record, an id and a `where` object.
//!
//! # Writing, and what decides it
//!
//! Whether a table backed by a provider is writable is decided by **which
//! methods the provider class defines** — `insert_row`, `update_row`,
//! `delete_rows` — which is v1's rule said in Python. It is asked of the module
//! rather than declared, so a provider that grows a write method grows the
//! button that uses it at the next reload.
//!
//! It is a property of the provider here and of the *configuration* in v1
//! (`@saltcorn/postgres-tables` omits all three when its `read_only` flag is
//! set), and the seam allows either: the configuration travels with the
//! question, so a Python provider that wants to answer differently for two
//! tables is free to grow that later without anything above it changing.

use std::sync::Arc;

use async_trait::async_trait;
use sc_catalog::{DataField, ProvidedWrites, TableProviderHost, TableProviderKind};
use sc_error::{Error, Result};
use sc_module::LoadedModule;
use serde_json::Value as Json;

use super::fields;
use super::host::PyModuleHost;

/// The table providers this server's Python modules supply.
pub struct PyModuleTableProviders {
    host: Arc<PyModuleHost>,
    providers: Vec<TableProviderKind>,
}

impl PyModuleTableProviders {
    /// Every provider of every loaded Python module, over `host`.
    #[must_use]
    pub fn new(host: &Arc<PyModuleHost>, modules: &[LoadedModule]) -> PyModuleTableProviders {
        let mut providers = Vec::new();
        for loaded in modules {
            let Some(manifest) = &loaded.manifest else {
                continue;
            };
            for provider in &manifest.table_providers {
                providers.push(TableProviderKind {
                    module: loaded.module.name.clone(),
                    provider: provider.name.clone(),
                    config_spec: fields::form_fields(&provider.config_fields),
                });
            }
        }
        PyModuleTableProviders {
            host: Arc::clone(host),
            providers,
        }
    }

    /// An empty set — a server with no Python modules.
    #[must_use]
    pub fn empty(host: &Arc<PyModuleHost>) -> PyModuleTableProviders {
        PyModuleTableProviders {
            host: Arc::clone(host),
            providers: Vec::new(),
        }
    }

    /// Refuse a name this set does not have, before the interpreter is reached.
    fn require(&self, module: &str, provider: &str) -> Result<()> {
        if self
            .providers
            .iter()
            .any(|p| p.module == module && p.provider == provider)
        {
            return Ok(());
        }
        Err(Error::not_found(format!(
            "no installed module supplies the table provider `{provider}` of `{module}`; it may \
             have been uninstalled, or failed to load"
        )))
    }

    /// Every provider, in the order the module set has them.
    #[must_use]
    pub fn providers(&self) -> &[TableProviderKind] {
        &self.providers
    }
}

#[async_trait]
impl TableProviderHost for PyModuleTableProviders {
    fn providers(&self) -> Vec<TableProviderKind> {
        self.providers.clone()
    }

    async fn fields(&self, module: &str, provider: &str, config: &Json) -> Result<Vec<DataField>> {
        self.require(module, provider)?;
        let declared = self.host.provider_fields(module, provider, config).await?;
        Ok(declared.iter().filter_map(fields::data_field).collect())
    }

    async fn rows(
        &self,
        module: &str,
        provider: &str,
        table: &str,
        config: &Json,
        filter: &Json,
        options: &Json,
    ) -> Result<Vec<Json>> {
        self.require(module, provider)?;
        self.host
            .provider_rows(module, provider, config, table, filter, options)
            .await
    }

    async fn writes(
        &self,
        module: &str,
        provider: &str,
        table: &str,
        config: &Json,
    ) -> Result<ProvidedWrites> {
        // The table is not part of the question here — which methods a class
        // defines is a property of the class — and it is in the signature
        // because the seam is v1's too, where it is.
        let _ = table;
        self.require(module, provider)?;
        let answer = self.host.provider_writes(module, provider, config).await?;
        Ok(ProvidedWrites::from_json(&answer))
    }

    async fn insert(
        &self,
        module: &str,
        provider: &str,
        table: &str,
        config: &Json,
        record: &Json,
    ) -> Result<Json> {
        let _ = table;
        self.require(module, provider)?;
        let answer = self
            .host
            .provider_insert(module, provider, config, record)
            .await?;
        // A provider may answer nothing, so a missing key is `null` rather than
        // an error: the caller reads the row back through what it wrote.
        Ok(answer.get("key").cloned().unwrap_or(Json::Null))
    }

    async fn update(
        &self,
        module: &str,
        provider: &str,
        table: &str,
        config: &Json,
        id: &Json,
        record: &Json,
    ) -> Result<()> {
        let _ = table;
        self.require(module, provider)?;
        self.host
            .provider_update(module, provider, config, id, record)
            .await?;
        Ok(())
    }

    async fn delete(
        &self,
        module: &str,
        provider: &str,
        table: &str,
        config: &Json,
        filter: &Json,
    ) -> Result<()> {
        let _ = table;
        self.require(module, provider)?;
        self.host
            .provider_delete(module, provider, config, filter)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PythonRuntime;
    use serde_json::json;

    #[tokio::test]
    async fn a_provider_nothing_supplies_is_refused_before_the_interpreter() {
        let host = Arc::new(PyModuleHost::new(Arc::new(PythonRuntime::new())));
        let providers = PyModuleTableProviders::empty(&host);
        let err = providers
            .fields("sc-fixture", "Fixture rows", &json!({}))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("Fixture rows") && err.contains("sc-fixture"),
            "{err}"
        );
        assert!(TableProviderHost::providers(&providers).is_empty());
    }
}
