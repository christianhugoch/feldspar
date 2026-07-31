//! The `_sc_llm_providers` table: its schema, bootstrap, and the
//! [`LlmProviderDef`] ⇄ row mapping (design §9, §11.1).
//!
//! Follows `_sc_file_stores` in every respect that matters, because it is the
//! same kind of thing: a provider, like a store and like an application, has
//! **nothing to introspect it from**, so its row *is* its definition rather than
//! an overlay on something the database already knows.
//!
//! **Reading is strict.** A column that is missing or of the wrong shape is an
//! [`Error::invalid`] naming the provider and the column, not a silently
//! defaulted field: a half-understood row is a misconfigured provider the admin
//! needs told about, and one that silently defaults its `base_url` would send an
//! API key to the wrong host.
//!
//! **The key is not encrypted at rest** (§11.1, and out of scope for this
//! milestone by decision): it sits in the primary database like every other
//! configuration value. Saying so is better than implying a protection a
//! database dump would disprove. What *is* guaranteed is that it does not leave
//! through the API — see [`sc_types::redact_attrs`] and the admin handlers.

use sc_catalog::{Catalog, DataField, Table};
use sc_db::Row;
use sc_error::{Error, Result};
use sc_query::{Assignment, Delete, Expr, Insert, Select, Source, Statement, Update, Value};
use sc_types::{Attrs, BasicType, TypeRef};
use serde_json::Value as Json;

use crate::def::{LlmProviderDef, LlmProviderDefId, validate_provider_config};

/// Name of the providers table in the primary database.
pub const LLM_PROVIDERS_TABLE: &str = "_sc_llm_providers";

/// The UUID primary-key column (§9).
pub const COL_ID: &str = "id";
/// The provider's name — what an agent references.
pub const COL_NAME: &str = "name";
/// The human-readable description column (§9).
pub const COL_DESCRIPTION: &str = "description";
/// The backend serving the provider (`anthropic`, `openai_responses`).
pub const COL_BACKEND: &str = "backend";
/// The backend's settings (JSON object), including the API key.
pub const COL_CONFIG: &str = "config";
/// The sparse per-provider values column (§9) — JSON, always an object.
pub const COL_ATTRIBUTES: &str = "attributes";

/// The fields of the `_sc_llm_providers` table, in declaration order.
///
/// `name` carries the `UNIQUE` constraint for the same reason a file store's
/// does: it is the key an agent resolves through, so two definitions claiming
/// one name is not a state the system can serve, and the database is the
/// authority because two admins saving concurrently cannot see each other's
/// transaction.
fn provider_fields() -> Vec<DataField> {
    let text = || TypeRef::Basic(BasicType::Text);
    let json = || TypeRef::Basic(BasicType::Json);
    let uuid = || TypeRef::Basic(BasicType::Uuid);
    vec![
        DataField::plain(COL_ID, uuid()).required().primary_key(),
        DataField::plain(COL_NAME, text()).required().unique(),
        DataField::plain(COL_DESCRIPTION, text()),
        DataField::plain(COL_BACKEND, text()).required(),
        DataField::plain(COL_CONFIG, json()).required(),
        DataField::plain(COL_ATTRIBUTES, json()).required(),
    ]
}

/// Ensure the `_sc_llm_providers` table exists, creating it if absent.
///
/// Idempotent and safe against a database that has never seen Saltcorn — the
/// same contract `bootstrap_file_stores` has. Call once at startup, after the
/// [`Catalog`] is initialised.
pub async fn bootstrap_llm_providers(catalog: &Catalog) -> Result<Table> {
    catalog
        .bootstrap_table(LLM_PROVIDERS_TABLE, &provider_fields())
        .await
}

/// Save a provider definition: insert its row, or update it in place if a row
/// with its [`LlmProviderDefId`] already exists.
///
/// Settings are checked against the backend's spec first — a missing key is the
/// admin's to fix and the admin is standing in front of the form, whereas the
/// same mistake found at chat time is an agent that fails in a transcript.
///
/// The name is unique, and a clash is reported as an [`Error::invalid`] naming
/// the conflict rather than a raw constraint violation. The database's `UNIQUE`
/// constraint remains the authority: this check and the write are not one
/// transaction.
pub async fn save_llm_provider(catalog: &Catalog, def: &LlmProviderDef) -> Result<()> {
    check_provider_saveable(catalog, def).await?;

    let columns = provider_columns();
    let values = provider_values(def);

    if load_llm_provider(catalog, def.id).await?.is_some() {
        let assignments = columns
            .iter()
            .zip(values)
            // The id is the row's identity, not something to reassign.
            .filter(|(col, _)| *col != COL_ID)
            .map(|(col, value)| Assignment::new(col.clone(), Expr::Lit(value)))
            .collect();
        let update = Update::new(LLM_PROVIDERS_TABLE, assignments)
            .filter(Expr::col(COL_ID).eq(Expr::lit(def.id.0)));
        run(catalog, Statement::from(update)).await?;
    } else {
        let insert = Insert::row(
            LLM_PROVIDERS_TABLE,
            columns,
            values.into_iter().map(Expr::Lit).collect(),
        );
        run(catalog, Statement::from(insert)).await?;
    }
    Ok(())
}

