//! A module's **table providers**, as `sc-catalog`'s
//! [`TableProviderHost`](sc_catalog::TableProviderHost) (design §8.3).
//!
//! A v1 plugin may export `table_providers` beside its actions and functions:
//!
//! ```js
//! table_providers: {
//!   "RSS feed": {
//!     configuration_workflow,                       // this provider's settings
//!     fields: [{ name: "title", type: "String" }],  // or a function of the config
//!     get_table: (cfg) => ({ getRows: async (where, opts) => [...] }),
//!   },
//! }
//! ```
//!
//! and a table whose `_sc_tables` row names one is a **provided** table: it
//! exists because the row does, its columns are what `fields(cfg)` answers, and
//! its rows are what `get_table(cfg).getRows(...)` answers.
//!
//! [`ModuleTableProviders`] is the implementation of that seam for this server,
//! and it is [`ModuleFunctions`](crate::ModuleFunctions)' sibling in every
//! respect: built whole on every module change, routed to the worker the named
//! module is loaded on, and checking every name again on this side because the
//! set can be rebuilt between a catalog reload starting and a query arriving.
//!
//! # Why the routing matters here too
//!
//! `get_table(cfg)` is a closure over what the module built at load time —
//! `@saltcorn/postgres-tables` returns one holding a `pg` pool, and a second
//! copy of it on another isolate would be a second pool. A module is loaded once,
//! on one worker, so the call goes there.
//!
//! # Writing
//!
//! v1's `get_table` may also answer `insertRow`, `updateRow` and `deleteRows`,
//! and whether it does is a property of the **configuration** rather than of the
//! provider — `@saltcorn/postgres-tables` omits all three when its `read_only`
//! flag is set. So [`writes`](TableProviderHost::writes) asks the module,
//! `get_table(cfg)` in hand, and the three write methods route exactly as the
//! read ones do.
//!
//! The narrowing from this system's `Statement` into those three signatures is
//! `sc_catalog`'s, not this crate's: what crosses here is already v1's
//! vocabulary — a record, an id, a `where` object.

use std::sync::Arc;

use async_trait::async_trait;
use sc_catalog::{DataField, ProvidedWrites, TableProviderHost, TableProviderKind};
use sc_error::{Error, Result};
use sc_types::{BasicType, TypeRef};
use serde_json::Value as Json;

use crate::host::ModuleHost;
use crate::modules::ModuleSet;
use crate::spec::config_fields_to_form_fields;

/// The table providers this server supplies, over the pool they run on.
pub struct ModuleTableProviders {
    host: Arc<ModuleHost>,
    providers: Vec<TableProviderKind>,
}

impl ModuleTableProviders {
    /// Every provider of every loaded module in `set`, over `host`.
    ///
    /// A module that would not load supplies none: it has no manifest, and a
    /// provider nobody can call is not one to offer in the "new table" form.
    pub fn new(host: &Arc<ModuleHost>, set: &ModuleSet) -> ModuleTableProviders {
        let mut providers = Vec::new();
        for loaded in set.modules() {
            let Some(manifest) = &loaded.manifest else {
                continue;
            };
            for provider in &manifest.table_providers {
                // The issues from translating the fields are dropped here rather
                // than collected, because they are already the module's: the
                // loader records them on the module's card (see `modules.rs`),
                // and reporting them a second time on every form render would
                // put a module's problem in front of an admin creating an
                // unrelated table.
                let (config_spec, _) = config_fields_to_form_fields(
                    &provider.config_fields,
                    &format!("the table provider `{}`", provider.name),
                );
                providers.push(TableProviderKind {
                    module: loaded.module.name.clone(),
                    provider: provider.name.clone(),
                    config_spec,
                });
            }
        }
        ModuleTableProviders {
            host: Arc::clone(host),
            providers,
        }
    }

    /// An empty set — a server with no modules, and the starting point for a
    /// test that wants the seam without a worker.
    pub fn empty(host: &Arc<ModuleHost>) -> ModuleTableProviders {
        ModuleTableProviders {
            host: Arc::clone(host),
            providers: Vec::new(),
        }
    }

    /// Refuse a name this set does not have, before a worker is reached.
    ///
    /// Re-checked here even though the catalog resolved the table through this
    /// same list: a module can be deleted between a reload and a query, and the
    /// honest answer then is a sentence naming what went away rather than a
    /// worker's "not loaded in this host".
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
    pub fn providers(&self) -> &[TableProviderKind] {
        &self.providers
    }
}

#[async_trait]
impl TableProviderHost for ModuleTableProviders {
    fn providers(&self) -> Vec<TableProviderKind> {
        self.providers.clone()
    }