/// Everything [`save_llm_provider`] checks before it writes: a name, a backend,
/// settings matching that backend's spec, and no other provider holding the
/// name.
pub async fn check_provider_saveable(catalog: &Catalog, def: &LlmProviderDef) -> Result<()> {
    let name = def.name.trim();
    if name.is_empty() {
        return Err(Error::invalid("an LLM provider needs a name"));
    }
    if def.backend.trim().is_empty() {
        return Err(Error::invalid(format!(
            "LLM provider `{name}` needs a backend"
        )));
    }
    validate_provider_config(def)?;

    if let Some(other) = load_llm_provider_by_name(catalog, name).await?
        && other.id != def.id
    {
        return Err(Error::invalid(format!(
            "LLM provider name `{name}` is already used; \
             each provider is referenced by its own name"
        )));
    }
    Ok(())
}

/// Load the definition with this id, if it exists.
pub async fn load_llm_provider(
    catalog: &Catalog,
    id: LlmProviderDefId,
) -> Result<Option<LlmProviderDef>> {
    load_one(catalog, Expr::col(COL_ID).eq(Expr::lit(id.0))).await
}

/// Load the definition named `name`, if any — the lookup an agent's `provider`
/// resolves through.
pub async fn load_llm_provider_by_name(
    catalog: &Catalog,
    name: &str,
) -> Result<Option<LlmProviderDef>> {
    load_one(catalog, Expr::col(COL_NAME).eq(Expr::lit(name))).await
}

/// The definition named `name`, or an error saying it does not exist — what an
/// agent's validation calls, so a provider that was deleted names itself in the
/// reason the agent is invalid (§11.2).
pub async fn require_llm_provider(catalog: &Catalog, name: &str) -> Result<LlmProviderDef> {
    load_llm_provider_by_name(catalog, name)
        .await?
        .ok_or_else(|| Error::not_found(format!("no LLM provider named `{name}`")))
}

/// Every stored definition, ordered by name — what the admin UI lists.
pub async fn list_llm_providers(catalog: &Catalog) -> Result<Vec<LlmProviderDef>> {
    let select = Select::from(Source::table(LLM_PROVIDERS_TABLE));
    let mut defs: Vec<LlmProviderDef> = rows(catalog, select)
        .await?
        .iter()
        .map(provider_from_row)
        .collect::<Result<_>>()?;
    defs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(defs)
}

/// Delete a provider definition, returning whether one was there to delete.
///
/// `extra_referents` is how references this crate cannot see are supplied: an
/// **agent** names a provider, and `_sc_agents` lives in `sc-agent`, a layer
/// above this one. Callers that know about agents collect those first and pass
/// them in; a caller passing an empty slice gets no reference check at all,
/// which is correct for a system with no agents and wrong for one with them.
/// The same arrangement `delete_file_store` uses for applications, and for the
/// same layering reason.
pub async fn delete_llm_provider(
    catalog: &Catalog,
    id: LlmProviderDefId,
    extra_referents: &[String],
) -> Result<bool> {
    let Some(def) = load_llm_provider(catalog, id).await? else {
        return Ok(false);
    };

    if !extra_referents.is_empty() {
        return Err(Error::invalid(format!(
            "LLM provider `{}` is still used by {}; \
             remove those references before deleting it",
            def.name,
            extra_referents.join(", ")
        )));
    }

    let delete =
        Delete::from(LLM_PROVIDERS_TABLE).filter(Expr::col(COL_ID).eq(Expr::lit(def.id.0)));
    run(catalog, Statement::from(delete)).await?;
    Ok(true)
}

/// The row's columns, in the order [`provider_values`] produces them.
fn provider_columns() -> Vec<String> {
    [
        COL_ID,
        COL_NAME,
        COL_DESCRIPTION,
        COL_BACKEND,
        COL_CONFIG,
        COL_ATTRIBUTES,
    ]
    .iter()
    .map(|c| (*c).to_owned())
    .collect()
}

/// The definition serialised to its row's values, in [`provider_columns`] order.
fn provider_values(def: &LlmProviderDef) -> Vec<Value> {
    vec![
        Value::Uuid(def.id.0),
        Value::Text(def.name.trim().to_owned()),
        Value::Text(def.description.clone()),
        Value::Text(def.backend.trim().to_owned()),
        Value::Json(Json::Object(def.config.clone())),
        Value::Json(Json::Object(def.attributes.clone())),
    ]
}

/// Rebuild an [`LlmProviderDef`] from its row. The strictness note in the module
/// docs applies throughout.
fn provider_from_row(row: &Row) -> Result<LlmProviderDef> {
    let id = match row.get(COL_ID) {
        Some(Value::Uuid(u)) => LlmProviderDefId(*u),
        other => return Err(bad_column(COL_ID, "a uuid", other)),
    };

    // A NULL description is "none given", not a broken row.
    let description = match row.get(COL_DESCRIPTION) {
        Some(Value::Text(t)) => t.clone(),
        Some(Value::Null) | None => String::new(),
        other => return Err(bad_column(COL_DESCRIPTION, "text", other)),
    };

    Ok(LlmProviderDef {
        id,
        name: text(row, COL_NAME)?,
        description,
        backend: text(row, COL_BACKEND)?,
        config: object(row, COL_CONFIG)?,
        attributes: object(row, COL_ATTRIBUTES)?,
    })
}

/// A required text column.
fn text(row: &Row, column: &str) -> Result<String> {
    match row.get(column) {
        Some(Value::Text(t)) => Ok(t.clone()),
        other => Err(bad_column(column, "text", other)),
    }
}

/// A JSON column that must hold an object.
fn object(row: &Row, column: &str) -> Result<Attrs> {
    match row.get(column) {
        Some(Value::Json(Json::Object(o))) => Ok(o.clone()),
        Some(Value::Json(_)) => Err(Error::invalid(format!(
            "{LLM_PROVIDERS_TABLE}.{column} should be a json object"
        ))),
        other => Err(bad_column(column, "json", other)),
    }
}

fn bad_column(column: &str, expected: &str, got: Option<&Value>) -> Error {
    match got {
        Some(value) => Error::invalid(format!(
            "{LLM_PROVIDERS_TABLE}.{column} should be {expected}, got {}",
            value.kind()
        )),
        None => Error::invalid(format!("row has no `{column}` column")),
    }
}

/// Run a statement that returns no rows of interest.
async fn run(catalog: &Catalog, statement: Statement) -> Result<()> {
    catalog
        .primary()
        .query(&statement)
        .await?
        .try_collect()
        .await?;
    Ok(())
}

/// Run a select and collect its rows.
async fn rows(catalog: &Catalog, select: Select) -> Result<Vec<Row>> {
    catalog
        .primary()
        .query(&Statement::from(select))
        .await?
        .try_collect()
        .await
}

/// Load the single definition matching `filter`, if any.
async fn load_one(catalog: &Catalog, filter: Expr) -> Result<Option<LlmProviderDef>> {
    let select = Select::from(Source::table(LLM_PROVIDERS_TABLE))
        .filter(filter)
        .limit(1);
    match rows(catalog, select).await?.first() {
        Some(row) => Ok(Some(provider_from_row(row)?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::def::ANTHROPIC_BACKEND;

    #[test]
    fn schema_has_the_section_9_required_columns() {
        let fields = provider_fields();
        let by_name = |n: &str| fields.iter().find(|f| f.base.name == n).unwrap();

        let id = by_name(COL_ID);
        assert!(id.primary_key && id.required);
        assert_eq!(id.base.type_, TypeRef::Basic(BasicType::Uuid));
        assert!(by_name(COL_NAME).required && by_name(COL_NAME).unique);
        assert_eq!(
            by_name(COL_ATTRIBUTES).base.type_,
            TypeRef::Basic(BasicType::Json)
        );
        assert!(!by_name(COL_DESCRIPTION).required);
    }

    #[test]
    fn the_table_is_a_hidden_system_table() {
        assert!(LLM_PROVIDERS_TABLE.starts_with("_sc_"));
    }

    #[test]
    fn columns_and_values_stay_in_step() {
        // The insert pairs these two positionally, so a column added to one and
        // not the other would write an API key into the wrong column.
        let def = LlmProviderDef::anthropic("main", "sk-x", "claude-opus-4");
        assert_eq!(provider_columns().len(), provider_values(&def).len());
        assert_eq!(provider_columns().len(), provider_fields().len());
    }

    #[test]
    fn a_definitions_config_reaches_the_row_whole() {
        let def = LlmProviderDef::anthropic("main", "sk-x", "claude-opus-4");
        let idx = provider_columns()
            .iter()
            .position(|c| c == COL_CONFIG)
            .unwrap();
        let Value::Json(Json::Object(config)) = &provider_values(&def)[idx] else {
            panic!("config should be a json object");
        };
        assert_eq!(config.get("api_key"), Some(&Json::from("sk-x")));
        assert_eq!(
            provider_values(&def)[provider_columns()
                .iter()
                .position(|c| c == COL_BACKEND)
                .unwrap()],
            Value::Text(ANTHROPIC_BACKEND.to_owned())
        );
    }
}