    async fn fields(&self, module: &str, provider: &str, config: &Json) -> Result<Vec<DataField>> {
        self.require(module, provider)?;
        let declared = self.host.provider_fields(module, provider, config).await?;
        Ok(declared.iter().filter_map(data_field).collect())
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
        self.require(module, provider)?;
        let answer = self
            .host
            .provider_writes(module, provider, config, table)
            .await?;
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
        self.require(module, provider)?;
        let answer = self
            .host
            .provider_insert(module, provider, config, table, record)
            .await?;
        // v1 lets `insertRow` answer nothing, so a missing key is `null` rather
        // than an error: the caller reads the row back through what it wrote.
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
        self.require(module, provider)?;
        self.host
            .provider_update(module, provider, config, table, id, record)
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
        self.require(module, provider)?;
        self.host
            .provider_delete(module, provider, config, table, filter)
            .await?;
        Ok(())
    }
}

/// One v1 field declaration as a [`DataField`].
///
/// v1's vocabulary is the same one [`crate::spec`] already translates for a
/// setting — `{ name, label, type, primary_key, required }` — and this is the
/// same mapping applied to a *column* rather than to a form control. A field
/// with no usable name is dropped: it has no column to be.
///
/// **A type this version does not know becomes text**, on `spec`'s grounds: a
/// provider whose fifth column is a rich type nobody registered should not cost
/// the admin the other four, and text is what a JSON value is until something
/// types it. A v1 `Key to books` is text as well for now — a foreign key out of
/// a provided table is a join, and §3 of this milestone says a provided table
/// cannot be joined.
fn data_field(declared: &Json) -> Option<DataField> {
    let name = declared
        .get("name")
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())?;
    let type_name = declared
        .get("type")
        .and_then(Json::as_str)
        .unwrap_or("String")
        .trim();
    let mut field = DataField::plain(name, TypeRef::Basic(basic_type(type_name)));
    field = field.label(
        declared
            .get("label")
            .and_then(Json::as_str)
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .unwrap_or(name),
    );
    if truthy(declared.get("primary_key")) {
        field = field.primary_key();
    }
    if truthy(declared.get("required")) {
        field = field.required();
    }
    if truthy(declared.get("is_unique")) {
        field = field.unique();
    }
    Some(field)
}

/// v1's column type names, and what they are here. The same table
/// [`crate::spec`] keeps for a setting, minus the form-only spellings.
fn basic_type(declared: &str) -> BasicType {
    match declared {
        "Integer" | "integer" | "Int" => BasicType::Int,
        "Float" | "float" | "Number" => BasicType::Float,
        "Bool" | "bool" | "Boolean" => BasicType::Bool,
        "Date" | "date" => BasicType::Date,
        "JSON" | "json" => BasicType::Json,
        _ => BasicType::Text,
    }
}

/// JavaScript truthiness for a v1 field's flags — a plugin writes
/// `primary_key: true`, and an older one writes `"on"`.
fn truthy(value: Option<&Json>) -> bool {
    match value {
        Some(Json::Bool(b)) => *b,
        Some(Json::String(s)) => !s.is_empty() && s != "false",
        Some(Json::Number(n)) => n.as_f64().unwrap_or(0.0) != 0.0,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn v1_field_declarations_become_columns() {
        let fields: Vec<DataField> = [
            json!({ "name": "id", "label": "ID", "type": "Integer", "primary_key": true }),
            json!({ "name": "title", "type": "String" }),
            json!({ "name": "at", "type": "Date" }),
            // A type nobody registered: text, not a refusal, and not a dropped
            // column.
            json!({ "name": "geo", "type": "PostGIS point" }),
            // Nothing that can be a column.
            json!({ "label": "nameless", "type": "String" }),
        ]
        .iter()
        .filter_map(data_field)
        .collect();

        assert_eq!(fields.len(), 4);
        assert_eq!(fields[0].base.name, "id");
        assert_eq!(fields[0].base.label, "ID");
        assert_eq!(fields[0].base.type_, TypeRef::Basic(BasicType::Int));
        assert!(fields[0].primary_key);
        // An unlabelled column is labelled by its name, as everywhere else.
        assert_eq!(fields[1].base.label, "title");
        assert_eq!(fields[2].base.type_, TypeRef::Basic(BasicType::Date));
        assert_eq!(fields[3].base.type_, TypeRef::Basic(BasicType::Text));
    }

    #[tokio::test]
    async fn a_provider_nothing_supplies_is_refused_before_a_worker_starts() {
        // No worker is built here — the pool is lazy — so this asserts exactly
        // what it says: the check happens on this side.
        let host = Arc::new(ModuleHost::new("/nonexistent/modules"));
        let providers = ModuleTableProviders::empty(&host);
        let err = providers
            .fields("@saltcorn/rss", "RSS feed", &json!({}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("RSS feed"), "{err}");
        assert!(err.contains("@saltcorn/rss"), "{err}");
        assert!(TableProviderHost::providers(&providers).is_empty());
    }
}
