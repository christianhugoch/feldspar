//! The concrete admin-API handlers (technical design §13.1, §7).
//!
//! [`admin_handlers`] builds the [`HandlerRegistry`] that the admin
//! [`EndpointSet`](sc_api::admin_endpoints) dispatches against: one entry per
//! endpoint `name`, each closing over a shared [`Catalog`]. The handlers are the
//! server-side half of the typed API — bootstrap/auth, the table & field catalog,
//! row CRUD, and user management — and they stay free of HTTP plumbing: they read
//! a [`HandlerCtx`] and return a [`HandlerResponse`], deferring cookies/sessions
//! to the dispatcher via [`SessionAction`](crate::SessionAction).
//!
//! Data crosses the wire as plain JSON. The row endpoints are thin wrappers over
//! [`sc_api::rows`], the shared row-CRUD layer an application's REST API runs
//! too — the admin API is not a privileged special case, it is the same
//! machinery (design §13.1). A row is addressed by its single-column primary
//! key; `createTable` invents none (GOALS), so a new table has no key until a
//! field says it is one, and until then the row endpoints that address a single
//! row refuse — which is what the field list's red banner is warning about.

use std::collections::BTreeMap;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use sc_action::{
    ATTR_DAY_OF_WEEK, ATTR_HOUR, ATTR_MINUTE, EventKind, Trigger, TriggerBody, TriggerId,
    delete_trigger, list_triggers, load_trigger, save_trigger,
};
use sc_api::auth::{credentials, user_row_json, user_summary_json};
use sc_api::csv as csv_rows;
use sc_api::rows::{self, require_object};
use sc_api::schema_edit;
use sc_api::{ApiRequest, GRAPHQL_PROVIDER, Method as ApiMethod};
use sc_app::{
    ApiConfig, AppId, Application, CspPolicy, FrameworkRef, StaticDir, TriggerRef,
    app_source_from_config, applications_using_file_store, builder_agent_name, delete_application,
    framework_builder_agent, framework_config_spec, framework_default_csp, list_applications,
    load_application, registered_api_provider_info, registered_framework_info,
    require_scaffoldable, save_application, scaffold_app, update_app_client,
};
use sc_auth::{
    COL_EMAIL, NewUser, ROLE_ADMIN, ROLE_PUBLIC, Role, USERS_TABLE, User, UserUpdate,
    any_user_exists, authenticate_admin, create_first_user, create_user_with, delete_role,
    delete_user, list_roles, load_user, random_password, save_role, set_user_disabled,
    set_user_password, update_user,
};
use sc_catalog::{
    ATTR_OWNERSHIP_FORMULA, Attrs, Catalog, ConstraintKind, DataField, DataFieldKind,
    DbConnectionDef, DbConnectionId, FIELD_META_TABLE, FieldId, FieldMeta, FileStoreId, Table,
    TableConstraint, TableId, check_db_connection_saveable, check_file_store_saveable,
    connect_db_connection, connect_file_store_def, delete_db_connection, delete_file_store,
    file_kind_config_spec, key_kind_config_spec, list_db_connections, list_field_meta_for_table,
    list_file_stores, load_db_connection, load_db_connection_by_name, load_file_store,
    load_file_store_by_name, orphan_table_meta, resolve_options, save_db_connection,
    save_file_store,
};
use sc_email::Mailer;
use sc_error::{Error, Result};
use sc_files::{
    Entry, FileMeta, FileStoreDef, FileStoreDefId, backend_config_spec, backend_operations,
    check_access, display_config, effective_min_role, filter_visible, registered_backends,
    run_backend_operation,
};
use sc_llm::{
    LlmProviderDef, LlmProviderDefId, LlmRequest, connect_provider, delete_llm_provider,
    list_llm_providers, load_llm_provider, provider_config_spec, save_llm_provider,
};
use sc_module::{load_module, load_module_by_name, save_module};
use sc_query::{Expr, OrderBy, Projection, Select, Source, Statement, Value};
use sc_types::{
    FormField, Operation, OperationScope, SECRET_SENTINEL, registered_rich_types,
    rich_type_config_spec,
};
use serde_json::{Map, Value as Json, json};

use crate::apps::{AppMounts, build_and_mount};
use crate::handler::{HandlerRegistry, HandlerResponse};

/// The `text/plain` half of the Email tab's test message.
///
/// It says what it is and where it came from, because the person who receives it
/// may not be the person who pressed the button — a shared `postmaster@` address
/// is a common answer to "who do I send this to" — and an unexplained email from
/// a machine is a support ticket.
const TEST_EMAIL_TEXT: &str = "This is a test message from Saltcorn.\n\nIf you received it, the \
                               SMTP settings saved under Settings \u{2192} Email work.";

/// The `text/html` half. The same sentences: a test message is not the place to
/// find out whether the two bodies say different things.
const TEST_EMAIL_HTML: &str = "<p>This is a test message from Saltcorn.</p><p>If you received \
                               it, the SMTP settings saved under Settings \u{2192} Email \
                               work.</p>";

/// Build the registry of admin handlers over a shared [`Catalog`] and the live
/// [`AppMounts`] registry.
///
/// The names match the [`Endpoint`](sc_api::Endpoint) `name`s in
/// [`admin_endpoints`](sc_api::admin_endpoints); the router resolves each
/// endpoint's [`HandlerRef::Named`](sc_api::HandlerRef) here at dispatch time.
///
/// `apps` is the **same** live handle the router resolves requests against, so
/// the application `build`/`delete` handlers mount and unmount apps that start
/// serving (or stop) with no restart (§13.2). For an admin-only server pass an
/// [`AppMounts`] with the catalog attached ([`AppMounts::new`]); the application
/// CRUD handlers still work, and a build simply mounts an app no subdomain routes
/// to until a `--base-domain` is configured.
pub fn admin_handlers(catalog: Arc<Catalog>, apps: Arc<AppMounts>) -> HandlerRegistry {
    // A schema change re-projects the API providers of every mounted app
    // exposing that table, live (Phase 7's seam). This used to be a
    // `refresh_table` call inside each handler that changed a schema, which
    // worked only while an HTTP request was the only way to change one; an agent
    // can now change one too (§11.3), and `sc-api`'s schema editor — where the
    // change is actually made — cannot name this crate. So the catalog carries
    // the seam and the observer is installed **here**, at the one place a server
    // has both handles: a caller that assembles the admin API gets live
    // re-projection by construction rather than by remembering to ask for it.
    catalog.set_schema_observer(Arc::clone(&apps) as Arc<dyn sc_catalog::SchemaObserver>);
    // And the same for the trigger set, installed on the dispatcher rather than
    // the catalog because that is what holds it: a saved, renamed or deleted
    // trigger re-projects every app exposing one, whether the save came from
    // these handlers or from an agent's `admin_copilot` (§11.3). The handlers'
    // own `reproject_apps` calls are gone rather than double-firing beside it.
    if let Ok(dispatcher) = triggers_of(&apps) {
        dispatcher.set_observer(Arc::clone(&apps) as Arc<dyn sc_action::TriggerObserver>);
    }

    let mut reg = HandlerRegistry::new();

    // --- bootstrap & auth ---------------------------------------------------

    reg.register("authStatus", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let exists = any_user_exists(&catalog).await?;
                let current = ctx.user.as_ref().map_or(Json::Null, user_summary_json);
                Ok(HandlerResponse::ok(json!({
                    "any_user_exists": exists,
                    "current_user": current,
                })))
            }
        }
    });

    reg.register("createFirstUser", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let (email, password) = credentials(&ctx.body)?;
                let user = create_first_user(&catalog, &email, &password).await?;
                // Log the new admin straight in — they came from the bootstrap
                // screen and expect to land in the app.
                let body = user_summary_json(&user);
                Ok(HandlerResponse::start_session(user, body))
            }
        }
    });

    reg.register("login", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let (email, password) = credentials(&ctx.body)?;
                match authenticate_admin(&catalog, &email, &password).await? {
                    Some(user) => {
                        let body = user_summary_json(&user);
                        Ok(HandlerResponse::start_session(user, body))
                    }
                    None => Err(Error::auth("invalid credentials")),
                }
            }
        }
    });

    reg.register("logout", |_ctx| async {
        Ok(HandlerResponse::end_session(json!({ "ok": true })))
    });

    // --- catalog: tables & fields ------------------------------------------

    reg.register("listTables", {
        let catalog = catalog.clone();
        move |_ctx| {
            let catalog = catalog.clone();
            async move {
                let rls = catalog.primary().capabilities().row_level_security;
                let tables = catalog.tables()?;
                let out: Vec<Json> = tables
                    .iter()
                    .filter(|t| !t.is_system())
                    .map(|t| table_json(&catalog, t, rls))
                    .collect();
                Ok(HandlerResponse::ok(Json::Array(out)))
            }
        }
    });

    reg.register("createTable", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let obj = require_object(&ctx.body)?;
                let name = non_empty_str_field(obj, "name")?.trim().to_owned();
                let database = optional_str(obj, "database");
                // One operation through the shared schema editor (§3.3): the
                // identifier check and the live re-projection are its, so the
                // admin API and an agent create a table the same way. With no
                // fields, this makes an empty table with **no primary key** —
                // the admin adds the key as a field, like any other (GOALS).
                schema_edit::apply(
                    &catalog,
                    &[schema_edit::Operation::CreateTable {
                        name: name.clone(),
                        database,
                        settings: schema_edit::TableSettings::default(),
                        fields: Vec::new(),
                    }],
                    &schema_edit::ApplyOptions::default(),
                )
                .await?;
                let rls = catalog.primary().capabilities().row_level_security;
                Ok(
                    HandlerResponse::ok(table_json(&catalog, &catalog.require(&name)?, rls))
                        .with_status(201),
                )
            }
        }
    });

    reg.register("createTableFromCsv", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let obj = require_object(&ctx.body)?;
                let name = non_empty_str_field(obj, "name")?.trim().to_owned();
                let document = obj
                    .get("csv")
                    .and_then(Json::as_str)
                    .ok_or_else(|| Error::invalid("`csv` must be the CSV document as text"))?
                    .to_owned();
                // The fields, the table and the rows are all `sc-api`'s: this
                // endpoint chooses nothing the CLI or an agent doing the same
                // thing would choose differently. A file the rows will not go
                // into leaves no table behind, which is why the failure here is
                // an ordinary error and not a half-made table plus a warning.
                let (table, outcome) = csv_rows::create_table_from_csv(
                    &catalog,
                    &name,
                    &optional_str(obj, "database"),
                    &document,
                    Some(&admin_caller(ctx.user.as_ref())),
                )
                .await?;
                let rls = catalog.primary().capabilities().row_level_security;
                Ok(HandlerResponse::ok(json!({
                    "table": table_json(&catalog, &table, rls),
                    "inserted": outcome.inserted,
                }))
                .with_status(201))
            }
        }
    });

    reg.register("dropTable", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                // Deliberately **not** `deleteTableSettings`: that forgets a
                // configuration, this destroys the table, its columns and every
                // row in it. Both delete the overlay row; only this one issues
                // DDL, and only this one refuses by name what another table's
                // key still points at.
                let table = ctx.path_param("table")?.to_owned();
                // A **provided** table (§8.3) is deleted, not dropped: there is
                // no table in any database to issue DDL against, and its
                // `_sc_tables` row is its whole definition. One verb in the UI,
                // because "delete this table" is one thing an admin means.
                if catalog.get(&table)?.is_some_and(|t| t.provider().is_some()) {
                    sc_api::provided_tables::forget(&catalog, &table).await?;
                    return Ok(HandlerResponse::ok(json!({ "dropped": table })));
                }
                schema_edit::apply(
                    &catalog,
                    &[schema_edit::Operation::DropTable {
                        table: table.clone(),
                    }],
                    &schema_edit::ApplyOptions::default(),
                )
                .await?;
                Ok(HandlerResponse::ok(json!({ "dropped": table })))
            }
        }
    });

    reg.register("updateTable", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                // The table must exist: this configures a table the admin is
                // looking at. A row for a table that is *not* there is an
                // orphan, which is cleaned up rather than edited — see
                // `deleteTableSettings`.
                let table = catalog.require(ctx.path_param("table")?)?;
                let obj = require_object(&ctx.body)?;

                // **Every** setting is passed, which is this endpoint's
                // whole-object contract (§13.1) rather than the schema editor's
                // omitted-means-leave default: the admin UI edits a table it has
                // loaded, so an omitted value here means the admin cleared it.
                // `alter_table` in an agent's batch is the other case, and the
                // difference is spelled at exactly these two call sites.
                let settings = table_settings_from_body(obj)?;
                schema_edit::apply(
                    &catalog,
                    &[schema_edit::Operation::AlterTable {
                        table: table.name.clone(),
                        settings,
                    }],
                    &schema_edit::ApplyOptions::default(),
                )
                .await?;
                let rls = catalog.primary().capabilities().row_level_security;
                Ok(HandlerResponse::ok(table_json(
                    &catalog,
                    &catalog.require(&table.name)?,
                    rls,
                )))
            }
        }
    });

    // --- table providers (§8.3) ---------------------------------------------

    reg.register("listTableProviders", {
        let catalog = catalog.clone();
        move |_ctx| {
            let catalog = catalog.clone();
            async move {
                // A property of the **installed modules**, not of any table, and
                // the empty list is the ordinary answer: a server with no
                // modules supplies no providers, and the New table dialog offers
                // the option only when there is one.
                let out: Vec<Json> = catalog
                    .table_provider_kinds()
                    .iter()
                    .map(|kind| {
                        json!({
                            "module": kind.module,
                            "provider": kind.provider,
                            "config_spec": kind.config_spec
                                .iter()
                                .map(form_field_json)
                                .collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                Ok(HandlerResponse::ok(Json::Array(out)))
            }
        }
    });

    reg.register("createProvidedTable", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let obj = require_object(&ctx.body)?;
                let name = non_empty_str_field(obj, "name")?.trim().to_owned();
                let module = non_empty_str_field(obj, "module")?.to_owned();
                let provider = non_empty_str_field(obj, "provider")?.to_owned();
                // No DDL: this writes the definition row and reloads, which is
                // what makes the table exist and what asks the module for its
                // columns.
                let table = sc_api::provided_tables::create(
                    &catalog,
                    &name,
                    &module,
                    &provider,
                    object_field(obj, "configuration")?,
                )
                .await?;
                let rls = catalog.primary().capabilities().row_level_security;
                Ok(HandlerResponse::ok(table_json(&catalog, &table, rls)).with_status(201))
            }
        }
    });

    reg.register("updateProvidedTable", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let name = ctx.path_param("table")?.to_owned();
                let table = catalog.require(&name)?;
                let obj = require_object(&ctx.body)?;
                let submitted = object_field(obj, "configuration")?;
                // A provider's `password` setting is a secret like any other:
                // the form was shown the sentinel, and submitting it back
                // unchanged keeps what is stored — the same merge a module's own
                // settings go through, against the spec the provider declared.
                let spec = table
                    .provider()
                    .and_then(|(module, provider)| {
                        catalog
                            .table_provider_kinds()
                            .into_iter()
                            .find(|k| k.module == module && k.provider == provider)
                    })
                    .map(|kind| kind.config_spec)
                    .unwrap_or_default();
                let stored = match table.attributes.get(sc_catalog::ATTR_PROVIDER_CONFIG) {
                    Some(Json::Object(map)) => map.clone(),
                    _ => sc_types::Attrs::new(),
                };
                let configuration = sc_types::merge_secrets(&spec, &stored, &submitted);
                let table =
                    sc_api::provided_tables::configure(&catalog, &name, configuration).await?;
                let rls = catalog.primary().capabilities().row_level_security;
                Ok(HandlerResponse::ok(table_json(&catalog, &table, rls)))
            }
        }
    });

    reg.register("deleteTableSettings", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                // Addressed by name and looked up in the *rows*, not the
                // catalog: this is also how an orphan is cleaned up, and an
                // orphan by definition has no table to look up. Dropping the
                // policies a forgotten `rls_enabled` had turned on is the schema
                // editor's rule, not this handler's.
                let name = ctx.path_param("table")?.to_owned();
                let deleted = schema_edit::forget_table_settings(&catalog, &name).await?;
                Ok(HandlerResponse::ok(json!({ "deleted": deleted })))
            }
        }
    });

    reg.register("listOrphanTableSettings", {
        let catalog = catalog.clone();
        move |_ctx| {
            let catalog = catalog.clone();
            async move {
                let out: Vec<Json> = orphan_table_meta(&catalog)
                    .await?
                    .iter()
                    .map(|meta| {
                        json!({
                            "name": meta.table_name,
                            "label": meta.label,
                            "description": meta.description,
                            "min_role_read": meta.access.min_role_read,
                            "min_role_write": meta.access.min_role_write,
                            "ownership_formula": meta.ownership_formula().unwrap_or(""),
                            "rls_enabled": meta.rls_enabled(),
                        })
                    })
                    .collect();
                Ok(HandlerResponse::ok(Json::Array(out)))
            }
        }
    });

    reg.register("listRoles", {
        let catalog = catalog.clone();
        move |_ctx| {
            let catalog = catalog.clone();
            async move {
                let out: Vec<Json> = list_roles(&catalog).await?.iter().map(role_json).collect();
                Ok(HandlerResponse::ok(Json::Array(out)))
            }
        }
    });

    reg.register("createRole", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let obj = require_object(&ctx.body)?;
                let number = role_field(obj, "role")?;
                let mut role = Role::new(number, non_empty_str_field(obj, "name")?.trim());
                role.description = str_field(obj, "description")?.trim().to_owned();
                save_role(&catalog, &role).await?;
                Ok(HandlerResponse::ok(role_json(&role)).with_status(201))
            }
        }
    });

    reg.register("deleteRole", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let raw = ctx.path_param("role")?;
                let number = raw
                    .parse::<u8>()
                    .ok()
                    .filter(|r| (ROLE_ADMIN..=ROLE_PUBLIC).contains(r))
                    .ok_or_else(|| {
                        Error::invalid(format!("`{raw}` is not a role between 1 and 100"))
                    })?;
                let deleted = delete_role(&catalog, number).await?;
                Ok(HandlerResponse::ok(json!({ "deleted": deleted })))
            }
        }
    });

    reg.register("listFields", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let table = catalog.require(ctx.path_param("table")?)?;
                // Descriptions live only in the overlay row, not the merged field,
                // so they are looked up alongside — but only when the overlay
                // table exists at all (a database with no `_sc_fields` behaves as
                // before the overlay, §9).
                let metas = if catalog.get(FIELD_META_TABLE)?.is_some() {
                    list_field_meta_for_table(&catalog, &table.name).await?
                } else {
                    Vec::new()
                };
                let out: Vec<Json> = table
                    .fields
                    .iter()
                    .map(|f| field_json(f, &description_of(&metas, &f.base.name)))
                    .collect();
                Ok(HandlerResponse::ok(Json::Array(out)))
            }
        }
    });

    reg.register("createField", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let table_name = ctx.path_param("table")?.to_owned();
                let obj = require_object(&ctx.body)?;
                let description = optional_str(obj, "description");
                // The wire shape is parsed by `field_spec_from_body`; what a field *means* — the
                // storage type behind a rich type, the foreign key behind a
                // `Key`, the calculated-field check, the DDL-then-overlay
                // sequence and the live re-projection — is the schema editor's
                // (§3.3), so an agent's `add_field` and this endpoint cannot
                // drift.
                //
                // `type` is read as optional rather than required because a
                // `Key`'s storage type is its target's, and the schema editor is
                // the one that knows that — it fills the type in for a reference
                // and names the omission for anything else. Requiring it here
                // would make the admin UI ask for an answer it cannot know and
                // the database would refuse.
                let field = field_spec_from_body(obj)?;
                let name = field.name.trim().to_owned();
                schema_edit::apply(
                    &catalog,
                    &[schema_edit::Operation::AddField {
                        table: table_name.clone(),
                        field,
                    }],
                    &schema_edit::ApplyOptions::default(),
                )
                .await?;

                let table = catalog.require(&table_name)?;
                let created = table
                    .field(&name)
                    .ok_or_else(|| Error::msg(format!("field `{name}` missing after create")))?;
                Ok(HandlerResponse::ok(field_json(created, &description)).with_status(201))
            }
        }
    });

    reg.register("updateField", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                // The field must exist: this edits a field the admin is looking
                // at. Overlay-only — nothing here touches the column itself.
                let table_name = ctx.path_param("table")?.to_owned();
                let field_name = ctx.path_param("field")?.to_owned();
                let obj = require_object(&ctx.body)?;
                let description = optional_str(obj, "description").trim().to_owned();
                // Whole-object, like `updateTable`: the admin UI edits a field it
                // has loaded, so an omitted value means cleared.
                let settings = schema_edit::FieldSettings {
                    label: Some(optional_str(obj, "label")),
                    description: Some(description.clone()),
                    type_name: Some(optional_str(obj, "type")),
                    kind: Some(parse_field_kind(obj)?),
                    attributes: Some(attributes_field(obj)?),
                    // Unlike the rest of this body, **omitted means leave it**:
                    // a form that did not ask about the key must not be able to
                    // drop one, and the field editor sends it only when the
                    // admin ticked or unticked the box.
                    primary_key: obj
                        .get("primary_key")
                        .filter(|v| !v.is_null())
                        .and_then(Json::as_bool),
                };
                schema_edit::apply(
                    &catalog,
                    &[schema_edit::Operation::AlterField {
                        table: table_name.clone(),
                        field: field_name.clone(),
                        settings,
                    }],
                    &schema_edit::ApplyOptions::default(),
                )
                .await?;
                let table = catalog.require(&table_name)?;
                let updated = table.field(&field_name).ok_or_else(|| {
                    Error::msg(format!("field `{field_name}` missing after update"))
                })?;
                Ok(HandlerResponse::ok(field_json(updated, &description)))
            }
        }
    });

    reg.register("deleteField", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                // Drops the column **and its data**, plus the overlay row that
                // described it — an overlay left behind would be
                // indistinguishable from §1.1's deliberately-kept orphan. What
                // the database would refuse with a foreign-key error is refused
                // here by name first.
                let table = ctx.path_param("table")?.to_owned();
                let field = ctx.path_param("field")?.to_owned();
                schema_edit::apply(
                    &catalog,
                    &[schema_edit::Operation::DropField {
                        table: table.clone(),
                        field: field.clone(),
                    }],
                    &schema_edit::ApplyOptions::default(),
                )
                .await?;
                Ok(HandlerResponse::ok(json!({ "dropped": field })))
            }
        }
    });

    // --- table constraints (§5) ---------------------------------------------

    reg.register("listConstraints", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                // Read straight off the merged table, which is to say straight
                // off the database: a constraint is not stored anywhere else, so
                // there is nothing here to look up beside it (§5).
                let table = catalog.require(ctx.path_param("table")?)?;
                let out: Vec<Json> = table.constraints.iter().map(constraint_json).collect();
                Ok(HandlerResponse::ok(Json::Array(out)))
            }
        }
    });

    reg.register("createConstraint", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let table_name = ctx.path_param("table")?.to_owned();
                let obj = require_object(&ctx.body)?;
                let (given_name, constraint) = constraint_from_body(obj)?;
                // Through the schema editor like every other schema change, so
                // an agent adding a constraint and this endpoint cannot drift —
                // and so the DDL joins one transaction with anything else in the
                // batch (§3.3).
                let name =
                    TableConstraint::derived_name(&table_name, &constraint.kind, &given_name);
                schema_edit::apply(
                    &catalog,
                    &[schema_edit::Operation::AddConstraint {
                        table: table_name.clone(),
                        given_name,
                        constraint,
                    }],
                    &schema_edit::ApplyOptions::default(),
                )
                .await?;
                let table = catalog.require(&table_name)?;
                let created = table
                    .constraints
                    .iter()
                    .find(|c| c.name == name)
                    .ok_or_else(|| {
                        Error::msg(format!("constraint `{name}` missing after create"))
                    })?;
                Ok(HandlerResponse::ok(constraint_json(created)).with_status(201))
            }
        }
    });

    reg.register("deleteConstraint", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let table = ctx.path_param("table")?.to_owned();
                let name = ctx.path_param("constraint")?.to_owned();
                schema_edit::apply(
                    &catalog,
                    &[schema_edit::Operation::DropConstraint {
                        table,
                        name: name.clone(),
                    }],
                    &schema_edit::ApplyOptions::default(),
                )
                .await?;
                Ok(HandlerResponse::ok(json!({ "dropped": name })))
            }
        }
    });

    reg.register("listFieldTypes", {
        let catalog = catalog.clone();
        move |_ctx| {
            let catalog = catalog.clone();
            async move {
                let mut out = Vec::new();
                // Basic types first (no attributes), then rich types with their
                // attribute specs, then the Key/File kinds — one list the field
                // editor assembles its picker from (§3.4).
                for basic in schema_edit::basic_field_types() {
                    out.push(field_type_json(basic.name(), "basic", &[]));
                }
                for name in registered_rich_types() {
                    let spec = rich_type_config_spec(&name)?;
                    out.push(field_type_json(&name, "rich", &spec));
                }
                for (name, spec) in [
                    ("key", key_kind_config_spec()),
                    ("file", file_kind_config_spec()),
                ] {
                    // Resolve any `server_query` (the File store pick-list) to a
                    // static list before the spec leaves the server.
                    let resolved = resolve_options(&catalog, spec).await?;
                    out.push(field_type_json(name, "kind", &resolved));
                }
                Ok(HandlerResponse::ok(Json::Array(out)))
            }
        }
    });

    // --- row CRUD -----------------------------------------------------------

    reg.register("listRows", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let table = catalog.require(ctx.path_param("table")?)?;
                Ok(HandlerResponse::ok(
                    rows::list_rows_ctx(&catalog, &table, Some(&admin_caller(ctx.user.as_ref())))
                        .await?,
                ))
            }
        }
    });

    reg.register("countRows", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let table = catalog.require(ctx.path_param("table")?)?;
                let count =
                    rows::count_rows(&catalog, &table, Some(&admin_caller(ctx.user.as_ref())))
                        .await?;
                Ok(HandlerResponse::ok(json!({ "count": count })))
            }
        }
    });

    reg.register("createRow", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let table = catalog.require(ctx.path_param("table")?)?;
                let row = rows::create_row_ctx(
                    &catalog,
                    &table,
                    &ctx.body,
                    Some(&admin_caller(ctx.user.as_ref())),
                )
                .await?;
                Ok(HandlerResponse::ok(row).with_status(201))
            }
        }
    });

    reg.register("updateRow", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let table = catalog.require(ctx.path_param("table")?)?;
                let id = ctx.path_param("id")?;
                let row = rows::update_row_ctx(
                    &catalog,
                    &table,
                    id,
                    &ctx.body,
                    Some(&admin_caller(ctx.user.as_ref())),
                )
                .await?;
                Ok(HandlerResponse::ok(row))
            }
        }
    });

    reg.register("deleteRow", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let table = catalog.require(ctx.path_param("table")?)?;
                let id = ctx.path_param("id")?;
                // The row layer hands back the row it removed; the admin API
                // acknowledges, as it does for every other delete it serves.
                rows::delete_row_ctx(&catalog, &table, id, Some(&admin_caller(ctx.user.as_ref())))
                    .await?;
                Ok(HandlerResponse::ok(json!({ "deleted": true })))
            }
        }
    });

    // --- rows in bulk, as CSV ----------------------------------------------

    reg.register("exportTableCsv", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let table = catalog.require(ctx.path_param("table")?)?;
                let document = csv_rows::export_table(
                    &catalog,
                    &table,
                    Some(&admin_caller(ctx.user.as_ref())),
                )
                .await?;
                // The browser saves the file, so the server says what to call
                // it: the table's own name is the only thing that makes three
                // downloads distinguishable in a downloads folder.
                Ok(HandlerResponse::ok(json!({
                    "filename": format!("{}.csv", table.name),
                    "csv": document,
                })))
            }
        }
    });

    reg.register("importTableCsv", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let table = catalog.require(ctx.path_param("table")?)?;
                let document = require_object(&ctx.body)?
                    .get("csv")
                    .and_then(Json::as_str)
                    .ok_or_else(|| Error::invalid("`csv` must be the CSV document as text"))?
                    .to_owned();
                let outcome = csv_rows::import_table(
                    &catalog,
                    &table,
                    &document,
                    Some(&admin_caller(ctx.user.as_ref())),
                )
                .await?;
                Ok(HandlerResponse::ok(json!({
                    "inserted": outcome.inserted,
                    "updated": outcome.updated,
                    "errors": outcome.errors,
                })))
            }
        }
    });

    // --- database connections ----------------------------------------------
    // The other databases whose tables share the tables list. Every response
    // goes through `db_connection_json`, which is where the password becomes the
    // sentinel — the same discipline `llm_provider_json` keeps, and for the same
    // reason: an endpoint added later would have to build the JSON by hand to
    // leak one.

    reg.register("listDatabaseConnections", {
        let catalog = catalog.clone();
        move |_ctx| {
            let catalog = catalog.clone();
            async move {
                let defs = list_db_connections(&catalog).await?;
                let out: Vec<Json> = defs
                    .iter()
                    .map(|def| db_connection_json(&catalog, def))
                    .collect::<Result<_>>()?;
                Ok(HandlerResponse::ok(Json::Array(out)))
            }
        }
    });

    reg.register("createDatabaseConnection", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let def = db_connection_from_body(DbConnectionId::new(), &ctx.body, None)?;
                // Checked before dialling so a clashing name fails fast, and
                // dialled before saving so a create that could never work leaves
                // no row behind — the same order, for the same reason, as
                // `createFileStore`.
                check_db_connection_saveable(&catalog, &def).await?;
                save_db_connection(&catalog, &def).await?;
                // A failure here is deliberately *not* an error: the connection
                // was created, and the reason travels in the response so the UI
                // can show "saved, but not connected: <why>".
                let _ = connect_db_connection(&catalog, &def).await;
                catalog.reload().await?;
                Ok(HandlerResponse::ok(db_connection_json(&catalog, &def)?).with_status(201))
            }
        }
    });

    reg.register("updateDatabaseConnection", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let id = parse_db_connection_id(ctx.path_param("id")?)?;
                let existing = load_db_connection(&catalog, id).await?.ok_or_else(|| {
                    Error::not_found(format!("no database connection with id {id:?}"))
                })?;
                // The id is the path's, not the body's. A password sent back as
                // the sentinel is the sentinel the form was shown, not the
                // password, so what is stored is restored (§11.1).
                let def = db_connection_from_body(id, &ctx.body, Some(&existing))?;
                // Attributes are server-managed and the edit form neither shows
                // nor sends them, so they are carried across rather than reset.
                let def = DbConnectionDef {
                    attributes: existing.attributes.clone(),
                    ..def
                };
                save_db_connection(&catalog, &def).await?;

                // A rename leaves the old driver registered under the old name,
                // still contributing tables stamped with a connection that no
                // longer exists. This is the one place that knows both the
                // before and the after, so it is where the old one goes.
                if existing.name != def.name {
                    catalog.disconnect_database(&existing.name)?;
                }
                // Re-dial under the current settings so an edited host takes
                // effect immediately, with no restart.
                catalog.disconnect_database(&def.name)?;
                let _ = connect_db_connection(&catalog, &def).await;
                catalog.reload().await?;
                Ok(HandlerResponse::ok(db_connection_json(&catalog, &def)?))
            }
        }
    });

    reg.register("deleteDatabaseConnection", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let id = parse_db_connection_id(ctx.path_param("id")?)?;
                let existing = load_db_connection(&catalog, id).await?.ok_or_else(|| {
                    Error::not_found(format!("no database connection with id {id:?}"))
                })?;
                let deleted = delete_db_connection(&catalog, id).await?;
                // Removing the row is the definition's end; disconnecting and
                // reloading is what takes its tables out of the catalog. Both
                // are needed, or the tables list would go on offering tables
                // from a connection the admin had just removed.
                if deleted {
                    catalog.disconnect_database(&existing.name)?;
                    catalog.reload().await?;
                }
                Ok(HandlerResponse::ok(json!({ "deleted": deleted })))
            }
        }
    });

    reg.register("testDatabaseConnection", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                // A test of an *existing* connection may carry the sentinel
                // instead of the password — the form shows the sentinel — so the
                // stored one is looked up by name and merged back in. Without
                // this, Test would fail on every connection the admin had not
                // retyped the password for.
                let name = require_object(&ctx.body)?
                    .get("name")
                    .and_then(Json::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_owned();
                let existing = if name.is_empty() {
                    None
                } else {
                    load_db_connection_by_name(&catalog, &name).await?
                };
                let def =
                    db_connection_from_body(DbConnectionId::new(), &ctx.body, existing.as_ref())?;
                Ok(HandlerResponse::ok(
                    match sc_catalog::dial(&catalog, &def).await {
                        Ok(driver) => json!({
                            "connected": true,
                            "error": Json::Null,
                            "tables": driver.introspect().await?.len(),
                        }),
                        // A failed test is an answer, not a request that went wrong:
                        // 200 with the reason, so the form shows it beside the
                        // button rather than as a red banner about the API.
                        Err(e) => json!({
                            "connected": false,
                            "error": sc_error::format_causes(&e),
                            "tables": 0,
                        }),
                    },
                ))
            }
        }
    });

    // --- files (file manager) ----------------------------------------------

    // The **union** of defined and connected stores, which neither alone gets
    // right. Listing only the connected ones (what the MVP did) hides a store
    // whose directory has been unmounted — exactly the store the admin needs to
    // find and repoint. Listing only the definitions hides a store connected by
    // `--file-store`, which has no row but is real and browsable, so a developer
    // running with the flag would see an empty list. Stored stores come first,
    // then any connected store with no definition, which reports a null id to
    // say "there is nothing here to edit".
    reg.register("listFileStores", {
        let catalog = catalog.clone();
        move |_ctx| {
            let catalog = catalog.clone();
            async move {
                let defs = list_file_stores(&catalog).await?;
                let mut out: Vec<Json> = defs
                    .iter()
                    .map(|def| file_store_json(&catalog, def))
                    .collect::<Result<_>>()?;

                for name in catalog.file_store_names()? {
                    if !defs.iter().any(|def| def.name == name) {
                        out.push(ephemeral_file_store_json(&catalog, &name)?);
                    }
                }
                Ok(HandlerResponse::ok(Json::Array(out)))
            }
        }
    });

    reg.register("createFileStore", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                // A create mints a fresh id; the body carries everything else.
                let mut def = file_store_from_body(FileStoreDefId::new(), &ctx.body)?;

                // **Creating a store is transactional.** The cheap checks run
                // first, then whatever the backend has to build, and only then
                // is anything written — so a git store whose clone fails leaves
                // no row at all. Saving first and reporting the failure (what
                // an *edit* does, §1.2) is wrong here for a concrete reason:
                // the admin corrects the URL, presses Create again, and is told
                // the name is already taken — by the row their failed attempt
                // left behind. There is nothing to preserve on a create, since
                // the form still holds everything they typed.
                check_file_store_saveable(&catalog, &def).await?;
                create_backend_resources(&mut def).await?;

                save_file_store(&catalog, &def).await?;
                // Connect it straight away so the admin finds out now whether
                // the store is actually reachable, rather than on next boot. A
                // failure here is deliberately *not* an error — the store was
                // created, and the reason is reported in the response so the UI
                // can show "saved, but not connected: <why>".
                let _ = connect_file_store_def(&catalog, &def);
                Ok(HandlerResponse::ok(file_store_json(&catalog, &def)?).with_status(201))
            }
        }
    });

    reg.register("updateFileStore", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let id = parse_file_store_id(ctx.path_param("id")?)?;
                let existing = load_file_store(&catalog, id)
                    .await?
                    .ok_or_else(|| Error::not_found(format!("no file store with id {id:?}")))?;
                // The id is the path's, not the body's — the row's identity is
                // not something a payload gets to reassign.
                let mut def = file_store_from_body(id, &ctx.body)?;
                // Attributes are server-managed (§9) — a git store's recorded
                // clone directory lives there — and the edit form neither shows
                // nor sends them, so they are carried across rather than reset.
                // Dropping them would orphan a working tree on the next save.
                def.attributes = existing.attributes.clone();
                // A secret the form sent back untouched is the sentinel it was
                // shown, not the key: restore what is stored (§11.1). A
                // create-only setting is restored whatever was sent — the store
                // exists, so that setting is no longer the admin's to change.
                def.config = unredacted_config(&def.backend, &existing.config, &def.config);
                save_file_store(&catalog, &def).await?;

                // A rename leaves the old handle connected under the old name,
                // still serving, because saving deliberately does not touch the
                // registry (§1.1). Disconnect it here — this is the one place
                // that knows both the before and the after.
                if existing.name != def.name {
                    catalog.disconnect_file_store(&existing.name)?;
                }
                // Reconnect under the current name so an edited path takes
                // effect immediately, with no restart (§1.3).
                recreate_backend_resources(&catalog, &mut def).await?;
                Ok(HandlerResponse::ok(file_store_json(&catalog, &def)?))
            }
        }
    });

    reg.register("deleteFileStore", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let id = parse_file_store_id(ctx.path_param("id")?)?;
                let existing = load_file_store(&catalog, id)
                    .await?
                    .ok_or_else(|| Error::not_found(format!("no file store with id {id:?}")))?;

                // Collect the references `sc-catalog` cannot see. Applications
                // live a crate above it, so this is the one place that can put
                // both halves of the check together — without it a store still
                // serving an app's source could be deleted out from under it.
                let app_refs = applications_using_file_store(&catalog, &existing.name).await?;
                let deleted = delete_file_store(&catalog, id, &app_refs).await?;

                // Removing the row is the definition's end; disconnecting is what
                // stops it serving. Both are needed — the file manager would
                // otherwise go on browsing a store the admin had just deleted.
                if deleted {
                    catalog.disconnect_file_store(&existing.name)?;
                }
                Ok(HandlerResponse::ok(json!({ "deleted": deleted })))
            }
        }
    });

    reg.register("listFileStoreBackends", {
        let catalog = catalog.clone();
        move |_ctx| {
            let catalog = catalog.clone();
            async move {
                let mut out = Vec::new();
                for name in registered_backends() {
                    let spec = resolve_options(&catalog, backend_config_spec(&name)?).await?;
                    out.push(json!({
                        "name": name,
                        "config_spec": spec.iter().map(form_field_json).collect::<Vec<_>>(),
                        // What the backend can *do*, declared the same way as
                        // what it can be told — so the form renders a button per
                        // operation without knowing what any of them mean.
                        "operations": backend_operations(&name)?
                            .iter()
                            .map(operation_json)
                            .collect::<Vec<_>>(),
                    }));
                }
                Ok(HandlerResponse::ok(Json::Array(out)))
            }
        }
    });

    // --- backend operations -------------------------------------------------
    // Both handlers are backend-agnostic: they look up what the backend declared
    // and run it. Nothing here knows what a repository, a clone or a deploy key
    // is — which is what lets a backend added later, in Rust or through
    // `sc-code`, have buttons in the admin UI without either of these changing.

    // Configure scope: against configuration the admin has not saved yet, so the
    // definition is built from the body and thrown away, and what comes back is
    // its config for the form to adopt.
    reg.register("runBackendOperation", {
        move |ctx| async move {
            let backend = ctx.path_param("backend")?.to_owned();
            let operation = ctx.path_param("operation")?.to_owned();
            let obj = require_object(&ctx.body)?;
            let name = obj.get("name").and_then(Json::as_str).unwrap_or("");
            let mut def = FileStoreDef {
                config: object_field(obj, "config")?,
                ..FileStoreDef::new(name, &backend)
            };
            require_scope(&backend, &operation, OperationScope::Configure)?;

            let outcome =
                run_backend_operation(&mut def, &operation, &object_field(obj, "input")?).await?;
            Ok(HandlerResponse::ok(json!({
                "config": Json::Object(def.config),
                "output": outcome.output,
                "data": outcome.data,
            })))
        }
    });

    // Instance scope: against a saved store. Whatever the operation changed in
    // the definition is persisted, and the store is reconnected — an operation
    // may be exactly what makes it connectable (a clone), and an admin who has
    // just repaired a store should not have to save again to use it.
    reg.register("runFileStoreOperation", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let id = parse_file_store_id(ctx.path_param("id")?)?;
                let operation = ctx.path_param("operation")?.to_owned();
                let mut def = load_file_store(&catalog, id)
                    .await?
                    .ok_or_else(|| Error::not_found(format!("no file store with id {id:?}")))?;
                require_scope(&def.backend, &operation, OperationScope::Instance)?;
                let input = match &ctx.body {
                    Json::Object(obj) => object_field(obj, "input")?,
                    _ => sc_types::Attrs::new(),
                };

                let before = def.clone();
                let outcome = run_backend_operation(&mut def, &operation, &input).await?;
                // Only when it actually changed something: an operation that
                // merely reports (a status) must not rewrite the row on every
                // screen open.
                if def != before {
                    save_file_store(&catalog, &def).await?;
                }
                // Failure stays a non-error, exactly as it is on save (§1.2):
                // pulling a store whose disk has gone should report the pull's
                // outcome, not swallow it behind a connection error.
                let _ = connect_file_store_def(&catalog, &def);
                Ok(HandlerResponse::ok(json!({
                    "config": Json::Object(def.config),
                    "output": outcome.output,
                    // Optional and backend-shaped (§14.1): the git backend fills
                    // it with the working copy's state so the IDE's source
                    // control view can draw itself; every other backend leaves it
                    // null and every other client ignores it.
                    "data": outcome.data,
                    "connected": catalog.file_store(&def.name)?.is_some(),
                })))
            }
        }
    });

    // --- LLM providers ------------------------------------------------------
    //
    // The file-store handlers one section up, with one difference that runs
    // through all of them: a provider's config holds an API key, so it is
    // redacted on the way out (`llm_provider_json`) and the sentinel is merged
    // back on the way in (`unredacted_provider_config`). Both happen here, where
    // the record is serialised, rather than in the screen — a second reader added
    // later gets the same treatment without having to know it needs it (§11.1).

    reg.register("listLlmProviders", {
        let catalog = catalog.clone();
        move |_ctx| {
            let catalog = catalog.clone();
            async move {
                let out: Vec<Json> = list_llm_providers(&catalog)
                    .await?
                    .iter()
                    .map(llm_provider_json)
                    .collect();
                Ok(HandlerResponse::ok(Json::Array(out)))
            }
        }
    });

    reg.register("createLlmProvider", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                // A create mints a fresh id; the body carries everything else.
                // There is nothing stored to merge a sentinel against, so a
                // submitted sentinel is dropped by `merge_secrets` and the save
                // fails on the missing required key — which is the right answer
                // for a form that was never given one.
                let def = llm_provider_from_body(LlmProviderDefId::new(), &ctx.body)?;
                save_llm_provider(&catalog, &def).await?;
                Ok(HandlerResponse::ok(llm_provider_json(&def)).with_status(201))
            }
        }
    });

    reg.register("updateLlmProvider", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let id = parse_llm_provider_id(ctx.path_param("id")?)?;
                let existing = load_llm_provider(&catalog, id)
                    .await?
                    .ok_or_else(|| Error::not_found(format!("no LLM provider with id {id:?}")))?;
                // The id is the path's, not the body's — the row's identity is
                // not something a payload gets to reassign.
                let mut def = llm_provider_from_body(id, &ctx.body)?;
                // Attributes are server-managed (§9) and the form neither shows
                // nor sends them, so they are carried across rather than reset.
                def.attributes = existing.attributes.clone();
                // The key the form sent back untouched is the sentinel it was
                // shown, not the key: restore what is stored.
                def.config =
                    unredacted_provider_config(&def.backend, &existing.config, &def.config);
                save_llm_provider(&catalog, &def).await?;
                Ok(HandlerResponse::ok(llm_provider_json(&def)))
            }
        }
    });

    reg.register("deleteLlmProvider", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let id = parse_llm_provider_id(ctx.path_param("id")?)?;
                // The agents naming this provider are the references `sc-llm`
                // cannot see for itself: `_sc_agents` is a layer above it, so
                // they are collected here and passed in (the same arrangement
                // `delete_file_store` has with applications). Deleting a
                // provider an agent still calls through would leave that agent
                // unable to answer, with the reason a layer away from the
                // action that caused it.
                let users = agents_using_provider(&catalog, id).await?;
                let deleted = delete_llm_provider(&catalog, id, &users).await?;
                Ok(HandlerResponse::ok(json!({ "deleted": deleted })))
            }
        }
    });

    reg.register("listLlmProviderBackends", {
        let catalog = catalog.clone();
        move |_ctx| {
            let catalog = catalog.clone();
            async move {
                let mut out = Vec::new();
                for name in sc_llm::registered_backends() {
                    let spec = resolve_options(&catalog, provider_config_spec(&name)?).await?;
                    out.push(json!({
                        "name": name,
                        "config_spec": spec.iter().map(form_field_json).collect::<Vec<_>>(),
                    }));
                }
                Ok(HandlerResponse::ok(Json::Array(out)))
            }
        }
    });

    // Send one trivial prompt and report what came back. This is the only
    // handler in the admin API that waits on a third party, and it is worth it:
    // without it a wrong key is discovered inside a chat transcript, where it
    // looks like the agent misbehaving rather than the configuration being
    // wrong.
    //
    // A failure is a **200 with `ok: false`**, not an error status. The provider
    // refusing is the answer to the question that was asked — "does this work?"
    // — and an error response would make the UI show it as a broken request
    // rather than as the diagnostic it is.
    reg.register("testLlmProvider", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let obj = require_object(&ctx.body)?;
                let backend = non_empty_str_field(obj, "backend")?.to_owned();
                let submitted = object_field(obj, "config")?;
                let model = obj.get("model").and_then(Json::as_str).unwrap_or("");

                // Testing a *saved* provider must not require retyping its key,
                // so a submitted sentinel resolves against the stored row when
                // the body names one.
                let config = match obj.get("id").and_then(Json::as_str) {
                    Some(raw) => {
                        let id = parse_llm_provider_id(raw)?;
                        match load_llm_provider(&catalog, id).await? {
                            Some(stored) => {
                                unredacted_provider_config(&backend, &stored.config, &submitted)
                            }
                            None => submitted,
                        }
                    }
                    None => submitted,
                };

                let def = LlmProviderDef {
                    config,
                    ..LlmProviderDef::new("test", &backend)
                };
                // A structurally wrong config is an ordinary `Err`: it is the
                // admin's typo, not the provider's answer, and the form should
                // show it the way it shows a failed save.
                let provider = connect_provider(&def, Some(model))?;
                let model = provider.model().to_owned();

                // Capped hard: the question is "does this endpoint answer",
                // and a provider that takes it as an invitation to write an
                // essay would bill the admin for asking.
                let probe = LlmRequest::prompt("Reply with the single word: ok").max_tokens(16);
                let outcome = match provider.stream(probe).await {
                    Ok(stream) => stream.collect().await,
                    // Failing to *start* — a refused connection, a rejected key
                    // — is the same kind of answer as failing mid-stream, and
                    // the admin reads it the same way.
                    Err(e) => Err(e),
                };

                Ok(HandlerResponse::ok(match outcome {
                    Ok(msg) => json!({
                        "ok": true,
                        // What the model actually said, so an admin who pointed
                        // at the wrong endpoint sees a wrong answer rather than
                        // a green tick.
                        "message": msg.content.trim(),
                        "model": model,
                    }),
                    // The provider's own words, whole. A category ("auth
                    // failed") would throw away the part that says *which* key
                    // or *which* model.
                    Err(e) => json!({
                        "ok": false,
                        "message": sc_error::format_causes(&e),
                        "model": model,
                    }),
                }))
            }
        }
    });

    // --- modules ------------------------------------------------------------
    // A module is somebody else's npm package, so every one of these handlers
    // does the same two-part act: change the disk or the row, then **reload** —
    // rebuild the action registry from the built-ins plus every installed
    // module, swap it into the dispatcher, and revalidate the triggers against
    // it (`ModuleServices::reload`). That is what makes a module's actions
    // usable on a running server, which is the whole point of installing one
    // from a form rather than from a deploy.

    reg.register("listModules", {
        let apps = apps.clone();
        move |_ctx| {
            let apps = apps.clone();
            async move {
                let services = modules_of(&apps)?;
                let set = services.modules();
                let modules: Vec<Json> = set.modules().iter().map(module_json).collect();
                Ok(HandlerResponse::ok(json!({
                    "modules": modules,
                    "root": services.installer().root().display().to_string(),
                    // Asked here rather than assumed, because a server with no
                    // Node toolchain can do everything else and nothing on this
                    // tab — and the tab should say so before the admin types a
                    // package name.
                    "npm": sc_module::have_npm().await,
                    "node": sc_module::have_node().await,
                })))
            }
        }
    });

    reg.register("installModule", {
        let catalog = catalog.clone();
        let apps = apps.clone();
        move |ctx| {
            let catalog = catalog.clone();
            let apps = apps.clone();
            async move {
                let services = modules_of(&apps)?;
                let obj = require_object(&ctx.body)?;
                let source = sc_module::ModuleSource::parse(non_empty_str_field(obj, "source")?)?;
                let location = non_empty_str_field(obj, "location")?.trim().to_owned();

                let package = services.installer().install(source, &location).await?;
                // A package name that is already installed is an **upgrade**,
                // not a collision: reinstalling is how a module is upgraded
                // (TODO, out of scope: an upgrade UI), and the admin who typed
                // `@saltcorn/mqtt@0.3.0` over a 0.2.0 install means exactly
                // that. The row keeps its id and its configuration.
                let mut module = match load_module_by_name(&catalog, &package.name).await? {
                    Some(existing) => existing,
                    None => sc_module::Module::new(&package.name, source, &location),
                };
                module.source = source;
                module.location = location;
                module.version = Some(package.version);
                save_module(&catalog, &module).await?;

                services.reload().await?;
                let set = services.modules();
                let loaded = set
                    .get(&module.name)
                    .ok_or_else(|| Error::msg("the installed module is not in the loaded set"))?;
                Ok(HandlerResponse::ok(module_json(loaded)).with_status(201))
            }
        }
    });

    reg.register("updateModule", {
        let catalog = catalog.clone();
        let apps = apps.clone();
        move |ctx| {
            let catalog = catalog.clone();
            let apps = apps.clone();
            async move {
                let services = modules_of(&apps)?;
                let id = parse_module_id(ctx.path_param("id")?)?;
                let mut module = sc_module::require_module(&catalog, id).await?;
                let obj = require_object(&ctx.body)?;

                // Two independent edits behind one endpoint, and each takes
                // effect only when it was **sent**: the settings form and the
                // permissions form are separate saves, and a save of one must
                // not clear the other.
                if let Some(submitted) = obj.get("configuration").filter(|v| !v.is_null()) {
                    let Json::Object(submitted) = submitted.clone() else {
                        return Err(Error::invalid("field `configuration` must be an object"));
                    };
                    // A module's `password` setting is a secret like any other:
                    // what the form was shown is the sentinel, and submitting it
                    // back unchanged keeps what is stored.
                    let set = services.modules();
                    module.configuration = match set.get(&module.name) {
                        Some(loaded) => sc_types::merge_secrets(
                            &loaded.config_spec,
                            &module.configuration,
                            &submitted,
                        ),
                        None => submitted,
                    };
                }
                if let Some(submitted) = obj.get("permissions").filter(|v| !v.is_null()) {
                    // Checked here rather than at the worker: a permission an
                    // admin typed wrong should be refused in front of the form,
                    // not discovered as a module that stopped working.
                    module.permissions = sc_module::ModulePermissions::from_json(submitted)?;
                }
                save_module(&catalog, &module).await?;

                // The configuration is what v1's `actions(cfg)` is called with,
                // so it only takes effect when the module is loaded again — and
                // a permission change moves the module onto a worker built with
                // the new set, which the same reload does.
                services.reload().await?;
                let set = services.modules();
                let loaded = set
                    .get(&module.name)
                    .ok_or_else(|| Error::msg("the saved module is not in the loaded set"))?;
                Ok(HandlerResponse::ok(module_json(loaded)))
            }
        }
    });

    reg.register("deleteModule", {
        let catalog = catalog.clone();
        let apps = apps.clone();
        move |ctx| {
            let catalog = catalog.clone();
            let apps = apps.clone();
            async move {
                let services = modules_of(&apps)?;
                let id = parse_module_id(ctx.path_param("id")?)?;
                let Some(module) = load_module(&catalog, id).await? else {
                    return Ok(HandlerResponse::ok(json!({ "deleted": false })));
                };
                // The host first, so a restarted one does not reload a package
                // that is about to go; then npm; then the row, which is what
                // makes the module exist at all. npm failing must not leave a
                // module nobody can delete, so its complaint is logged rather
                // than returned.
                services.host().unload(&module.name).await;
                if let Err(e) = services.installer().uninstall(&module.name).await {
                    sc_log::log_error!(
                        "saltcorn: the module `{}` was removed but its package could not be \
                         uninstalled: {}",
                        module.name,
                        sc_error::format_chain(&e)
                    );
                }
                let deleted = sc_module::delete_module(&catalog, id).await?;
                services.reload().await?;
                Ok(HandlerResponse::ok(json!({ "deleted": deleted })))
            }
        }
    });

    reg.register("reloadModules", {
        let apps = apps.clone();
        move |_ctx| {
            let apps = apps.clone();
            async move {
                let services = modules_of(&apps)?;
                services.reload().await?;
                let count = services.modules().modules().len();
                Ok(HandlerResponse::ok(json!({ "modules": count })))
            }
        }
    });

    // --- agents -------------------------------------------------------------
    // The row ⇄ live-set path again (§11.2), for the third record that has it:
    // every save goes through `save_agent`, which validates against the *same*
    // registry the chat socket runs with, and the list carries the reason a
    // stored agent is not usable beside it — because editing it is the repair.

    reg.register("listAgents", {
        let catalog = catalog.clone();
        let apps = apps.clone();
        move |_ctx| {
            let catalog = catalog.clone();
            let apps = apps.clone();
            async move {
                // The **stored** rows, with the live set consulted only for its
                // issues: an agent that fails validation would otherwise vanish
                // from the screen that exists to repair it.
                let services = agents_of(&apps)?;
                let stored = sc_agent::list_agents(&catalog).await?;
                let live = sc_agent::validate::Agents::load(&catalog, services.registry()).await?;
                let out: Vec<Json> = stored
                    .iter()
                    .map(|agent| {
                        let problem = live
                            .issues()
                            .iter()
                            .find(|i| i.agent == agent.name)
                            .map(|i| i.problem.clone());
                        agent_json(agent, problem)
                    })
                    .collect();
                Ok(HandlerResponse::ok(Json::Array(out)))
            }
        }
    });

    reg.register("createAgent", {
        let catalog = catalog.clone();
        let apps = apps.clone();
        move |ctx| {
            let catalog = catalog.clone();
            let apps = apps.clone();
            async move {
                let services = agents_of(&apps)?;
                let agent = agent_from_body(sc_agent::AgentId::new(), &ctx.body)?;
                sc_agent::save_agent(&catalog, services.registry(), &agent).await?;
                Ok(HandlerResponse::ok(agent_json(&agent, None)).with_status(201))
            }
        }
    });

    reg.register("updateAgent", {
        let catalog = catalog.clone();
        let apps = apps.clone();
        move |ctx| {
            let catalog = catalog.clone();
            let apps = apps.clone();
            async move {
                let services = agents_of(&apps)?;
                let id = parse_agent_id(ctx.path_param("id")?)?;
                if sc_agent::load_agent(&catalog, id).await?.is_none() {
                    return Err(Error::not_found(format!("no agent with id {id}")));
                }
                // The id is the path's, not the body's — the row's identity is
                // not something a payload gets to reassign.
                let agent = agent_from_body(id, &ctx.body)?;
                sc_agent::save_agent(&catalog, services.registry(), &agent).await?;
                Ok(HandlerResponse::ok(agent_json(&agent, None)))
            }
        }
    });

    reg.register("deleteAgent", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let id = parse_agent_id(ctx.path_param("id")?)?;
                if !sc_agent::delete_agent(&catalog, id).await? {
                    return Err(Error::not_found(format!("no agent with id {id}")));
                }
                // Its runs are **not** deleted with it: a run is a record of what
                // happened, its subject is the agent's name rather than its id
                // (§11.4), and a transcript that disappeared with the definition
                // would take the evidence with it.
                Ok(HandlerResponse::ok(json!({ "deleted": true })))
            }
        }
    });

    reg.register("listAgentTraits", {
        let catalog = catalog.clone();
        let apps = apps.clone();
        move |_ctx| {
            let catalog = catalog.clone();
            let apps = apps.clone();
            async move {
                let services = agents_of(&apps)?;
                let mut out = Vec::new();
                for trait_ in services.registry().all() {
                    // Resolved like every other spec the admin UI renders, so a
                    // setting whose options come from the catalog (a table name,
                    // a trigger name) arrives as a picker rather than a text box.
                    let spec = resolve_options(&catalog, trait_.config_spec()).await?;
                    out.push(json!({
                        "name": trait_.name(),
                        "description": trait_.description(),
                        "config_spec": spec.iter().map(form_field_json).collect::<Vec<_>>(),
                    }));
                }
                Ok(HandlerResponse::ok(Json::Array(out)))
            }
        }
    });

    // --- runs ---------------------------------------------------------------

    reg.register("listRuns", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let agent = ctx.path_param("agent")?.to_owned();
                let out: Vec<Json> = sc_agent::list_runs(&catalog, &agent)
                    .await?
                    .iter()
                    .map(run_summary_json)
                    .collect();
                Ok(HandlerResponse::ok(Json::Array(out)))
            }
        }
    });

    reg.register("getRun", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let id = sc_agent::RunId(parse_uuid(ctx.path_param("id")?, "run")?);
                let run = sc_agent::require_run(&catalog, id).await?;
                Ok(HandlerResponse::ok(
                    run_json_with_workflow(&catalog, &run).await?,
                ))
            }
        }
    });

    reg.register("deleteRun", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let id = sc_agent::RunId(parse_uuid(ctx.path_param("id")?, "run")?);
                if !sc_agent::delete_run(&catalog, id).await? {
                    return Err(Error::not_found(format!("no run with id {id}")));
                }
                Ok(HandlerResponse::ok(json!({ "deleted": true })))
            }
        }
    });

    reg.register("browseFiles", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let name = ctx.path_param("store")?.to_owned();
                let (store, floor) = resolve_store(&catalog, &name).await?;
                let obj = require_object(&ctx.body)?;
                // `dir` is optional-ish: an absent/empty value lists the root.
                let dir = obj.get("dir").and_then(Json::as_str).unwrap_or("");
                let role = caller_role(&ctx);
                check_access(store.as_ref(), floor, dir, role).await?;

                // The directory's own floor covers every child, so it is
                // computed once here rather than re-walking the ancestors for
                // each entry.
                let dir_floor = effective_min_role(store.as_ref(), floor, dir).await?;
                let entries = store.list(dir).await?;
                // Filtered, not refused: a readable directory may hold entries
                // the caller cannot open, and listing their names leaks exactly
                // what the rule was set to hide.
                let visible = filter_visible(store.as_ref(), dir_floor, entries, role).await?;
                let out: Vec<Json> = visible.iter().map(entry_json).collect();
                Ok(HandlerResponse::ok(Json::Array(out)))
            }
        }
    });

    reg.register("searchFiles", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let name = ctx.path_param("store")?.to_owned();
                let (store, floor) = resolve_store(&catalog, &name).await?;
                let obj = require_object(&ctx.body)?;
                let dir = obj.get("dir").and_then(Json::as_str).unwrap_or("");
                let role = caller_role(&ctx);
                // The directory the search starts in is reached like any other,
                // so a search rooted somewhere the caller may not go is refused
                // rather than quietly returning nothing.
                check_access(store.as_ref(), floor, dir, role).await?;

                let glob = obj
                    .get("glob")
                    .and_then(Json::as_str)
                    .map(str::trim)
                    .filter(|g| !g.is_empty())
                    .map(str::to_owned);
                let flag = |key: &str| obj.get(key).and_then(Json::as_bool).unwrap_or(false);
                let max_results = match obj.get("max_results").and_then(Json::as_i64) {
                    Some(n) if n >= 1 => n as usize,
                    _ => sc_files::DEFAULT_MAX_RESULTS,
                };
                let query = sc_files::SearchQuery {
                    pattern: non_empty_str_field(obj, "pattern")?.to_owned(),
                    regex: flag("regex"),
                    case_sensitive: flag("case_sensitive"),
                    whole_word: flag("whole_word"),
                    glob,
                    dir: dir.to_owned(),
                    max_results,
                    ..sc_files::SearchQuery::literal("")
                };
                // The caller's own role, so the walk skips what a listing would
                // have hidden: a search must not report a line out of a file the
                // caller could not open.
                let found = sc_files::search_store(store.as_ref(), floor, role, &query).await?;
                let matches: Vec<Json> = found
                    .hits
                    .iter()
                    .map(|hit| {
                        json!({
                            "path": hit.path,
                            "line": hit.line,
                            "column": hit.column,
                            "length": hit.length,
                            "text": hit.text,
                        })
                    })
                    .collect();
                Ok(HandlerResponse::ok(json!({
                    "matches": matches,
                    "files_searched": found.files_scanned,
                    "truncated": found.truncated,
                })))
            }
        }
    });

    reg.register("readFile", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let name = ctx.path_param("store")?.to_owned();
                let (store, floor) = resolve_store(&catalog, &name).await?;
                let obj = require_object(&ctx.body)?;
                let path = non_empty_str_field(obj, "path")?;
                check_access(store.as_ref(), floor, path, caller_role(&ctx)).await?;
                let bytes = store.read(path).await?;
                // Always provide base64 (for binary download); add a decoded
                // `text` when the bytes are valid UTF-8 (for the text editor).
                let text = std::str::from_utf8(&bytes)
                    .ok()
                    .map(|s| Json::String(s.to_owned()))
                    .unwrap_or(Json::Null);
                Ok(HandlerResponse::ok(json!({
                    "path": path,
                    "size": bytes.len(),
                    "base64": BASE64.encode(&bytes),
                    "text": text,
                })))
            }
        }
    });

    reg.register("writeFile", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let name = ctx.path_param("store")?.to_owned();
                let (store, floor) = resolve_store(&catalog, &name).await?;
                let obj = require_object(&ctx.body)?;
                let path = non_empty_str_field(obj, "path")?.to_owned();
                check_access(store.as_ref(), floor, &path, caller_role(&ctx)).await?;
                let data = file_body_bytes(obj)?;
                let size = data.len();
                store.write(&path, data).await?;
                Ok(HandlerResponse::ok(file_entry_written_json(&path, size)).with_status(201))
            }
        }
    });

    reg.register("makeDirectory", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let name = ctx.path_param("store")?.to_owned();
                let (store, floor) = resolve_store(&catalog, &name).await?;
                let obj = require_object(&ctx.body)?;
                let path = non_empty_str_field(obj, "path")?.to_owned();
                check_access(store.as_ref(), floor, &path, caller_role(&ctx)).await?;
                store.mkdir(&path).await?;
                Ok(HandlerResponse::ok(directory_entry_json(&path)).with_status(201))
            }
        }
    });

    reg.register("deleteFile", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let name = ctx.path_param("store")?.to_owned();
                let (store, floor) = resolve_store(&catalog, &name).await?;
                let obj = require_object(&ctx.body)?;
                let path = non_empty_str_field(obj, "path")?.to_owned();
                check_access(store.as_ref(), floor, &path, caller_role(&ctx)).await?;
                let deleted = store.delete(&path).await?;
                Ok(HandlerResponse::ok(json!({ "deleted": deleted })))
            }
        }
    });

    reg.register("renameFile", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let name = ctx.path_param("store")?.to_owned();
                let (store, floor) = resolve_store(&catalog, &name).await?;
                let obj = require_object(&ctx.body)?;
                let from = non_empty_str_field(obj, "from")?.to_owned();
                let to = non_empty_str_field(obj, "to")?.to_owned();
                // Both ends are checked: reading from a restricted place and
                // writing into one are each things the rule governs, and a move
                // does both.
                let role = caller_role(&ctx);
                check_access(store.as_ref(), floor, &from, role).await?;
                check_access(store.as_ref(), floor, &to, role).await?;
                store.rename(&from, &to).await?;
                Ok(HandlerResponse::ok(entry_json(&renamed_entry(&to))))
            }
        }
    });

    // Served by the `/upload/{store}/{*path}` route rather than a typed endpoint
    // (the endpoint model has no bytes shape), but registered here like any
    // other handler so it reaches the same catalog and the same access rule.
    // Routing around the `EndpointSet` must not mean routing around those.
    reg.register("uploadFile", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let name = ctx.path_param("store")?.to_owned();
                let (store, floor) = resolve_store(&catalog, &name).await?;
                let path = ctx.path_param("path")?.to_owned();
                check_access(store.as_ref(), floor, &path, caller_role(&ctx)).await?;
                let data = ctx.raw_body()?.clone();
                let size = data.len();
                store.write(&path, data).await?;
                Ok(HandlerResponse::ok(file_entry_written_json(&path, size)).with_status(201))
            }
        }
    });

    reg.register("getFileMeta", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let name = ctx.path_param("store")?.to_owned();
                let (store, floor) = resolve_store(&catalog, &name).await?;
                let obj = require_object(&ctx.body)?;
                let path = non_empty_str_field(obj, "path")?.to_owned();
                check_access(store.as_ref(), floor, &path, caller_role(&ctx)).await?;
                let meta = store.get_meta(&path).await?;
                let effective = effective_min_role(store.as_ref(), floor, &path).await?;
                Ok(HandlerResponse::ok(file_meta_json(&path, &meta, effective)))
            }
        }
    });

    reg.register("setFileMeta", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let name = ctx.path_param("store")?.to_owned();
                let (store, floor) = resolve_store(&catalog, &name).await?;
                let obj = require_object(&ctx.body)?;
                let path = non_empty_str_field(obj, "path")?.to_owned();
                check_access(store.as_ref(), floor, &path, caller_role(&ctx)).await?;

                let min_role = optional_role(obj, "min_role")?;
                let attributes = match obj.get("attributes") {
                    Some(Json::Object(o)) => o
                        .iter()
                        .map(|(k, v)| {
                            let text = v.as_str().map(str::to_owned).ok_or_else(|| {
                                Error::invalid(format!("attribute `{k}` must be a string"))
                            })?;
                            Ok((k.clone(), text))
                        })
                        .collect::<Result<BTreeMap<String, String>>>()?,
                    None | Some(Json::Null) => BTreeMap::new(),
                    Some(_) => return Err(Error::invalid("field `attributes` must be an object")),
                };

                let meta = FileMeta {
                    min_role,
                    attributes,
                };
                store.set_meta(&path, &meta).await?;
                // Recomputed after the write, so the response reflects what now
                // applies rather than what was asked for — they differ whenever a
                // parent directory is more restrictive than the rule just set.
                let effective = effective_min_role(store.as_ref(), floor, &path).await?;
                Ok(HandlerResponse::ok(file_meta_json(&path, &meta, effective)))
            }
        }
    });

    // --- applications -------------------------------------------------------

    // --- triggers -----------------------------------------------------------
    // The row ⇄ live-set path (§10.2). Every mutation goes through
    // `save_trigger`/`delete_trigger` — which validate against the *same*
    // registry the dispatcher runs — and then reloads the live set, so the list
    // an admin is looking at is the set that will fire.

    reg.register("listTriggers", {
        let catalog = catalog.clone();
        let apps = apps.clone();
        move |_ctx| {
            let catalog = catalog.clone();
            let apps = apps.clone();
            async move {
                // The **stored** rows, not the live set: a trigger that fails
                // validation is dropped from the live set and would otherwise
                // vanish from the screen that exists to repair it. Its reason
                // comes from the live set's issues, alongside it.
                let stored = list_triggers(&catalog).await?;
                let dispatcher = triggers_of(&apps)?;
                let issues = dispatcher.triggers()?;
                let out: Vec<Json> = stored
                    .iter()
                    .map(|t| {
                        let problem = issues
                            .issues()
                            .iter()
                            .find(|i| i.trigger == t.name)
                            .map(|i| i.problem.clone());
                        trigger_json(t, problem)
                    })
                    .collect();
                Ok(HandlerResponse::ok(Json::Array(out)))
            }
        }
    });

    reg.register("createTrigger", {
        let catalog = catalog.clone();
        let apps = apps.clone();
        move |ctx| {
            let catalog = catalog.clone();
            let apps = apps.clone();
            async move {
                let dispatcher = triggers_of(&apps)?;
                let trigger = trigger_from_body(TriggerId::new(), &ctx.body)?;
                save_trigger(&catalog, &dispatcher.registry(), &trigger).await?;
                // A workflow body gets **version 1** here, not on the editor's
                // first save (§10.3, phase 5.5): an empty workflow with one
                // start step, so a new workflow opens on a canvas rather than on
                // "no version of this has been saved yet", which is not a good
                // first thing for an admin to be told about something they have
                // just created.
                if trigger.body == TriggerBody::Workflow {
                    sc_workflow::save_workflow(
                        &catalog,
                        &sc_workflow::Workflow::empty(trigger.id),
                        "created",
                        ctx.user.as_ref().map(|u| u.id),
                    )
                    .await?;
                }
                dispatcher.reload(&catalog).await?;
                Ok(HandlerResponse::ok(trigger_json(&trigger, None)).with_status(201))
            }
        }
    });

    reg.register("updateTrigger", {
        let catalog = catalog.clone();
        let apps = apps.clone();
        move |ctx| {
            let catalog = catalog.clone();
            let apps = apps.clone();
            async move {
                let dispatcher = triggers_of(&apps)?;
                let id = parse_trigger_id(ctx.path_param("id")?)?;
                if load_trigger(&catalog, id).await?.is_none() {
                    return Err(Error::not_found(format!("no trigger with id {id}")));
                }
                // The id is the path's, not the body's — the row's identity is
                // not something a payload gets to reassign.
                let trigger = trigger_from_body(id, &ctx.body)?;
                save_trigger(&catalog, &dispatcher.registry(), &trigger).await?;
                // Switching an action body to a workflow one is the other way a
                // trigger comes to need a version 1 (§10.3, phase 5.5), and it
                // gets the same empty canvas a create does. Only when it has
                // none: a trigger switched to an action and back finds the
                // program it had, because the versions were never deleted.
                if trigger.body == TriggerBody::Workflow
                    && sc_workflow::max_version(&catalog, id).await?.is_none()
                {
                    sc_workflow::save_workflow(
                        &catalog,
                        &sc_workflow::Workflow::empty(id),
                        "created",
                        ctx.user.as_ref().map(|u| u.id),
                    )
                    .await?;
                }
                dispatcher.reload(&catalog).await?;
                Ok(HandlerResponse::ok(trigger_json(&trigger, None)))
            }
        }
    });

    reg.register("deleteTrigger", {
        let catalog = catalog.clone();
        let apps = apps.clone();
        move |ctx| {
            let catalog = catalog.clone();
            let apps = apps.clone();
            async move {
                let dispatcher = triggers_of(&apps)?;
                let id = parse_trigger_id(ctx.path_param("id")?)?;
                if !delete_trigger(&catalog, id).await? {
                    return Err(Error::not_found(format!("no trigger with id {id}")));
                }
                // Append-only is a rule about *editing* a workflow, not a promise
                // that a deleted trigger leaves rows nobody can reach: the
                // versions are keyed by this id and there is now nothing to
                // address them by.
                sc_workflow::delete_workflow_versions(&catalog, id).await?;
                dispatcher.reload(&catalog).await?;
                Ok(HandlerResponse::ok(json!({ "deleted": true })))
            }
        }
    });

    reg.register("runTrigger", {
        let catalog = catalog.clone();
        let apps = apps.clone();
        move |ctx| {
            let catalog = catalog.clone();
            let apps = apps.clone();
            async move {
                let dispatcher = triggers_of(&apps)?;
                let id = parse_trigger_id(ctx.path_param("id")?)?;
                // By id, then by name: the row is what the admin clicked, and
                // resolving it here means a rename cannot make the button run
                // somebody else's trigger.
                let trigger = load_trigger(&catalog, id)
                    .await?
                    .ok_or_else(|| Error::not_found(format!("no trigger with id {id}")))?;
                // The posted body is the event's payload, and the caller is the
                // admin who pressed the button. A failing action comes back as
                // an error — a 200 carrying a hidden failure is the thing a
                // "test this now" button exists to prevent.
                let caller = admin_caller(ctx.user.as_ref());
                let result = dispatcher
                    .run_trigger(&catalog, &trigger.name, ctx.body.clone(), Some(&caller))
                    .await?;
                Ok(HandlerResponse::ok(json!({ "result": result })))
            }
        }
    });

    reg.register("listActions", {
        let apps = apps.clone();
        let catalog = catalog.clone();
        move |ctx| {
            let apps = apps.clone();
            let catalog = catalog.clone();
            async move {
                let dispatcher = triggers_of(&apps)?;
                // The table the trigger being edited fires on, when it has one.
                // An action's declaration may depend on it — `send_email`'s
                // attachment checkboxes are that table's File fields — and every
                // other action ignores it.
                let channel = ctx
                    .query_get("table")
                    .map(str::trim)
                    .filter(|table| !table.is_empty());
                let out: Vec<Json> = dispatcher
                    .registry()
                    .all()
                    .map(|action| {
                        let spec = action.config_spec_for(&catalog, channel);
                        json!({
                            "name": action.name(),
                            "description": action.description(),
                            "config_spec": spec
                                .iter()
                                .map(form_field_json)
                                .collect::<Vec<_>>(),
                            // Whether the step palette may offer it here (§10.3,
                            // phase 5.4). Decided from the declaration this same
                            // call is answering with, so the flag and the form
                            // the client would render cannot disagree.
                            "workflow_step": sc_workflow::usable_as_step(&spec),
                        })
                    })
                    .collect();
                Ok(HandlerResponse::ok(Json::Array(out)))
            }
        }
    });

    // --- workflows ----------------------------------------------------------
    // The program half of a trigger whose body is a workflow (§10.3). Addressed
    // by the **trigger's** id, because that is what a workflow is identified by:
    // decision 1 made it a body rather than an entity, and inventing a second id
    // here would be inventing the entity back.
    //
    // Every read validates, and every write validates before it stores
    // (decision 11). A workflow that does not validate is still returned, still
    // listed and still editable — with its issues beside it, because editing it
    // is the repair — and what it cannot do is start a run.

    reg.register("getWorkflow", {
        let catalog = catalog.clone();
        let apps = apps.clone();
        move |ctx| {
            let catalog = catalog.clone();
            let apps = apps.clone();
            async move {
                let dispatcher = triggers_of(&apps)?;
                let trigger = require_workflow_trigger(&catalog, ctx.path_param("id")?).await?;
                let versions = sc_workflow::list_workflow_versions(&catalog, trigger.id).await?;
                // The newest version is the current one; a trigger created
                // before its first save has none, which is a real state and is
                // answered as an empty canvas rather than as an error.
                let workflow = match versions.first() {
                    Some(version) => version.workflow.clone(),
                    None => sc_workflow::Workflow::empty(trigger.id),
                };
                Ok(HandlerResponse::ok(
                    workflow_json(&catalog, &dispatcher, &trigger, &workflow, &versions).await?,
                ))
            }
        }
    });

    reg.register("saveWorkflow", {
        let catalog = catalog.clone();
        let apps = apps.clone();
        move |ctx| {
            let catalog = catalog.clone();
            let apps = apps.clone();
            async move {
                let dispatcher = triggers_of(&apps)?;
                let trigger = require_workflow_trigger(&catalog, ctx.path_param("id")?).await?;
                let obj = require_object(&ctx.body)?;
                let posted = obj
                    .get("workflow")
                    .ok_or_else(|| Error::invalid("field `workflow` is required"))?;
                let workflow = workflow_from_document(trigger.id, posted)?;
                // Refused *before* it is stored, with the whole list of problems
                // rather than the first: an admin told about one of three fixes
                // it and is told about the next one, which is three round trips
                // to save one workflow.
                sc_workflow::validate_workflow(
                    &catalog,
                    &dispatcher.registry(),
                    &workflow,
                    trigger.channel.as_deref(),
                )
                .await?;
                let saved = sc_workflow::save_workflow(
                    &catalog,
                    &workflow,
                    &optional_str(obj, "description"),
                    ctx.user.as_ref().map(|u| u.id),
                )
                .await?;
                let versions = sc_workflow::list_workflow_versions(&catalog, trigger.id).await?;
                Ok(HandlerResponse::ok(
                    workflow_json(&catalog, &dispatcher, &trigger, &saved, &versions).await?,
                ))
            }
        }
    });

    // Revert: a **new** version whose steps are an old one's. Not a rewrite, and
    // not a deletion of the versions in between — a run suspended on any of them
    // still has to be able to load the one it is pinned to tomorrow, which is
    // the whole reason the table is append-only (decision 2).
    reg.register("revertWorkflow", {
        let catalog = catalog.clone();
        let apps = apps.clone();
        move |ctx| {
            let catalog = catalog.clone();
            let apps = apps.clone();
            async move {
                let dispatcher = triggers_of(&apps)?;
                let trigger = require_workflow_trigger(&catalog, ctx.path_param("id")?).await?;
                let obj = require_object(&ctx.body)?;
                let version = obj
                    .get("version")
                    .and_then(Json::as_u64)
                    .ok_or_else(|| Error::invalid("field `version` must be a version number"))?;
                let version = u32::try_from(version)
                    .map_err(|_| Error::invalid(format!("there is no version {version}")))?;
                let old =
                    sc_workflow::require_workflow_version(&catalog, trigger.id, version).await?;
                // Checked again on the way back in: a version that was fine when
                // it was saved may name a table that has since been dropped, and
                // restoring it silently would restore a workflow that cannot run.
                sc_workflow::validate_workflow(
                    &catalog,
                    &dispatcher.registry(),
                    &old,
                    trigger.channel.as_deref(),
                )
                .await?;
                let description = match optional_str(obj, "description") {
                    given if given.is_empty() => format!("reverted to version {version}"),
                    given => given,
                };
                let saved = sc_workflow::save_workflow(
                    &catalog,
                    &old,
                    &description,
                    ctx.user.as_ref().map(|u| u.id),
                )
                .await?;
                let versions = sc_workflow::list_workflow_versions(&catalog, trigger.id).await?;
                Ok(HandlerResponse::ok(
                    workflow_json(&catalog, &dispatcher, &trigger, &saved, &versions).await?,
                ))
            }
        }
    });

    reg.register("listWorkflowRuns", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let trigger = require_workflow_trigger(&catalog, ctx.path_param("id")?).await?;
                let state = match ctx
                    .query_get("state")
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    Some(state) => Some(sc_agent::RunState::parse(state)?),
                    None => None,
                };
                let runs = sc_workflow::list_workflow_runs(
                    &catalog,
                    &trigger.name,
                    state,
                    query_count(&ctx, "limit", DEFAULT_RUN_PAGE),
                    query_count(&ctx, "offset", 0),
                )
                .await?;
                let out: Vec<Json> = runs.iter().map(run_summary_json).collect();
                Ok(HandlerResponse::ok(Json::Array(out)))
            }
        }
    });

    // --- the three things an admin does to a run ----------------------------

    reg.register("resumeRun", {
        let catalog = catalog.clone();
        let apps = apps.clone();
        move |ctx| {
            let catalog = catalog.clone();
            let apps = apps.clone();
            async move {
                let dispatcher = triggers_of(&apps)?;
                let id = sc_agent::RunId(parse_uuid(ctx.path_param("id")?, "run")?);
                let run = sc_agent::require_run(&catalog, id).await?;
                // The step's own floor, checked here rather than assumed
                // (§10.3, phase 4.3). Today every caller that reaches this is an
                // admin and meets every floor; the check is still made, because
                // a form exposed to a role is exactly the thing that must not be
                // reachable by way of a surface that forgot to ask.
                if let Some(form) = sc_workflow::run_pending_form(&run)? {
                    sc_workflow::check_may_resume(&form, caller_role(&ctx))?;
                }
                let values = match &ctx.body {
                    Json::Null => sc_types::Attrs::new(),
                    body => require_object(body)?.clone().into_iter().collect(),
                };
                let run = sc_workflow::resume_run(
                    &catalog,
                    &dispatcher,
                    &sc_workflow::SystemClock,
                    id,
                    values,
                )
                .await?;
                Ok(HandlerResponse::ok(
                    run_json_with_workflow(&catalog, &run).await?,
                ))
            }
        }
    });

    reg.register("cancelRun", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let id = sc_agent::RunId(parse_uuid(ctx.path_param("id")?, "run")?);
                let reason = match &ctx.body {
                    Json::Null => None,
                    body => Some(optional_str(require_object(body)?, "reason").to_owned()),
                };
                let run = sc_workflow::cancel_run(
                    &catalog,
                    &sc_workflow::SystemClock,
                    id,
                    reason.as_deref(),
                )
                .await?;
                Ok(HandlerResponse::ok(
                    run_json_with_workflow(&catalog, &run).await?,
                ))
            }
        }
    });

    reg.register("retryRun", {
        let catalog = catalog.clone();
        let apps = apps.clone();
        move |ctx| {
            let catalog = catalog.clone();
            let apps = apps.clone();
            async move {
                let dispatcher = triggers_of(&apps)?;
                let id = sc_agent::RunId(parse_uuid(ctx.path_param("id")?, "run")?);
                let run =
                    sc_workflow::retry_run(&catalog, &dispatcher, &sc_workflow::SystemClock, id)
                        .await?;
                Ok(HandlerResponse::ok(
                    run_json_with_workflow(&catalog, &run).await?,
                ))
            }
        }
    });

    // --- settings -----------------------------------------------------------
    //
    // `_sc_config` (§9), rendered from its declarations: the response carries
    // the sections and their fields alongside the values, so the screen is
    // generic over what a setting is — the same arrangement the file-store and
    // provider forms have. Two things happen *here* rather than in the store:
    // secrets are redacted on the way out and the sentinel merged back on the
    // way in (§11.1), and a certificate is parsed before it is saved, because a
    // key that does not match its chain must fail in front of the admin rather
    // than at the next restart (§13.5).
    reg.register("getSettings", {
        let catalog = catalog.clone();
        move |_ctx| {
            let catalog = catalog.clone();
            async move { Ok(HandlerResponse::ok(settings_json(&catalog).await?)) }
        }
    });

    reg.register("updateSettings", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let submitted = match ctx.body.get("values") {
                    Some(Json::Object(values)) => values.clone(),
                    Some(Json::Null) | None => Attrs::new(),
                    Some(other) => {
                        return Err(Error::invalid(format!(
                            "`values` should be an object of settings, got {other}"
                        )));
                    }
                };
                let stored = sc_config::all_config(&catalog).await?;
                let spec = sc_config::config_spec();
                // The secret the form sent back untouched is the sentinel it was
                // shown, not the key: restore what is stored.
                let values = sc_types::merge_secrets(&spec, &stored, &submitted);

                // What the settings *mean together* is checked before anything
                // is written: `custom` with no certificate, or a certificate its
                // key does not match, is refused as one act rather than saved as
                // half a configuration.
                let merged = {
                    let mut merged = stored.clone();
                    for (key, value) in &values {
                        merged.insert(key.clone(), value.clone());
                    }
                    merged
                };
                let ssl = sc_config::ssl_settings_from(&merged)?;
                ssl.check()?;
                if ssl.mode == sc_config::SslMode::Custom {
                    crate::tls::check_certificate(&ssl.certificate, &ssl.private_key)?;
                }
                // The email section's own cross-field rules, on the same
                // footing: a username with no password, or credentials over an
                // unencrypted connection, is refused here rather than at the
                // first message that fails to send. Reading is the check —
                // `from_config` runs it — so the result is dropped.
                sc_config::EmailSettings::from_config(&merged)?;
                // The Development section's own reading, on the same footing: a
                // verbosity that is not a level is refused here rather than
                // stored and discovered at the next boot.
                let development = sc_config::development_settings_from(&merged)?;

                sc_config::set_config_many(&catalog, &values).await?;
                // The two switches this process runs under move **now**, not at
                // the next restart: an admin ticks "Log SQL" precisely because
                // something is happening in the server they are looking at, and
                // a switch that needed a restart would have thrown that away.
                development.apply();
                Ok(HandlerResponse::ok(settings_json(&catalog).await?))
            }
        }
    });

    // One message through the **stored** email settings, so an admin can find
    // out whether they work while they are still on the screen that sets them.
    //
    // Everything about it is deliberate. The settings come from `_sc_config`
    // rather than the request, so this tests what is saved and the note beside
    // the button says to save first. The recipient defaults to the signed-in
    // admin's own address, because they are the one person who can go and look.
    // And the transport's error is returned **verbatim**: "connection refused",
    // "authentication failed" and "relay access denied" are three different
    // problems with three different fixes, and the whole value of a test button
    // is learning which one you have.
    reg.register("sendTestEmail", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let settings = sc_config::EmailSettings::load(&catalog).await?.ok_or_else(|| {
                    Error::invalid(
                        "no SMTP server is configured: set an SMTP host in Settings → Email and \
                         save before sending a test message",
                    )
                })?;

                // The signed-in admin's own address, unless they named one.
                let to = match ctx.body.get("to") {
                    Some(Json::String(s)) if !s.trim().is_empty() => s.trim().to_owned(),
                    _ => ctx
                        .user
                        .as_ref()
                        .and_then(|user| user.get(COL_EMAIL))
                        .and_then(Value::as_text)
                        .map(str::to_owned)
                        .ok_or_else(|| {
                            Error::invalid(
                                "there is nobody to send the test message to: give an address, \
                                 or set one on your own account",
                            )
                        })?,
                };
                let recipients = sc_email::parse_recipients(&to)?;

                let mailer = sc_email::SmtpMailer::new(&settings)?;
                let mut email = sc_email::Email::new(settings.from.clone());
                email.to = recipients;
                email.subject = "Saltcorn test message".to_owned();
                // Both bodies, because both are what a real message will carry
                // and a transport that mangles `multipart/alternative` should
                // fail here rather than on the first receipt.
                email.text = Some(TEST_EMAIL_TEXT.to_owned());
                email.html = Some(TEST_EMAIL_HTML.to_owned());
                mailer.send(&email).await?;

                Ok(HandlerResponse::ok(json!({ "sent_to": to })))
            }
        }
    });

    // --- backup & restore ---------------------------------------------------
    // Four handlers for one screen, split by what crosses the wire rather than by
    // what they do (§16): the *choice* is JSON and typed, the *archive* is bytes
    // and is not. `createBackup` and `uploadBackup` are reached by routes outside
    // the endpoint set (see `crate::router`) and are in this registry all the same,
    // so all four are behind the same admin check and the same catalog.
    //
    // The uploaded archive waits in `pending` between the upload and the restore,
    // because what to restore cannot be chosen until the file has been read.

    let pending = crate::backup::PendingUploads::new();

    // What this server has to offer, plus the selection the admin last made — so
    // the dialog opens on their tuned choice rather than on everything, and on
    // *everything* the first time.
    reg.register("getBackupOptions", {
        let catalog = catalog.clone();
        move |_ctx| {
            let catalog = catalog.clone();
            async move {
                let available = crate::backup::available(&catalog).await?;
                let preferences = stored_backup_preferences(&catalog).await?;
                Ok(HandlerResponse::ok(json!({
                    "available": available.to_json(),
                    "include": preferences.selection(&available).to_json(),
                })))
            }
        }
    });

    // Build the archive, and remember what was asked for.
    //
    // The selection is persisted **here**, on the way to producing a file, rather
    // than by a save button of its own: the admin's answer to "what should a backup
    // include" is exactly the backup they just took, and a screen that made them
    // state it twice would drift.
    reg.register("createBackup", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let available = crate::backup::available(&catalog).await?;
                let selection = match ctx.body.get("include") {
                    Some(value) => crate::backup::Selection::from_json(value)?,
                    // No selection at all means everything, which is the default
                    // the screen starts from and what a script with no opinion
                    // should get.
                    None => crate::backup::Selection::everything(&available),
                };
                let previous = stored_backup_preferences(&catalog).await?;
                let preferences =
                    crate::backup::BackupPreferences::of(&previous, &available, &selection);
                sc_config::set_config(&catalog, sc_config::BACKUP_INCLUDE, preferences.to_json())
                    .await?;

                let bytes = crate::backup::write_backup(&catalog, &selection).await?;
                Ok(HandlerResponse::download(crate::handler::Download {
                    bytes: Bytes::from(bytes),
                    content_type: "application/zip".to_owned(),
                    filename: backup_filename(),
                }))
            }
        }
    });

    // Take delivery of a file and say what is in it. Nothing is written.
    reg.register("uploadBackup", {
        let pending = pending.clone();
        move |ctx| {
            let pending = pending.clone();
            async move {
                let bytes = ctx.raw_body()?.clone();
                let (contents, manifest) = crate::backup::inspect(&bytes)?;
                let id = pending.keep(bytes)?;
                Ok(HandlerResponse::ok(json!({
                    "id": id.to_string(),
                    "created_at": manifest.get("created_at"),
                    "available": contents.to_json(),
                    // Everything the file holds, ticked: the admin excludes from
                    // there, which is the same direction the backup dialog works
                    // in.
                    "include": crate::backup::Selection::everything(&contents).to_json(),
                })))
            }
        }
    });

    reg.register("restoreBackup", {
        let catalog = catalog.clone();
        let apps = apps.clone();
        let pending = pending.clone();
        move |ctx| {
            let catalog = catalog.clone();
            let apps = apps.clone();
            let pending = pending.clone();
            async move {
                let obj = require_object(&ctx.body)?;
                let id = parse_uuid(non_empty_str_field(obj, "id")?, "uploaded backup")?;
                let selection = match obj.get("include") {
                    Some(value) => crate::backup::Selection::from_json(value)?,
                    None => {
                        return Err(Error::invalid(
                            "`include` must say what to restore from the backup",
                        ));
                    }
                };
                let bytes = pending.take(id)?;
                let report =
                    crate::backup::restore_backup(&catalog, &apps, &bytes, &selection).await?;
                // A restore can create tables and stores an application's API is
                // projected from, so the mounted apps are re-projected once at the
                // end — the same thing a schema change through the admin API does.
                reproject_apps(&apps);
                Ok(HandlerResponse::ok(json!({
                    "restored": report.restored,
                    "warnings": report.warnings,
                })))
            }
        }
    });

    reg.register("listApplications", {
        let catalog = catalog.clone();
        move |_ctx| {
            let catalog = catalog.clone();
            async move {
                let apps = list_applications(&catalog).await?;
                let out: Vec<Json> = apps.iter().map(application_json).collect();
                Ok(HandlerResponse::ok(Json::Array(out)))
            }
        }
    });

    reg.register("createApplication", {
        let catalog = catalog.clone();
        let apps = apps.clone();
        move |ctx| {
            let catalog = catalog.clone();
            let apps = apps.clone();
            async move {
                // A create mints a fresh id; the body carries everything else.
                // What comes back from the save is what was *stored*: a custom
                // SQL query is described on the way in, so the response carries
                // the result columns the database reported rather than the empty
                // list the caller sent.
                let app =
                    save_application(&catalog, &application_from_body(AppId::new(), &ctx.body)?)
                        .await?;
                // A `react` app's project is the server's to create (§2.3): this
                // is the step that removes the SSH requirement, so it happens on
                // the first save rather than waiting for an admin to ask. It is
                // deliberately *not* fatal to the create — the row is already
                // saved and valid, and an unwritable store or an occupied
                // directory is a thing the admin fixes and re-tries, not a reason
                // to lose the application they just configured.
                let mut body = application_json(&app);
                // The scaffolded project's generated client is typed against the
                // app's endpoints, exposed triggers included, so the scaffold
                // resolves them against the same live set a mount would.
                let scaffold = scaffold_new_app(&catalog, &app, apps.triggers()).await;
                // ...and the agent that will build it, which its framework
                // declares (§13.3). After the scaffold, deliberately: the agent is
                // pointed at the project the scaffold just wrote. Non-fatal for
                // the same reason — an application with no builder agent is one
                // the admin creates an agent for, not one that failed to exist.
                let agent = create_builder_agent(&catalog, &apps, &app).await;
                if let Some(obj) = body.as_object_mut() {
                    match &scaffold {
                        Ok(Some(report)) => {
                            obj.insert("scaffolded".to_owned(), json!(report.summary()));
                        }
                        Ok(None) => {}
                        Err(e) => {
                            obj.insert("scaffold_error".to_owned(), json!(e.causes()));
                        }
                    }
                    match &agent {
                        Ok(Some(name)) => {
                            obj.insert("agent".to_owned(), json!(name));
                        }
                        Ok(None) => {}
                        Err(e) => {
                            obj.insert("agent_error".to_owned(), json!(e.causes()));
                        }
                    }
                }
                Ok(HandlerResponse::ok(body).with_status(201))
            }
        }
    });

    reg.register("updateApplication", {
        let catalog = catalog.clone();
        let apps = apps.clone();
        move |ctx| {
            let catalog = catalog.clone();
            let apps = apps.clone();
            async move {
                let id = parse_app_id(ctx.path_param("id")?)?;
                if load_application(&catalog, id).await?.is_none() {
                    return Err(Error::not_found(format!("no application with id {id}")));
                }
                // The id is the path's, not the body's — the row's identity is not
                // something a payload gets to reassign.
                let app =
                    save_application(&catalog, &application_from_body(id, &ctx.body)?).await?;
                // A save is an API-definition change: a table added to the
                // subset, an endpoint's role, a custom query. The generated
                // client has to describe what the app now serves (decision 10),
                // so it is rewritten here — logged and never fatal, because the
                // application is already saved and an unreachable store is not a
                // reason to report that it was not.
                reemit_app_client(&catalog, &app, apps.triggers()).await;
                Ok(HandlerResponse::ok(application_json(&app)))
            }
        }
    });

    reg.register("deleteApplication", {
        let catalog = catalog.clone();
        let apps = apps.clone();
        move |ctx| {
            let catalog = catalog.clone();
            let apps = apps.clone();
            async move {
                let id = parse_app_id(ctx.path_param("id")?)?;
                // Load first so we know the subdomain to unmount — deleting the row
                // is the definition's end, unmounting is what stops it serving.
                let existing = load_application(&catalog, id).await?;
                let deleted = delete_application(&catalog, id).await?;
                if let Some(app) = &existing {
                    apps.unmount(&app.subdomain);
                }
                if !deleted {
                    return Err(Error::not_found(format!("no application with id {id}")));
                }
                // The agent created to build it goes with it (§13.3). An
                // application's builder has nothing to build once the application
                // is gone — it would sit in the agents list as a broken record of
                // something that no longer exists, and the admin who deleted the
                // application is the one who would have to clean it up.
                let mut body = json!({ "deleted": true });
                if let Some(app) = &existing
                    && let Some(name) = delete_builder_agent(&catalog, app).await?
                    && let Some(obj) = body.as_object_mut()
                {
                    obj.insert("agent".to_owned(), json!(name));
                }
                Ok(HandlerResponse::ok(body))
            }
        }
    });

    reg.register("buildApplication", {
        let catalog = catalog.clone();
        let apps = apps.clone();
        move |ctx| {
            let catalog = catalog.clone();
            let apps = apps.clone();
            async move {
                let id = parse_app_id(ctx.path_param("id")?)?;
                let app = load_application(&catalog, id)
                    .await?
                    .ok_or_else(|| Error::not_found(format!("no application with id {id}")))?;
                // Build and mount live. A build failure propagates as an
                // Application error (§16) whose message is the bundler's own
                // diagnostics, and leaves any previously mounted version serving.
                let report = build_and_mount(&apps, app).await?;
                Ok(HandlerResponse::ok(json!({
                    "built": true,
                    "git_repo": report.git_repo,
                    "log": build_log(&report),
                })))
            }
        }
    });

    // Rewrite an application's generated code on demand, without building.
    //
    // The button beside "Build", and deliberately a different button: a build
    // runs a bundler and can take a minute, while this writes four files and
    // cannot fail on anything but the store. It is the same call the automatic
    // path makes when a table changes (decision 10), so an admin who wants it
    // now and an admin who changed a column get the same files.
    reg.register("updateApplicationClient", {
        let catalog = catalog.clone();
        let apps = apps.clone();
        move |ctx| {
            let catalog = catalog.clone();
            let apps = apps.clone();
            async move {
                let id = parse_app_id(ctx.path_param("id")?)?;
                let app = load_application(&catalog, id)
                    .await?
                    .ok_or_else(|| Error::not_found(format!("no application with id {id}")))?;
                let update = update_app_client(&catalog, &app, apps.triggers()).await?;
                Ok(HandlerResponse::ok(json!({
                    "scaffolded": matches!(update, sc_app::ClientUpdate::Scaffolded(_)),
                    "files": update.files(),
                    "log": update.summary(),
                })))
            }
        }
    });

    // The admin UI's GraphQL explorer (§13.4): one operation, run against the
    // application's **mounted** GraphQL provider.
    //
    // Everything this handler does is find that provider and hand it the body.
    // The document is executed by `ApiProvider::handle` — the same entry point a
    // request to `staff.example.com/graphql` reaches — so the schema, the
    // limits, the row layer and the §7 authorization are not restated here and
    // cannot drift from what the application serves. The one thing this handler
    // chooses is *who is asking*, and it chooses `ctx.user`: the signed-in
    // admin, with their own role and their own ownership. An explorer holding
    // authority the person driving it does not have would be a way to read rows
    // through a screen that were refused through the API.
    //
    // A GraphQL endpoint answers `200` with its errors in the body, so the
    // provider's response — status and all — is passed through as it stands
    // rather than being re-judged here.
    reg.register("runApplicationGraphql", {
        let catalog = catalog.clone();
        let apps = apps.clone();
        move |ctx| {
            let catalog = catalog.clone();
            let apps = apps.clone();
            async move {
                let id = parse_app_id(ctx.path_param("id")?)?;
                let app = load_application(&catalog, id)
                    .await?
                    .ok_or_else(|| Error::not_found(format!("no application with id {id}")))?;
                // The *mounted* app, not the stored row: an application whose
                // record enables GraphQL but which has never been built serves
                // nothing, and "no such provider" would be a misleading way to
                // say so.
                let mounted = apps.get(&app.subdomain).ok_or_else(|| {
                    Error::invalid(format!(
                        "application `{}` is not mounted, so it serves no GraphQL yet — build it \
                         first",
                        app.name
                    ))
                })?;
                let provider = mounted
                    .providers
                    .iter()
                    .find(|p| p.name() == GRAPHQL_PROVIDER)
                    .ok_or_else(|| {
                        Error::invalid(format!(
                            "application `{}` does not enable the `{GRAPHQL_PROVIDER}` API \
                             provider",
                            app.name
                        ))
                    })?;
                let req = ApiRequest {
                    method: ApiMethod::Post,
                    // The provider routes on its own mount, so this is that
                    // mount and never a path a request chose.
                    path: provider.mount(),
                    query: Vec::new(),
                    body: ctx.body.clone(),
                    raw: None,
                };
                let resp = provider.handle(req, &catalog, ctx.user.as_ref()).await?;
                Ok(HandlerResponse::ok(resp.body).with_status(resp.status))
            }
        }
    });

    // --- frameworks ---------------------------------------------------------

    // Server-query options are resolved here, so the UI receives a concrete list
    // and needs no query evaluator of its own (§1.6). That is what let the
    // `store` setting become a pick-list without waiting for `ui/form-runtime`.
    reg.register("listFrameworks", {
        let catalog = catalog.clone();
        move |_ctx| {
            let catalog = catalog.clone();
            async move {
                let mut out = Vec::new();
                // In registry order, which is itself editorial: the framework an
                // admin should take comes first (§2.2), and each carries the
                // label and sentence the picker shows — so the screen presents
                // two very different propositions while knowing neither (§2.4).
                for info in registered_framework_info() {
                    let spec =
                        resolve_options(&catalog, framework_config_spec(&info.name)?).await?;
                    out.push(json!({
                        "name": info.name,
                        "label": info.label,
                        "description": info.description,
                        "config_spec": spec.iter().map(form_field_json).collect::<Vec<_>>(),
                    }));
                }
                Ok(HandlerResponse::ok(Json::Array(out)))
            }
        }
    });

    // --- API providers ------------------------------------------------------

    // The names an application may enable, so the form offers them rather than
    // asking the admin to remember them. This is the *same* list
    // `app_providers_with` switches on, so a name offered here is a name that
    // mounts — which is the whole point of listing them from the server.
    reg.register("listApiProviders", |_ctx| async move {
        let out: Vec<Json> = registered_api_provider_info()
            .into_iter()
            .map(|info| {
                json!({
                    "name": info.name,
                    "label": info.label,
                    "description": info.description,
                    "default_mount": info.default_mount,
                    // The provider's own settings, in the same vocabulary a
                    // framework's arrive in — so the application form renders
                    // GraphQL's aggregation switch and its four bounds without
                    // knowing that GraphQL is what it is rendering.
                    "config_spec": info.config_spec.iter().map(form_field_json).collect::<Vec<_>>(),
                    // …and whether it takes custom SQL queries, which are not a
                    // settings field and so have an editor of their own.
                    "supports_custom_queries": info.supports_custom_queries,
                })
            })
            .collect();
        Ok(HandlerResponse::ok(Json::Array(out)))
    });

    // Prepare one custom SQL query and answer with the columns the database says
    // it returns — the editor's "Check" button (§13.4).
    //
    // The same two calls a save makes, in the same order: everything decidable
    // without a database first (the name, the path, one statement, the declared
    // parameters and the used ones being the same set), then `describe`, which
    // is where Postgres's own message comes from. Deciding it twice would be two
    // answers to "is this query valid", and the one that is wrong is the one
    // nobody is reading until a caller hits it.
    reg.register("describeCustomQuery", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let obj = require_object(&ctx.body)?;
                let tables = parse_str_array(obj, "tables")?;
                let query: sc_api::CustomQuery = serde_json::from_value(ctx.body.clone())
                    .map_err(|e| Error::invalid(format!("not a custom SQL query: {e}")))?;
                sc_api::validate_custom_queries(std::slice::from_ref(&query), &tables)?;
                let columns = sc_api::describe_custom_query(&catalog, &query).await?;
                Ok(HandlerResponse::ok(json!({
                    "columns": columns
                        .iter()
                        .map(|c| json!({ "name": c.name, "type": c.ty }))
                        .collect::<Vec<_>>(),
                })))
            }
        }
    });

    // --- users --------------------------------------------------------------

    reg.register("listUsers", {
        let catalog = catalog.clone();
        move |_ctx| {
            let catalog = catalog.clone();
            async move {
                // `SELECT *`, not the three columns it used to be: the users
                // table is the one table an admin adds their own columns to
                // (§7.1), and the users screen is where they expect to see them.
                // `User::from_row` drops the password hash on the way past, so
                // "everything" still never includes that.
                let mut select =
                    Select::from(Source::table(USERS_TABLE)).columns(vec![Projection::all()]);
                select.order = vec![OrderBy::asc(Expr::col(COL_EMAIL))];
                let rows = catalog
                    .primary()
                    .query(&Statement::from(select))
                    .await?
                    .try_collect()
                    .await?;
                let mut out = Vec::with_capacity(rows.len());
                for row in &rows {
                    out.push(user_row_json(&User::from_row(row)?));
                }
                Ok(HandlerResponse::ok(Json::Array(out)))
            }
        }
    });

    reg.register("createUser", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let obj = require_object(&ctx.body)?;
                let users = catalog.require(USERS_TABLE)?;
                let created = create_user_with(
                    &catalog,
                    NewUser {
                        email: non_empty_str_field(obj, "email")?.to_owned(),
                        // Absent or blank asks for a generated password, which
                        // comes back in this response and nowhere else.
                        password: optional_str(obj, "password"),
                        role: user_role_field(obj)?,
                        extra: user_extra_values(&users, obj)?,
                    },
                )
                .await?;
                Ok(HandlerResponse::ok(json!({
                    "user": user_row_json(&created.user),
                    "generated_password": created.generated_password,
                }))
                .with_status(201))
            }
        }
    });

    reg.register("updateUser", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let id = parse_user_id(ctx.path_param("id")?)?;
                let obj = require_object(&ctx.body)?;
                let users = catalog.require(USERS_TABLE)?;
                let user = update_user(
                    &catalog,
                    id,
                    UserUpdate {
                        email: Some(non_empty_str_field(obj, "email")?.to_owned()),
                        role: Some(user_role_field(obj)?),
                        // Blank here means "leave it alone" — the form's password
                        // box is empty because the admin is not changing it, not
                        // because they want the account to have no password.
                        password: Some(optional_str(obj, "password")).filter(|p| !p.is_empty()),
                        extra: user_extra_values(&users, obj)?,
                    },
                )
                .await?;
                Ok(HandlerResponse::ok(user_row_json(&user)))
            }
        }
    });

    reg.register("deleteUser", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let id = parse_user_id(ctx.path_param("id")?)?;
                refuse_self(&ctx, id, "delete")?;
                if !delete_user(&catalog, id).await? {
                    return Err(Error::not_found(format!("no user with id {id}")));
                }
                // The account is gone, so the sessions holding it must go with
                // it — not because they would still work (a session resolves by
                // reading the user, and there is no user), but because leaving
                // rows that resolve to nobody is leaving litter.
                Ok(HandlerResponse::end_user_sessions(
                    id,
                    json!({ "deleted": true }),
                ))
            }
        }
    });

    reg.register("setUserDisabled", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let id = parse_user_id(ctx.path_param("id")?)?;
                let obj = require_object(&ctx.body)?;
                let disabled = bool_field(obj, "disabled")?;
                if disabled {
                    refuse_self(&ctx, id, "disable")?;
                }
                let user = set_user_disabled(&catalog, id, disabled).await?;
                let body = user_row_json(&user);
                // Disabling stops them signing in again; ending the sessions is
                // what stops the one they are in the middle of.
                Ok(match disabled {
                    true => HandlerResponse::end_user_sessions(id, body),
                    false => HandlerResponse::ok(body),
                })
            }
        }
    });

    reg.register("forceLogoutUser", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let id = parse_user_id(ctx.path_param("id")?)?;
                // Read it first, so forcing a logout on somebody who is not there
                // is a 404 rather than a cheerful "ended 0 sessions".
                if load_user(&catalog, id).await?.is_none() {
                    return Err(Error::not_found(format!("no user with id {id}")));
                }
                Ok(HandlerResponse::end_user_sessions(
                    id,
                    json!({ "ok": true }),
                ))
            }
        }
    });

    reg.register("becomeUser", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let id = parse_user_id(ctx.path_param("id")?)?;
                let user = load_user(&catalog, id)
                    .await?
                    .ok_or_else(|| Error::not_found(format!("no user with id {id}")))?;
                // A disabled account cannot be signed into by its owner, so it
                // cannot be signed into over their head either.
                if user.is_disabled() {
                    return Err(Error::invalid(
                        "that account is disabled; enable it before becoming it",
                    ));
                }
                let body = user_summary_json(&user);
                // A swap, not an addition: starting a session ends the one the
                // request arrived with, so the admin session does not stay live
                // behind the one it just became.
                Ok(HandlerResponse::start_session(user, body))
            }
        }
    });

    reg.register("setRandomPassword", {
        let catalog = catalog.clone();
        move |ctx| {
            let catalog = catalog.clone();
            async move {
                let id = parse_user_id(ctx.path_param("id")?)?;
                let password = random_password();
                let user = set_user_password(&catalog, id, &password).await?;
                // The one response that carries a readable password. Every live
                // session keeps working — a password reset is not a logout, and
                // an admin who wants both has "force logout" next to this.
                Ok(HandlerResponse::ok(json!({
                    "email": user.get(COL_EMAIL).and_then(sc_query::Value::as_text).unwrap_or_default(),
                    "password": password,
                })))
            }
        }
    });

    reg
}

// --- request/response shaping --------------------------------------------------
//
// A note on visibility, because several of these are `pub(crate)` rather than
// private: they are the admin API's *wire shape*, and a backup is the admin
// API's JSON in a zip (see [`crate::backup`]). The writer builds a backup out of
// the same `*_json` functions the endpoints answer with, and the restorer feeds
// it back through the same `*_from_body` parsers the endpoints accept — so a
// field added to an application, an agent or a trigger reaches the backup format
// the moment it reaches the API, with nothing to remember.

/// A field (column) with its `_sc_fields` overlay merged on (§3.2): the
/// introspected column facts plus the overlay's type, kind, label, description and
/// attributes. `description` is passed in because it lives only in the overlay
/// row, not the merged [`DataField`].
pub(crate) fn field_json(field: &DataField, description: &str) -> Json {
    json!({
        "name": field.base.name,
        "label": field.base.label,
        "description": description,
        "sql_type": field.base.type_.sql_type(),
        "type": field.base.type_.name(),
        "nullable": !field.required,
        "required": field.required,
        "unique": field.unique,
        // Introspected, like `unique`: what a field editor needs to say which
        // column a reference onto this table should point at by default.
        "primary_key": field.primary_key,
        // Whether the database fills the column in when a write omits it — an
        // identity key numbering itself, a `uuid` key generating itself. The
        // editor needs it to tell the admin which keys they must type, and the
        // row form needs it to leave a blank one out of the insert instead of
        // sending a null the `NOT NULL` would refuse.
        "generated": field.generated.is_some(),
        "kind": field_kind_json(&field.kind),
        "attributes": Json::Object(field.base.attributes.clone()),
    })
}

/// One constraint on the wire. Every kind uses one shape, with the fields that
/// do not apply left null — which is what lets the screen render a list of four
/// different things without four branches for the data and four for the display.
pub(crate) fn constraint_json(constraint: &TableConstraint) -> Json {
    let (fields, expression, method, language, formula) = match &constraint.kind {
        ConstraintKind::Unique { fields } => (fields.clone(), None, None, None, None),
        ConstraintKind::Index {
            fields,
            expression,
            method,
        } => (
            fields.clone(),
            expression.clone(),
            Some(method.clone()),
            None,
            None,
        ),
        ConstraintKind::FullTextSearch { language } => {
            (Vec::new(), None, None, Some(language.clone()), None)
        }
        ConstraintKind::Formula { formula } => {
            (Vec::new(), None, None, None, Some(formula.clone()))
        }
    };
    json!({
        "name": constraint.name,
        "type": constraint.kind.type_name(),
        "fields": fields,
        "expression": expression,
        "method": method,
        "language": language,
        "formula": formula,
        "error_message": constraint.error_message,
        "managed": constraint.is_saltcorn(),
    })
}

/// Read a `createConstraint` body into the short name an admin gave (only a row
/// constraint has one) and the constraint itself.
///
/// The `type` decides which of the body's fields are read, and a missing one is
/// named rather than defaulted: a unique constraint with no fields would be a
/// constraint over nothing, and a row constraint with no formula a trigger that
/// checks nothing.
fn constraint_from_body(obj: &Map<String, Json>) -> Result<(String, TableConstraint)> {
    let type_name = non_empty_str_field(obj, "type")?.trim().to_owned();
    let kind = match type_name.as_str() {
        "unique" => ConstraintKind::Unique {
            fields: string_array(obj, "fields")?,
        },
        "index" => ConstraintKind::Index {
            fields: string_array(obj, "fields")?,
            expression: None,
            method: "btree".to_owned(),
        },
        "full_text_search" => ConstraintKind::FullTextSearch {
            // The default is the ordinary one: an installation that has not
            // thought about stemming wants English rather than an error.
            language: match optional_str(obj, "language").trim() {
                "" => "english".to_owned(),
                given => given.to_owned(),
            },
        },
        "formula" => ConstraintKind::Formula {
            formula: non_empty_str_field(obj, "formula")?.trim().to_owned(),
        },
        other => {
            return Err(Error::invalid(format!(
                "`{other}` is not a constraint type; it is one of `unique`, `index`, \
                 `full_text_search` or `formula`"
            )));
        }
    };
    let mut constraint = TableConstraint::new(String::new(), kind);
    constraint.error_message = Some(optional_str(obj, "error_message"))
        .map(|m| m.trim().to_owned())
        .filter(|m| !m.is_empty());
    Ok((optional_str(obj, "name").trim().to_owned(), constraint))
}

/// A required array-of-strings field, e.g. the fields of a unique constraint.
fn string_array(obj: &Map<String, Json>, key: &str) -> Result<Vec<String>> {
    let array = obj
        .get(key)
        .and_then(Json::as_array)
        .ok_or_else(|| Error::invalid(format!("`{key}` must be an array of field names")))?;
    array
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| Error::invalid(format!("`{key}` must be an array of field names")))
        })
        .collect()
}

/// A field kind as `{ type, …parameters }` — the wire shape `createField` and
/// `updateField` accept back.
fn field_kind_json(kind: &DataFieldKind) -> Json {
    match kind {
        DataFieldKind::Plain => json!({ "type": "plain" }),
        DataFieldKind::Key {
            target_table,
            target_field,
            summary_field,
        } => json!({
            "type": "key",
            "target_table": target_table.0,
            "target_field": target_field.0,
            "summary_field": summary_field.as_ref().map(|f| f.0.clone()),
        }),
        DataFieldKind::File {
            store,
            folder,
            mime_allow,
        } => json!({
            "type": "file",
            "store": store.0,
            "folder": folder,
            "mime_allow": mime_allow,
        }),
        DataFieldKind::Calc { expression } => json!({
            "type": "calc",
            "expression": expression,
        }),
    }
}

/// One `listFieldTypes` entry.
fn field_type_json(name: &str, category: &str, spec: &[FormField]) -> Json {
    json!({
        "name": name,
        "label": name,
        "category": category,
        "config_spec": spec.iter().map(form_field_json).collect::<Vec<_>>(),
    })
}

/// Parse the optional `kind` object of a create/update request into a
/// [`DataFieldKind`], defaulting to `Plain` when absent or null.
fn parse_field_kind(obj: &Map<String, Json>) -> Result<DataFieldKind> {
    let Some(value) = obj.get("kind").filter(|v| !v.is_null()) else {
        return Ok(DataFieldKind::Plain);
    };
    let kind = value
        .as_object()
        .ok_or_else(|| Error::invalid("`kind` must be an object"))?;
    match str_field(kind, "type")? {
        "plain" => Ok(DataFieldKind::Plain),
        "file" => Ok(DataFieldKind::File {
            store: FileStoreId(non_empty_str_field(kind, "store")?.to_owned()),
            folder: optional_present_str(kind, "folder"),
            mime_allow: string_array_field(kind, "mime_allow")?,
        }),
        "key" => Ok(DataFieldKind::Key {
            target_table: TableId(non_empty_str_field(kind, "target_table")?.to_owned()),
            target_field: FieldId(non_empty_str_field(kind, "target_field")?.to_owned()),
            summary_field: optional_present_str(kind, "summary_field").map(FieldId),
        }),
        // A non-stored calculated field (Phase 8): a virtual field with no
        // column, only an `sc-expr` expression computed on read.
        "calc" => Ok(DataFieldKind::Calc {
            expression: non_empty_str_field(kind, "expression")?.to_owned(),
        }),
        other => Err(Error::invalid(format!("unknown field kind `{other}`"))),
    }
}

/// The field a `createField` body describes, ready for the schema editor.
///
/// Shared with a restore, which is handed exactly the `field_json` a `listFields`
/// would have returned: one parser, so a field kind the editor learns to write is
/// a field kind a backup can carry.
pub(crate) fn field_spec_from_body(obj: &Map<String, Json>) -> Result<schema_edit::FieldSpec> {
    Ok(schema_edit::FieldSpec {
        name: non_empty_str_field(obj, "name")?.to_owned(),
        type_name: optional_str(obj, "type"),
        label: optional_str(obj, "label"),
        description: optional_str(obj, "description"),
        required: optional_bool(obj, "required")?,
        unique: optional_bool(obj, "unique")?,
        primary_key: optional_bool(obj, "primary_key")?,
        kind: parse_field_kind(obj)?,
        attributes: attributes_field(obj)?,
    })
}

/// The whole-object table settings an `updateTable` body carries: every value
/// stated, so an omitted one means cleared (see `updateTable` for why that is the
/// right reading *here* and the wrong one in an agent's batch).
pub(crate) fn table_settings_from_body(
    obj: &Map<String, Json>,
) -> Result<schema_edit::TableSettings> {
    Ok(schema_edit::TableSettings {
        label: Some(str_field(obj, "label")?.to_owned()),
        description: Some(str_field(obj, "description")?.to_owned()),
        min_role_read: Some(role_field(obj, "min_role_read")?),
        min_role_write: Some(role_field(obj, "min_role_write")?),
        ownership_formula: Some(str_field(obj, "ownership_formula")?.to_owned()),
        rls_enabled: Some(bool_field(obj, "rls_enabled")?),
    })
}

/// The overlay description for a field, from the metas loaded for its table.
fn description_of(metas: &[FieldMeta], field: &str) -> String {
    metas
        .iter()
        .find(|m| m.field_name == field)
        .map(|m| m.description.clone())
        .unwrap_or_default()
}

// --- applications: wire shaping ------------------------------------------------

/// An [`Application`] as the API returns it (matching `application_schema`): the
/// id plus every field, with the nested framework/CSP/attributes as plain JSON.
pub(crate) fn application_json(app: &Application) -> Json {
    json!({
        "id": app.id.to_string(),
        "name": app.name,
        "description": app.description,
        "subdomain": app.subdomain,
        "framework": framework_ref_json(&app.framework),
        "extra_frameworks": app.extra_frameworks.iter().map(framework_ref_json).collect::<Vec<_>>(),
        "tables": app.tables.iter().map(|t| t.0.clone()).collect::<Vec<_>>(),
        "file_stores": app.file_stores.iter().map(|s| s.0.clone()).collect::<Vec<_>>(),
        "triggers": app.triggers.iter().map(|t| t.0.clone()).collect::<Vec<_>>(),
        "apis": app.apis.iter().map(|a| json!({ "provider": a.provider, "mount": a.mount, "config": Json::Object(a.config.clone()) })).collect::<Vec<_>>(),
        "static_dirs": app.static_dirs.iter().map(|d| json!({ "mount": d.mount, "store": d.store.0, "path": d.path })).collect::<Vec<_>>(),
        "csp": csp_json(&app.csp),
        "attributes": Json::Object(app.attributes.clone()),
        "source": app_source_json(app),
    })
}

/// Where the app's source lives, as `{ store, path }` — or `null` for a framework
/// with no source tree.
///
/// **Derived here rather than read off the config**, because how a framework
/// spells its source is the framework's business: `code` states it in `store` +
/// `source`, `react` derives it from `project`. Resolving it server-side is what
/// lets the admin UI link into the file manager at an app's source with no
/// per-framework code in the screen (§2.4). A config that does not resolve is not
/// an error here: this is a convenience field on a row that is being listed, and
/// the *reason* it does not resolve is reported where it belongs — on save, or on
/// build.
fn app_source_json(app: &Application) -> Json {
    match app_source_from_config(&app.framework) {
        Ok(source) => json!({
            "store": source.store.0,
            "path": source.build.source_dir,
        }),
        Err(_) => Json::Null,
    }
}

/// A [`FrameworkRef`] as `{ name, config }`.
fn framework_ref_json(fw: &FrameworkRef) -> Json {
    json!({ "name": fw.name, "config": Json::Object(fw.config.clone()) })
}

/// A [`CspPolicy`] as a directive→sources object.
fn csp_json(csp: &CspPolicy) -> Json {
    Json::Object(
        csp.directives
            .iter()
            .map(|(name, sources)| {
                (
                    name.clone(),
                    Json::Array(sources.iter().map(|s| Json::String(s.clone())).collect()),
                )
            })
            .collect(),
    )
}

/// Scaffold a newly created application's project, when it is one that has a
/// project to scaffold.
///
/// `Ok(None)` means "nothing to do" — the app uses a framework that brings its
/// own project (`code`), which is not a failure and should not be reported as
/// one. An `Err` is a real scaffold failure (unreachable store, occupied
/// directory) and is surfaced *alongside* the created application rather than
/// instead of it.
async fn scaffold_new_app(
    catalog: &Catalog,
    app: &Application,
    dispatcher: Option<&Arc<sc_action::TriggerDispatcher>>,
) -> Result<Option<sc_app::ScaffoldReport>> {
    if require_scaffoldable(app).is_err() {
        return Ok(None);
    }
    scaffold_app(catalog, app, dispatcher).await.map(Some)
}

/// Rewrite a saved application's generated files, reporting a failure to the
/// operator's console and to nobody else.
///
/// Saving an application changes its API definition — the tables it declares,
/// the providers it enables, the custom queries on them — so its `src/saltcorn/`
/// must follow (decision 10). It deliberately does **not** affect the response:
/// the application is stored and valid, and an unreachable store is something
/// the admin fixes and re-triggers with the update button, not a reason to tell
/// them their save failed.
async fn reemit_app_client(
    catalog: &Catalog,
    app: &Application,
    dispatcher: Option<&Arc<sc_action::TriggerDispatcher>>,
) {
    if let Err(e) = sc_app::emit_app_client(catalog, app, dispatcher).await {
        eprintln!(
            "saltcorn: application `{}` was saved, but its generated client could \
             not be rewritten: {e}",
            app.subdomain
        );
    }
}

/// Create the agent that builds a newly created application, returning its name.
///
/// **Which agent it is belongs to the framework** ([`framework_builder_agent`]),
/// which is where the knowledge sits: a code framework's application is a source
/// tree, so its builder is a coding agent over that tree, and a framework with no
/// source tree declares nothing. This function is only the part that needs to know
/// agents exist at all — assembling the declaration into a record and storing it —
/// because `sc-app` is a layer below `sc-agent`'s trait registry.
///
/// `Ok(None)` is "nothing to do, and that is not news": a framework that declares
/// no builder, a server assembled without agents, or an agent of that name already
/// there (a re-created application meets its own old builder, which still points
/// at the same subdomain). An `Err` is news the admin should hear — no provider is
/// connected, or the agent did not validate — and is reported *beside* the created
/// application, never instead of it: the row is saved and valid either way.
async fn create_builder_agent(
    catalog: &Catalog,
    apps: &AppMounts,
    app: &Application,
) -> Result<Option<String>> {
    let Some(spec) = framework_builder_agent(&app.framework, app) else {
        return Ok(None);
    };
    let Some(services) = apps.agents() else {
        return Ok(None);
    };
    if sc_agent::load_agent_by_name(catalog, &spec.name)
        .await?
        .is_some()
    {
        return Ok(None);
    }

    // An agent needs a provider that is connected, and a framework cannot know
    // which one a deployment has. The first by name is a choice, not a
    // preference — the admin can change it on the agent — but a deployment with
    // none is a thing to say out loud rather than a silently agent-less
    // application.
    let provider = list_llm_providers(catalog)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| {
            Error::config(format!(
                "no LLM provider is connected, so the `{}` agent that builds this \
                 application was not created; connect a provider and create it from \
                 the Agents screen",
                spec.name
            ))
        })?;

    let mut agent = sc_agent::Agent::new(&spec.name, &provider.name)
        .description(spec.description)
        .system_prompt(spec.system_prompt);
    for enabled in spec.traits {
        agent = agent
            .with_trait(sc_agent::EnabledTrait::new(enabled.trait_).configuration(enabled.config));
    }
    // The same save the Agents screen makes, so the same validation applies: a
    // builder agent this server would refuse to run is not one it quietly stores.
    sc_agent::save_agent(catalog, services.registry(), &agent).await?;
    Ok(Some(agent.name))
}

/// Delete the agent created to build `app`, returning its name if one went.
///
/// The other half of [`create_builder_agent`]: an application's builder is scoped
/// to that application and can do nothing once it is gone, so leaving it behind
/// would leave the admin who deleted the application an agent to clean up whose
/// only remaining property is that it does not work.
///
/// **It deletes that application's builder, not every agent that shares its
/// name.** The name is the derivation ([`builder_agent_name`]), but the check is
/// the trait: only an agent still carrying `build_application` for *this*
/// subdomain is one. So an agent an admin created themselves under that name, or
/// re-pointed at something else, survives the deletion — a delete button on one
/// screen must not silently take an agent that is doing another job. What an
/// admin's edits to the real builder cannot buy it is survival: an agent that
/// still names this application is still this application's.
///
/// Its **runs are not deleted**, as `deleteAgent` does not delete them either: a
/// transcript is a record of what happened, and what happened does not stop
/// having happened because the application was removed.
async fn delete_builder_agent(catalog: &Catalog, app: &Application) -> Result<Option<String>> {
    // No `_sc_agents` table *means* no agent has ever been defined — the same
    // reading `Agents::load` takes — so a server without agents installed deletes
    // an application rather than failing over a table nobody made.
    if catalog.get(sc_agent::AGENTS_TABLE)?.is_none() {
        return Ok(None);
    }
    let name = builder_agent_name(app);
    let Some(agent) = sc_agent::load_agent_by_name(catalog, &name).await? else {
        return Ok(None);
    };
    let subdomain = app.subdomain.trim();
    let builds_this_app = agent.traits.iter().any(|t| {
        t.trait_ == sc_app::TRAIT_BUILD_APPLICATION
            && t.config
                .get(sc_app::TRAIT_CFG_APPLICATION)
                .and_then(Json::as_str)
                .map(str::trim)
                == Some(subdomain)
    });
    if !builds_this_app {
        return Ok(None);
    }
    sc_agent::delete_agent(catalog, agent.id).await?;
    Ok(Some(name))
}

/// Parse an [`Application`] from a create/update body (matching
/// `application_input_schema`), carrying `id` — the body never sets the identity.
///
/// Missing collections default to empty and a missing `csp` to
/// [`CspPolicy::strict`], so the minimal body is `{ name, subdomain, framework }`.
/// Save-time validation (framework config against its spec) happens in
/// `save_application`, not here.
pub(crate) fn application_from_body(id: AppId, body: &Json) -> Result<Application> {
    let obj = require_object(body)?;
    let name = non_empty_str_field(obj, "name")?.to_owned();
    let subdomain = non_empty_str_field(obj, "subdomain")?.to_owned();
    let description = obj
        .get("description")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_owned();
    let framework = parse_framework(
        obj.get("framework")
            .ok_or_else(|| Error::invalid("missing field `framework`"))?,
    )?;
    let extra_frameworks = parse_array(obj, "extra_frameworks")?
        .iter()
        .map(parse_framework)
        .collect::<Result<_>>()?;
    let tables = parse_str_array(obj, "tables")?
        .into_iter()
        .map(TableId)
        .collect();
    let file_stores = parse_str_array(obj, "file_stores")?
        .into_iter()
        .map(FileStoreId)
        .collect();
    // Absent means "expose nothing": the field arrived after applications did,
    // and an older client that does not send it must not have its app's exposed
    // subset silently rewritten — `parse_str_array` already reads a missing
    // field as the empty list.
    let triggers = parse_str_array(obj, "triggers")?
        .into_iter()
        .map(TriggerRef)
        .collect();
    let apis = parse_array(obj, "apis")?
        .iter()
        .map(|v| {
            let o = require_object(v)?;
            // The mount is required rather than defaulted: an empty one
            // normalises to `/`, which claims every path — a blank field is far
            // more likely a slip than a request for that.
            //
            // The settings are optional and validated on save against the spec
            // the provider declares (`validate_api_config`), so a key nobody
            // declared is refused here rather than stored and ignored.
            Ok(ApiConfig::new(
                non_empty_str_field(o, "provider")?,
                non_empty_str_field(o, "mount")?,
            )
            .with_config(object_field(o, "config")?))
        })
        .collect::<Result<_>>()?;
    let static_dirs = parse_array(obj, "static_dirs")?
        .iter()
        .map(|v| {
            let o = require_object(v)?;
            Ok(StaticDir::new(
                str_field(o, "mount")?,
                FileStoreId(non_empty_str_field(o, "store")?.to_owned()),
                str_field(o, "path")?,
            ))
        })
        .collect::<Result<_>>()?;
    let csp = parse_csp(obj.get("csp"), &framework.name)?;
    let attributes = match obj.get("attributes") {
        None | Some(Json::Null) => Attrs::new(),
        Some(Json::Object(o)) => o.clone(),
        Some(_) => return Err(Error::invalid("`attributes` must be an object")),
    };

    Ok(Application {
        id,
        name,
        description,
        subdomain,
        framework,
        extra_frameworks,
        tables,
        file_stores,
        triggers,
        apis,
        static_dirs,
        csp,
        attributes,
    })
}

/// Parse a `{ name, config }` framework reference; a missing/null `config` is an
/// empty settings bag.
fn parse_framework(value: &Json) -> Result<FrameworkRef> {
    let obj = require_object(value)?;
    let name = non_empty_str_field(obj, "name")?.to_owned();
    let config = match obj.get("config") {
        None | Some(Json::Null) => Attrs::new(),
        Some(Json::Object(o)) => o.clone(),
        Some(_) => {
            return Err(Error::invalid(format!(
                "framework `{name}` config must be an object"
            )));
        }
    };
    Ok(FrameworkRef { name, config })
}

/// Parse a `csp` directive→sources object; absent means the framework's default
/// policy ([`framework_default_csp`]).
///
/// The default is the framework's rather than a fixed `strict` because a
/// framework that also chooses the build tooling knows what that tooling's output
/// needs — a scaffolded React app gets a policy fitted to a Vite bundle, and the
/// admin who is no longer picking the bundler is not asked to derive the policy
/// for it either. A stated `csp` always wins: this is a default, not a fixture.
fn parse_csp(value: Option<&Json>, framework: &str) -> Result<CspPolicy> {
    match value {
        None | Some(Json::Null) => Ok(framework_default_csp(framework)),
        Some(Json::Object(o)) => {
            let mut directives = BTreeMap::new();
            for (name, sources) in o {
                let Some(items) = sources.as_array() else {
                    return Err(Error::invalid(format!(
                        "csp directive `{name}` must be a list of sources"
                    )));
                };
                let sources = items
                    .iter()
                    .map(|s| {
                        s.as_str().map(str::to_owned).ok_or_else(|| {
                            Error::invalid(format!(
                                "csp directive `{name}` has a non-string source"
                            ))
                        })
                    })
                    .collect::<Result<_>>()?;
                directives.insert(name.clone(), sources);
            }
            Ok(CspPolicy { directives })
        }
        Some(_) => Err(Error::invalid("`csp` must be an object")),
    }
}

/// An optional JSON-array field, defaulting to empty when absent or null.
fn parse_array<'a>(obj: &'a Map<String, Json>, key: &str) -> Result<&'a [Json]> {
    match obj.get(key) {
        None | Some(Json::Null) => Ok(&[]),
        Some(Json::Array(items)) => Ok(items),
        Some(_) => Err(Error::invalid(format!("field `{key}` must be an array"))),
    }
}

/// An optional array-of-strings field (table / store subsets), defaulting empty.
fn parse_str_array(obj: &Map<String, Json>, key: &str) -> Result<Vec<String>> {
    parse_array(obj, key)?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| Error::invalid(format!("field `{key}` must be an array of strings")))
        })
        .collect()
}

/// Parse an application id from a path segment.
/// A file store as the API returns it (matching `file_store_schema`): its stored
/// definition plus the live state only the running server knows.
///
/// `connected`/`error`/`is_git_repo` are read from the catalog rather than the
/// row, because they are not properties of the definition: the same definition
/// is connected on one machine and not on another, and that difference is
/// exactly what the admin needs to see.
fn file_store_json(catalog: &Catalog, def: &FileStoreDef) -> Result<Json> {
    let connected = catalog.file_store(&def.name)?;
    Ok(json!({
        "id": Some(def.id.0),
        "name": def.name,
        "description": def.description,
        "backend": def.backend,
        // Redacted here, where the record is serialised, rather than in the
        // screen (§11.1): every reader of this endpoint gets the same treatment,
        // including one written later that never thought about keys. No backend
        // declares a secret today — the git backend's `key_path` is a path and
        // its `public_key` is public — so this is currently a no-op, and that is
        // the point: an S3 backend's secret is redacted by declaring it, not by
        // editing this function.
        //
        // `display_config` first, for the other direction: a setting the admin
        // left blank and the backend then settled — a git store's working-copy
        // directory — is shown as what it actually is rather than as the blank
        // that is stored. It is read-only on the edit form, so a blank one would
        // leave the admin with nowhere to learn where their files are.
        "config": Json::Object(shown_config(def)),
        "min_role": def.min_role,
        "connected": connected.is_some(),
        // Only meaningful when not connected; a connected store has had any
        // recorded reason cleared, so this is null for a working store.
        "error": catalog.file_store_error(&def.name)?,
        // A property of the instance, so null when there is no instance to ask.
        "is_git_repo": connected.map(|store| store.is_git_repo()),
    }))
}

/// A stored store's config as the admin should see it: what the backend decided
/// for itself filled in ([`display_config`]), then its
/// [`secret`](FormField::secret) settings replaced by the sentinel.
///
/// An unknown backend redacts nothing rather than failing: this is called while
/// *listing*, and a store whose backend a plugin used to supply must still be
/// listable and editable so the admin can repoint it. There is nothing to leak
/// in that case either — no spec means no field is declared secret.
fn shown_config(def: &FileStoreDef) -> sc_types::Attrs {
    let config = display_config(def);
    match backend_config_spec(&def.backend) {
        Ok(spec) => sc_types::redact_attrs(&spec, &config),
        Err(_) => config,
    }
}

/// The reverse, on the way in: a submitted config whose sentinels have been
/// replaced by what is stored, so an admin editing a store's *name* does not
/// save the mask over its key — and whose
/// [`create_only`](FormField::create_only) settings are put back to what is
/// stored, because the store already exists.
///
/// Both are the same move and belong together: the spec says how a value
/// submitted for a field is to be read against the one already there. The
/// create-only half is the load-bearing one for a git store's working-copy
/// directory — the form renders it read-only, but a request need not come from
/// the form, and a save that repointed it would abandon the working tree rather
/// than move it.
///
/// **Only reached on an edit.** A create has nothing stored, so there is nothing
/// to preserve and a create-only setting is settable exactly once, which is what
/// the flag means.
fn unredacted_config(
    backend: &str,
    stored: &sc_types::Attrs,
    submitted: &sc_types::Attrs,
) -> sc_types::Attrs {
    match backend_config_spec(backend) {
        Ok(spec) => sc_types::preserve_create_only(
            &spec,
            stored,
            &sc_types::merge_secrets(&spec, stored, submitted),
        ),
        Err(_) => submitted.clone(),
    }
}

/// Run every operation the backend declares as part of **creating** a store,
/// before anything is written (`Operation::on_create` — a git store's clone).
///
/// A failure propagates, and that is the point: the caller has not saved yet, so
/// a store whose contents could not be created is a store that does not exist,
/// and the admin gets the backend's own message back in the form they are still
/// looking at.
///
/// Backend-agnostic: `local` declares no such operation and so does nothing
/// here, and a backend added later — an object store creating its bucket — gets
/// the same transactional create by declaring one.
async fn create_backend_resources(def: &mut FileStoreDef) -> Result<()> {
    for op in backend_operations(&def.backend)? {
        if op.on_create {
            run_backend_operation(def, &op.name, &sc_types::Attrs::new()).await?;
        }
    }
    Ok(())
}

/// The same operations, on an **edit** of a store that already exists — where a
/// failure is reported rather than fatal.
///
/// The asymmetry with [`create_backend_resources`] is deliberate and is §1.2's
/// rule: an existing store must stay saved and editable even when it cannot be
/// brought up, because editing it is the repair. Refusing the save would trap an
/// admin whose remote is briefly unreachable with a definition they can no
/// longer correct.
///
/// It runs on every save rather than only when something changed, because it is
/// idempotent (a clone that has already happened is a no-op) and because this is
/// how repointing a store at a reachable URL brings it back up.
async fn recreate_backend_resources(catalog: &Catalog, def: &mut FileStoreDef) -> Result<()> {
    for op in backend_operations(&def.backend)? {
        if !op.on_create {
            continue;
        }
        match run_backend_operation(def, &op.name, &sc_types::Attrs::new()).await {
            // The operation may have recorded something in the definition — a
            // git clone records where it cloned to — which has to reach the row,
            // or a later rename would abandon the working copy.
            Ok(_) => save_file_store(catalog, def).await?,
            // Record this failure rather than letting the connect that follows
            // overwrite it with "this has not been cloned yet" — which is true
            // but says nothing about *why*, and the why is the admin's next move
            // (a wrong URL, a deploy key not yet installed at the remote).
            Err(e) => return report_unusable(catalog, def, &e),
        }
    }
    if let Err(e) = connect_file_store_def(catalog, def) {
        return report_unusable(catalog, def, &e);
    }
    Ok(())
}

/// An edited store that cannot be brought up: stop it serving, and record why.
///
/// The **disconnect** is the part that is easy to miss. A handle in the registry
/// was built from the *previous* definition, so leaving it there after a failed
/// save means the store goes on serving the old URL, the old directory — while
/// the admin is looking at a form that says something else. The store is
/// reported as connected and works, which is the most confusing possible answer
/// to "did my edit take effect?".
///
/// Disconnecting clears any recorded error, so the reason is recorded after it.
fn report_unusable(catalog: &Catalog, def: &FileStoreDef, error: &Error) -> Result<()> {
    catalog.disconnect_file_store(&def.name)?;
    catalog.record_file_store_error(&def.name, sc_error::format_causes(error))
}

/// Check that `operation` is one the backend declares **and** that it is being
/// run at the right scope.
///
/// The scope check is not pedantry: a `Configure`-scope operation runs against
/// an unsaved definition built from a request body, so allowing one through the
/// instance endpoint (or the reverse) would run it against a definition it was
/// never written for. Refusing by name is also the clearer error — "the `local`
/// backend has no operation `pull`" beats a failure from inside git.
fn require_scope(backend: &str, operation: &str, scope: OperationScope) -> Result<()> {
    let declared = backend_operations(backend)?;
    let found = declared
        .iter()
        .find(|op| op.name == operation)
        .ok_or_else(|| {
            Error::invalid(format!(
                "the `{backend}` backend has no operation `{operation}`"
            ))
        })?;
    if found.scope != scope {
        return Err(Error::invalid(format!(
            "operation `{operation}` of the `{backend}` backend cannot be run here"
        )));
    }
    Ok(())
}

/// A JSON object field of a request body, defaulting to empty when absent —
/// which is what a caller sends for an operation that takes no arguments.
fn object_field(obj: &Map<String, Json>, key: &str) -> Result<sc_types::Attrs> {
    match obj.get(key) {
        Some(Json::Object(o)) => Ok(o.clone()),
        None | Some(Json::Null) => Ok(sc_types::Attrs::new()),
        Some(_) => Err(Error::invalid(format!("field `{key}` must be an object"))),
    }
}

/// Resolve a store by name to its live handle **and** its store-wide `min_role`
/// floor (§1.1), which is the outermost entry on every path in it.
///
/// The floor comes from the stored definition, so a store connected by
/// `--file-store` — which has no definition — has no floor. That is correct
/// rather than a gap: an ephemeral developer store has no configured policy, and
/// inventing a restrictive default would break the workflow the flag exists for.
async fn resolve_store(
    catalog: &Catalog,
    name: &str,
) -> Result<(Arc<dyn sc_files::FileStore>, Option<u8>)> {
    let store = catalog.require_file_store(name)?;
    let floor = load_file_store_by_name(catalog, name)
        .await
        .ok()
        .flatten()
        .and_then(|def| def.min_role);
    Ok((store, floor))
}

/// The role of the caller, defaulting to public when unauthenticated.
///
/// **Currently always `1` in practice**, because every file endpoint is
/// `AuthRequirement::admin()` and an admin clears every rule. The check is wired
/// in regardless: it is correct by construction, it costs nothing, and it starts
/// doing real work the moment a non-admin role can reach a store — which is what
/// an application's file access will need. Enforcement living only in the future
/// caller would be the same mistake as the rule living only in a doc comment.
fn caller_role(ctx: &crate::handler::HandlerCtx) -> u8 {
    ctx.user
        .as_ref()
        .map_or(sc_files::ROLE_PUBLIC, |user| user.role)
}

/// A file's metadata as the API returns it (matching `file_meta_schema`).
///
/// Reports both the rule set on this entry and the `effective_min_role` that
/// actually applies — they differ whenever a parent directory or the store
/// itself is more restrictive, and showing only the former would let an admin
/// believe a file is reachable when its folder has locked it.
fn file_meta_json(path: &str, meta: &FileMeta, effective: Option<u8>) -> Json {
    json!({
        "path": path,
        "min_role": meta.min_role,
        "effective_min_role": effective,
        "attributes": meta
            .attributes
            .iter()
            .map(|(k, v)| (k.clone(), Json::String(v.clone())))
            .collect::<Map<String, Json>>(),
    })
}

/// An optional role field of a body: absent or null means unrestricted, and a
/// value outside the 1–100 scale is rejected rather than clamped.
fn optional_role(obj: &Map<String, Json>, key: &str) -> Result<Option<u8>> {
    match obj.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(value) => {
            let raw = value
                .as_i64()
                .ok_or_else(|| Error::invalid(format!("field `{key}` must be a number or null")))?;
            u8::try_from(raw)
                .ok()
                .filter(|r| (1..=100).contains(r))
                .ok_or_else(|| {
                    Error::invalid(format!(
                        "field `{key}` must be a role between 1 and 100, got {raw}"
                    ))
                })
                .map(Some)
        }
    }
}

/// The entry JSON returned after creating a directory.
fn directory_entry_json(path: &str) -> Json {
    json!({
        "name": path.trim_matches('/').rsplit('/').next().unwrap_or(path),
        "path": path.trim_matches('/'),
        "is_dir": true,
        "size": Json::Null,
    })
}

/// The [`Entry`] describing a rename's destination, so the response says where
/// the thing now is.
fn renamed_entry(to: &str) -> Entry {
    let path = to.trim_matches('/').to_owned();
    Entry {
        name: path.rsplit('/').next().unwrap_or(&path).to_owned(),
        path,
        is_dir: false,
        size: None,
    }
}

/// A connected store that has **no** stored definition — one supplied by
/// `--file-store` (§1.3), which is deliberately ephemeral and unpersisted.
///
/// Reported with a null id, which is what tells the admin UI there is nothing to
/// edit or delete: the store is real and browsable, but its existence ends with
/// the process unless the flag is passed again. It is listed rather than hidden
/// because a developer running with the flag would otherwise see an empty
/// file-stores screen and have no way to reach the store they just connected.
fn ephemeral_file_store_json(catalog: &Catalog, name: &str) -> Result<Json> {
    let store = catalog.require_file_store(name)?;
    Ok(json!({
        "id": Json::Null,
        "name": name,
        "description": "",
        // Named rather than left blank: the flag only ever makes local stores,
        // and the UI shows this next to the stored ones.
        "backend": sc_files::LOCAL_BACKEND,
        "config": json!({}),
        "min_role": Json::Null,
        // It is in the registry, so by construction it is connected.
        "connected": true,
        "error": Json::Null,
        "is_git_repo": Some(store.is_git_repo()),
    }))
}

/// Build a [`FileStoreDef`] from a create/update body, with `id` supplied by the
/// caller (minted on create, taken from the path on update).
///
/// The backend's settings are *not* validated here: `save_file_store` checks
/// them against the backend's declared spec, which is the single place that
/// knows how, and doing it twice would risk the two drifting.
/// A store's definition for a **backup**: the `createFileStore` shape, with the
/// backend settings *not* redacted.
///
/// The one place a secret is written out deliberately. `file_store_json` redacts,
/// as everything that answers a browser must; a backup restored onto a fresh
/// server has to be able to connect the store, and a definition carrying the
/// sentinel where its credential was is a store that will not. The zip is
/// therefore as sensitive as the database it came from, which the Backup screen
/// says beside the choice.
pub(crate) fn backup_store_def_json(def: &FileStoreDef) -> Json {
    json!({
        "name": def.name,
        "description": def.description,
        "backend": def.backend,
        "config": Json::Object(def.config.clone()),
        "min_role": def.min_role,
    })
}

/// One file's stored metadata for a backup: the rules that were *set*, not the
/// effective ones.
///
/// `file_meta_json` reports `effective_min_role` too, which is the right answer
/// for a screen and the wrong one to restore: it is computed from the
/// directories above the file, so writing it back would turn an inherited rule
/// into a rule of its own.
pub(crate) fn backup_file_meta_json(path: &str, meta: &FileMeta) -> Json {
    json!({
        "path": path,
        "min_role": meta.min_role,
        "attributes": meta
            .attributes
            .iter()
            .map(|(k, v)| (k.clone(), Json::String(v.clone())))
            .collect::<Map<String, Json>>(),
    })
}

/// A file's metadata back from a backup, in the shape `setFileMeta` accepts.
pub(crate) fn backup_file_meta_from_json(value: &Json) -> Result<(String, FileMeta)> {
    let obj = require_object(value)?;
    let path = non_empty_str_field(obj, "path")?.to_owned();
    let attributes = match obj.get("attributes") {
        Some(Json::Object(o)) => o
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|text| (k.clone(), text.to_owned())))
            .collect::<BTreeMap<String, String>>(),
        _ => BTreeMap::new(),
    };
    Ok((
        path,
        FileMeta {
            min_role: optional_role(obj, "min_role")?,
            attributes,
        },
    ))
}

/// The table a trigger fires on, or `None` when it is not a table event.
///
/// The channel *is* the table for `insert`/`update`/`delete` and is something
/// else entirely for a channel-based or scheduled trigger, so the distinction is
/// made once, here, rather than at each place that needs to ask.
pub(crate) fn trigger_table(trigger: &Trigger) -> Option<&str> {
    match trigger.when {
        EventKind::Insert | EventKind::Update | EventKind::Delete => trigger.channel.as_deref(),
        _ => None,
    }
}

pub(crate) fn file_store_from_body(id: FileStoreDefId, body: &Json) -> Result<FileStoreDef> {
    let obj = require_object(body)?;
    let name = non_empty_str_field(obj, "name")?.to_owned();
    let backend = non_empty_str_field(obj, "backend")?.to_owned();
    let description = obj
        .get("description")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_owned();
    let config = match obj.get("config") {
        Some(Json::Object(o)) => o.clone(),
        // A backend with no settings may be posted without a config at all.
        None | Some(Json::Null) => sc_types::Attrs::new(),
        Some(_) => return Err(Error::invalid("field `config` must be an object")),
    };
    // Absent or null means unrestricted. A value outside the role scale is
    // rejected rather than clamped — clamping would quietly change who can
    // reach the store, which is the opposite of what the admin asked for.
    let min_role = match obj.get("min_role") {
        None | Some(Json::Null) => None,
        Some(value) => {
            let raw = value
                .as_i64()
                .ok_or_else(|| Error::invalid("field `min_role` must be a number or null"))?;
            Some(
                u8::try_from(raw)
                    .ok()
                    .filter(|r| (1..=100).contains(r))
                    .ok_or_else(|| {
                        Error::invalid(format!(
                            "field `min_role` must be a role between 1 and 100, got {raw}"
                        ))
                    })?,
            )
        }
    };

    Ok(FileStoreDef {
        id,
        name,
        description,
        backend,
        config,
        min_role,
        attributes: sc_types::Attrs::new(),
    })
}

/// A database connection as the API returns it (matching
/// `db_connection_schema`), with the password replaced by the sentinel and the
/// live state the running server knows added.
///
/// **The redaction is here and nowhere else**, for the reason
/// [`llm_provider_json`] keeps its: every response carrying a connection goes
/// through this, so an endpoint added later cannot return a password without
/// building the JSON by hand.
fn db_connection_json(catalog: &Catalog, def: &DbConnectionDef) -> Result<Json> {
    let connected = catalog.database(&def.name)?.is_some();
    let error = catalog.database_error(&def.name)?;
    // How many of the catalog's tables this connection actually contributed —
    // the number that answers "did it work", where `connected` only answers "did
    // it dial".
    let tables = catalog
        .tables()?
        .iter()
        .filter(|t| t.database.0 == def.name)
        .count();
    Ok(json!({
        "id": def.id.0,
        "backend": def.backend,
        "name": def.name,
        "description": def.description,
        "host": def.host,
        "port": def.port,
        "database": def.database,
        "username": def.username,
        // Never the stored password. An empty one stays empty rather than
        // becoming the sentinel: "no password" is a real state, and showing dots
        // for it would tell the admin a password exists that does not.
        "password": if def.password.is_empty() { "" } else { SECRET_SENTINEL },
        "schema": def.schema,
        "file_store": def.file_store,
        "file_path": def.file_path,
        "connected": connected,
        "error": error,
        "tables": tables,
        "shadowed": catalog.shadowed_tables(&def.name)?,
    }))
}

/// Build a [`DbConnectionDef`] from a request body, restoring the stored
/// password when the body echoes the sentinel back.
///
/// `existing` is the stored row on an update (and on a test of a saved
/// connection), `None` on a create. Passing it is what makes "save the form
/// without retyping the password" work, and passing `None` on a create is what
/// makes a literal sentinel there mean itself rather than a lookup that would
/// find nothing.
fn db_connection_from_body(
    id: DbConnectionId,
    body: &Json,
    existing: Option<&DbConnectionDef>,
) -> Result<DbConnectionDef> {
    let obj = require_object(body)?;
    let name = non_empty_str_field(obj, "name")?.trim().to_owned();
    let str_field = |key: &str| -> String {
        obj.get(key)
            .and_then(Json::as_str)
            .unwrap_or("")
            .trim()
            .to_owned()
    };

    // Absent, null or zero means "the Postgres default", which is what an admin
    // who left the box alone meant. Anything outside the TCP range is rejected
    // rather than clamped.
    let port = match obj.get("port") {
        None | Some(Json::Null) => sc_catalog::DEFAULT_PORT,
        Some(value) => {
            let raw = value
                .as_i64()
                .ok_or_else(|| Error::invalid("field `port` must be a number"))?;
            if raw == 0 {
                sc_catalog::DEFAULT_PORT
            } else {
                u16::try_from(raw).map_err(|_| {
                    Error::invalid(format!("field `port` must be a TCP port, got {raw}"))
                })?
            }
        }
    };

    let submitted = obj
        .get("password")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_owned();
    let password = match (submitted.as_str(), existing) {
        (SECRET_SENTINEL, Some(existing)) => existing.password.clone(),
        // A sentinel with nothing stored behind it is not a password anybody
        // meant to set; it is a form that was shown one for a connection this
        // request is not about. Empty is the safe reading.
        (SECRET_SENTINEL, None) => String::new(),
        (other, _) => other.to_owned(),
    };

    // A body that names no backend means Postgres, which is what every
    // connection was before there was a second kind.
    let backend = match str_field("backend").as_str() {
        "" => sc_catalog::POSTGRES_BACKEND.to_owned(),
        other => other.to_owned(),
    };
    let sqlite = backend == sc_catalog::SQLITE_BACKEND;
    let schema = str_field("schema");
    Ok(DbConnectionDef {
        id,
        backend,
        name,
        description: str_field("description"),
        host: str_field("host"),
        port,
        database: str_field("database"),
        username: str_field("username"),
        password,
        // A SQLite database has one namespace and no name for it, so the
        // default is not applied there: an empty schema is the truth about the
        // connection rather than a box the admin forgot to fill in.
        schema: match (schema.is_empty(), sqlite) {
            (true, false) => sc_catalog::DEFAULT_SCHEMA.to_owned(),
            _ => schema,
        },
        file_store: str_field("file_store"),
        file_path: str_field("file_path"),
        attributes: sc_types::Attrs::new(),
    })
}

/// Parse a database connection id from a path segment.
fn parse_db_connection_id(raw: &str) -> Result<DbConnectionId> {
    uuid::Uuid::parse_str(raw)
        .map(DbConnectionId)
        .map_err(|_| Error::invalid(format!("`{raw}` is not a valid database connection id")))
}

fn parse_file_store_id(raw: &str) -> Result<FileStoreDefId> {
    uuid::Uuid::parse_str(raw)
        .map(FileStoreDefId)
        .map_err(|_| Error::invalid(format!("`{raw}` is not a valid file store id")))
}

/// An LLM provider as the API returns it (matching `llm_provider_schema`), with
/// its backend's `secret` settings replaced by the sentinel.
///
/// **The redaction is here and nowhere else.** Every response that carries a
/// provider goes through this function, so an endpoint added later cannot
/// accidentally return a key: it would have to build the JSON by hand to do so.
fn llm_provider_json(def: &LlmProviderDef) -> Json {
    json!({
        "id": def.id.0,
        "name": def.name,
        "description": def.description,
        "backend": def.backend,
        "config": Json::Object(redacted_provider_config(&def.backend, &def.config)),
    })
}

/// A provider config with its backend's [`secret`](FormField::secret) settings
/// replaced by the sentinel.
///
/// An unknown backend redacts nothing rather than failing, for the same reason
/// `redacted_config` does for stores: a provider whose backend is no longer
/// registered must stay listable so the admin can repoint or delete it, and no
/// spec means no field was declared secret in the first place.
fn redacted_provider_config(backend: &str, config: &sc_types::Attrs) -> sc_types::Attrs {
    match provider_config_spec(backend) {
        Ok(spec) => sc_types::redact_attrs(&spec, config),
        Err(_) => config.clone(),
    }
}

/// The reverse, on the way in: sentinels replaced by what is stored, so editing
/// a provider's name does not save the mask over its key.
fn unredacted_provider_config(
    backend: &str,
    stored: &sc_types::Attrs,
    submitted: &sc_types::Attrs,
) -> sc_types::Attrs {
    match provider_config_spec(backend) {
        Ok(spec) => sc_types::merge_secrets(&spec, stored, submitted),
        Err(_) => submitted.clone(),
    }
}

/// Rebuild a provider definition from a create/update body.
fn llm_provider_from_body(id: LlmProviderDefId, body: &Json) -> Result<LlmProviderDef> {
    let obj = require_object(body)?;
    Ok(LlmProviderDef {
        id,
        name: non_empty_str_field(obj, "name")?.to_owned(),
        description: obj
            .get("description")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_owned(),
        backend: non_empty_str_field(obj, "backend")?.to_owned(),
        config: object_field(obj, "config")?,
        attributes: sc_types::Attrs::new(),
    })
}

/// Parse a provider id from a path parameter.
fn parse_llm_provider_id(raw: &str) -> Result<LlmProviderDefId> {
    uuid::Uuid::parse_str(raw)
        .map(LlmProviderDefId)
        .map_err(|_| Error::invalid(format!("`{raw}` is not a valid LLM provider id")))
}

/// One loaded module as JSON (matching `module_schema`): the row, what the
/// package supplies, and everything wrong with it.
///
/// The actions' settings are translated here rather than carried from the
/// registry because they are the same translation — v1's `configFields` through
/// [`sc_module::config_fields_to_form_fields`] — and a module that failed to
/// register an action (a name clash) still has settings the tab should show
/// beside the reason it is not available.
fn module_json(loaded: &sc_module::LoadedModule) -> Json {
    let module = &loaded.module;
    let actions: Vec<Json> = loaded
        .manifest
        .as_ref()
        .map(|manifest| {
            manifest
                .actions
                .iter()
                .map(|action| {
                    let (spec, _) = sc_module::config_fields_to_form_fields(
                        &action.config_fields,
                        &action.name,
                    );
                    json!({
                        "name": action.name,
                        "description": action.description,
                        "config_spec": spec.iter().map(form_field_json).collect::<Vec<_>>(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let functions: Vec<Json> = loaded
        .manifest
        .as_ref()
        .map(|manifest| {
            manifest
                .functions
                .iter()
                .map(|function| {
                    json!({
                        "name": function.name,
                        "description": function.description,
                        "is_async": function.is_async,
                        "arguments": function
                            .arguments
                            .iter()
                            .map(|argument| json!({
                                "name": argument.name,
                                "type": argument.type_name,
                            }))
                            .collect::<Vec<_>>(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    json!({
        "id": module.id.0,
        "name": module.name,
        "source": module.source.as_str(),
        "location": module.location,
        "version": module.version,
        "configuration": Json::Object(sc_module::redacted_configuration(loaded)),
        // Not redacted, and nothing here is a secret: a permission set is what
        // an admin granted, and the point of the screen is that it can be read.
        "permissions": Json::Object(module.permissions.to_json()),
        "config_spec": loaded
            .config_spec
            .iter()
            .map(form_field_json)
            .collect::<Vec<_>>(),
        "actions": actions,
        "functions": functions,
        // The table providers it supplies (§8.3): what the "new table" screen
        // offers, listed here too because the Modules tab is where an admin
        // finds out what installing a module got them.
        "table_providers": loaded.table_provider_names(),
        "unsupported": sc_module::unsupported_json(loaded),
        "issues": loaded.issues,
        "loaded": loaded.is_loaded(),
        "api_version": loaded.manifest.as_ref().and_then(|m| m.api_version),
    })
}

/// Parse a module id from a path parameter.
fn parse_module_id(raw: &str) -> Result<sc_module::ModuleId> {
    uuid::Uuid::parse_str(raw)
        .map(sc_module::ModuleId)
        .map_err(|_| Error::invalid(format!("`{raw}` is not a valid module id")))
}

/// The module services this server was built with, or a configuration error
/// saying it has none — the same shape [`triggers_of`] has, and for the same
/// reason: a test or an admin-only server may have booted without them, and the
/// Modules tab should say so rather than appear to work.
fn modules_of(apps: &AppMounts) -> Result<Arc<crate::ModuleServices>> {
    apps.modules().cloned().ok_or_else(|| {
        Error::config("this server has no module support installed, so modules cannot be managed")
    })
}

/// One stored agent as JSON (matching `agent_schema`), with the reason it cannot
/// run when there is one (§11.2 — a broken agent stays listed and editable).
pub(crate) fn agent_json(agent: &sc_agent::Agent, problem: Option<String>) -> Json {
    json!({
        "id": agent.id.0,
        "name": agent.name,
        "description": agent.description,
        "provider": agent.provider,
        "model": agent.model,
        "system_prompt": agent.system_prompt,
        "traits": agent
            .traits
            .iter()
            .map(|enabled| json!({
                "trait": enabled.trait_,
                "config": Json::Object(enabled.config.clone()),
            }))
            .collect::<Vec<_>>(),
        "min_role": agent.min_role,
        "attributes": Json::Object(agent.attributes.clone()),
        "error": problem,
    })
}

/// Rebuild an agent definition from a create/update body.
///
/// Nothing is defaulted quietly: an absent `traits` is an agent with no traits,
/// which is a real agent (one that only talks), while a `traits` of the wrong
/// shape is a refusal naming the entry — an agent half-read is one that would
/// answer with the wrong tools.
pub(crate) fn agent_from_body(id: sc_agent::AgentId, body: &Json) -> Result<sc_agent::Agent> {
    let obj = require_object(body)?;
    let mut agent = sc_agent::Agent::with_id(
        id,
        non_empty_str_field(obj, "name")?,
        non_empty_str_field(obj, "provider")?,
    )
    .description(optional_str(obj, "description"))
    .system_prompt(optional_str(obj, "system_prompt"));
    // Absent, null or blank all mean "the provider's own default model", which
    // is a real answer rather than a missing one — a form posts "" for a box it
    // left alone.
    if let Some(model) = obj.get("model").and_then(Json::as_str)
        && !model.trim().is_empty()
    {
        agent = agent.model(model.trim());
    }
    if let Some(traits) = obj.get("traits").filter(|v| !v.is_null()) {
        let Json::Array(entries) = traits else {
            return Err(Error::invalid("field `traits` must be an array"));
        };
        for (i, entry) in entries.iter().enumerate() {
            let Json::Object(entry) = entry else {
                return Err(Error::invalid(format!(
                    "trait {} must be an object of `trait` and `config`",
                    i + 1
                )));
            };
            let name = entry
                .get("trait")
                .and_then(Json::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| Error::invalid(format!("trait {} must name a trait", i + 1)))?;
            agent = agent.with_trait(
                sc_agent::EnabledTrait::new(name.trim())
                    .configuration(object_field(entry, "config")?),
            );
        }
    }
    if let Some(min_role) = obj.get("min_role").filter(|v| !v.is_null()) {
        let raw = min_role
            .as_i64()
            .ok_or_else(|| Error::invalid("field `min_role` must be a number"))?;
        agent = agent.min_role(u8::try_from(raw).map_err(|_| {
            Error::invalid(format!(
                "`min_role` must be a role between 1 and 100, got {raw}"
            ))
        })?);
    }
    // The sparse per-agent values (§9): whatever the form set, nothing it did
    // not. An empty bag means every one of them is the provider's default.
    agent.attributes = object_field(obj, "attributes")?;
    Ok(agent)
}

/// Parse an agent id from a path parameter.
fn parse_agent_id(raw: &str) -> Result<sc_agent::AgentId> {
    Ok(sc_agent::AgentId(parse_uuid(raw, "agent")?))
}

/// Parse a UUID path parameter, naming what it was meant to identify.
fn parse_uuid(raw: &str, what: &str) -> Result<uuid::Uuid> {
    uuid::Uuid::parse_str(raw)
        .map_err(|_| Error::invalid(format!("`{raw}` is not a valid {what} id")))
}

/// One run as a list entry (matching `run_summary_schema`): everything except
/// the transcript, which a list of dozens of conversations must not carry.
fn run_summary_json(run: &sc_agent::Run) -> Json {
    json!({
        "id": run.id.0,
        "kind": run.kind.as_str(),
        "subject": run.subject,
        "description": run.description,
        "state": run.state.as_str(),
        "error": run.error,
        "user": run.user,
        "created_at": run.created_at,
        "updated_at": run.updated_at,
        // The workflow half (§10.3), null on an agent run: the version this run
        // is pinned to, the step it is on, and when it next wants the engine.
        "subject_version": run.subject_version,
        "current_step": sc_workflow::run_current_step(run),
        "wake_at": run.wake_at,
    })
}

/// One whole run (matching `run_schema`): the summary plus the loop state the
/// chat panel reads a transcript out of.
///
/// The workflow half — the trace and the pending form — is empty here and filled
/// in by [`run_json_with_workflow`], which is the one that reads the database.
fn run_json(run: &sc_agent::Run) -> Json {
    let mut out = run_summary_json(run);
    if let Json::Object(fields) = &mut out {
        fields.insert("context".to_owned(), run.context.clone());
        fields.insert(
            "attributes".to_owned(),
            Json::Object(run.attributes.clone()),
        );
        fields.insert("trace".to_owned(), Json::Array(Vec::new()));
        fields.insert("pending_form".to_owned(), Json::Null);
    }
    out
}

/// One whole run, with the workflow half filled in: the `_sc_run_traces` rows a
/// traced workflow wrote, and the form a suspended run is waiting on (§10.3).
///
/// Empty and null on an agent run rather than absent, so one typed shape serves
/// both engines and the client has no union to narrow. The trace rows are read
/// unconditionally for a workflow run because a workflow with tracing off simply
/// has none — asking is one query, and branching on the pinned version's `trace`
/// flag would be a second read to save the first.
async fn run_json_with_workflow(catalog: &Catalog, run: &sc_agent::Run) -> Result<Json> {
    let mut out = run_json(run);
    if run.kind != sc_agent::RunKind::Workflow {
        return Ok(out);
    }
    let traces = sc_workflow::list_run_traces(catalog, run.id.0).await?;
    // A run whose stored state will not parse is still a row worth showing: the
    // admin looking at it is the person who has to decide what to do about it,
    // and answering an error instead of the run would take the evidence away.
    let form = sc_workflow::run_pending_form(run).unwrap_or(None);
    if let Json::Object(fields) = &mut out {
        fields.insert(
            "trace".to_owned(),
            Json::Array(traces.iter().map(run_trace_json).collect()),
        );
        fields.insert(
            "pending_form".to_owned(),
            match form {
                Some(form) => pending_form_json(&form)?,
                None => Json::Null,
            },
        );
    }
    Ok(out)
}

/// One trace row (matching `run_trace_schema`).
fn run_trace_json(trace: &sc_workflow::RunTrace) -> Json {
    json!({
        "id": trace.id,
        "seq": trace.seq,
        "step": trace.step,
        "started_at": trace.started_at,
        "finished_at": trace.finished_at,
        "attempt": trace.attempt,
        "outcome": trace.outcome.as_str(),
        "error": trace.error,
        "context": Json::Object(trace.context.clone()),
    })
}

/// The form a suspended run is waiting on, lowered to the `FormField`
/// vocabulary the admin UI already renders every other settings form from.
///
/// Lowered here rather than sent as the stored declaration, so `SettingsFields`
/// renders an approval form with no knowledge that workflows exist — and so a
/// field whose declared type nothing recognises is reported *here*, where the
/// admin can read it, rather than as a control nobody can fill in.
fn pending_form_json(form: &sc_workflow::PendingForm) -> Result<Json> {
    let fields = form
        .fields
        .iter()
        .map(|f| f.to_form_field())
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({
        "fields": fields.iter().map(form_field_json).collect::<Vec<_>>(),
        "assign_to": form.assign_to,
        "min_role": form.min_role,
    }))
}

/// How many runs a page holds when the caller does not say.
const DEFAULT_RUN_PAGE: usize = 50;

/// A non-negative count from a query parameter, or `fallback` for one that is
/// absent, blank or not a number.
///
/// Lenient on purpose, and only here: a paging parameter a client got wrong is
/// not worth failing a list over, and the failure it would cause — a screen that
/// shows nothing — is worse than a page of the default size.
fn query_count(ctx: &crate::handler::HandlerCtx, name: &str, fallback: usize) -> usize {
    ctx.query_get(name)
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(fallback)
}

/// The trigger of this id, refusing one whose body is an **action**.
///
/// By id, and strict about the body: the workflow endpoints all address a
/// trigger, and answering an empty canvas for a trigger that runs one action
/// would invite an admin to draw a program nothing would ever run.
async fn require_workflow_trigger(catalog: &Catalog, id: &str) -> Result<Trigger> {
    let id = parse_trigger_id(id)?;
    let trigger = load_trigger(catalog, id)
        .await?
        .ok_or_else(|| Error::not_found(format!("no trigger with id {id}")))?;
    if trigger.body != TriggerBody::Workflow {
        return Err(Error::invalid(format!(
            "trigger `{}` runs an action, not a workflow",
            trigger.name
        )));
    }
    Ok(trigger)
}

/// The posted document as a [`Workflow`](sc_workflow::Workflow) of `id`.
///
/// The document's own `id` and `version` are **the server's**: a payload does not
/// get to reassign which workflow it is, and which version it is, is the store's
/// answer (`save_workflow` reads the maximum and mints the next). Everything else
/// is read strictly through the stored serde shape, so what the editor may send
/// and what the engine will run cannot drift apart.
fn workflow_from_document(
    id: sc_action::TriggerId,
    posted: &Json,
) -> Result<sc_workflow::Workflow> {
    let mut document = match posted {
        Json::Object(fields) => fields.clone(),
        _ => return Err(Error::invalid("field `workflow` must be a workflow object")),
    };
    document.insert("id".to_owned(), json!(id.0));
    document.insert("version".to_owned(), json!(1));
    serde_json::from_value(Json::Object(document))
        .map_err(|e| Error::invalid(format!("this is not a workflow the engine can read: {e}")))
}

/// One workflow as the editor reads it (matching `workflow_schema`): the
/// program, the checks against it, and the history behind it.
async fn workflow_json(
    catalog: &Catalog,
    dispatcher: &sc_action::TriggerDispatcher,
    trigger: &Trigger,
    workflow: &sc_workflow::Workflow,
    versions: &[sc_workflow::WorkflowVersion],
) -> Result<Json> {
    let issues = sc_workflow::workflow_issues(
        catalog,
        &dispatcher.registry(),
        workflow,
        trigger.channel.as_deref(),
    )
    .await?;
    Ok(json!({
        "id": trigger.id.0,
        "name": trigger.name,
        "channel": trigger.channel,
        "version": workflow.version,
        "workflow": serde_json::to_value(workflow).unwrap_or(Json::Null),
        "issues": issues
            .iter()
            .map(|issue| json!({ "step": issue.step, "problem": issue.problem }))
            .collect::<Vec<_>>(),
        "versions": versions
            .iter()
            .map(|v| json!({
                "version": v.version,
                "description": v.description,
                "created_at": v.created_at,
                "created_by": v.created_by,
            }))
            .collect::<Vec<_>>(),
    }))
}

/// The agents that call through the LLM provider `id`, by name — what a delete
/// has to refuse over.
///
/// By the provider's **name**, because that is what an agent stores: an agent
/// references a provider the way a trigger references an action, so a provider
/// renamed out from under one is already a broken agent and deleting the row is
/// the same question.
async fn agents_using_provider(catalog: &Catalog, id: LlmProviderDefId) -> Result<Vec<String>> {
    let Some(def) = load_llm_provider(catalog, id).await? else {
        return Ok(Vec::new());
    };
    // No `_sc_agents` table *means* no agent has ever been defined — the same
    // reading `Agents::load` takes — so a server without agents installed
    // deletes a provider rather than failing over a table nobody made.
    if catalog.get(sc_agent::AGENTS_TABLE)?.is_none() {
        return Ok(Vec::new());
    }
    Ok(sc_agent::list_agents(catalog)
        .await?
        .into_iter()
        .filter(|agent| agent.provider.trim() == def.name.trim())
        .map(|agent| format!("agent `{}`", agent.name))
        .collect())
}

/// The agent services this server was built with, or a configuration error.
///
/// Fails loudly rather than answering with an empty list, exactly as
/// [`triggers_of`] does: a process that never installed agents cannot list or
/// save one, and pretending there are none would make a save look like it
/// worked.
pub(crate) fn agents_of(apps: &AppMounts) -> Result<crate::AgentServices> {
    apps.agents().cloned().ok_or_else(|| {
        Error::config("this server has no agents installed, so agents cannot be managed")
    })
}

/// The trigger dispatcher this server was built with, or a configuration error.
///
/// Fails loudly rather than answering with an empty list: a process that never
/// installed triggers (a test, an admin-only server assembled by hand) cannot
/// list, save or run one, and pretending there are none would make a save look
/// like it worked.
pub(crate) fn triggers_of(apps: &AppMounts) -> Result<Arc<sc_action::TriggerDispatcher>> {
    apps.triggers().cloned().ok_or_else(|| {
        Error::config(
            "this server has no trigger dispatcher installed, so triggers cannot be managed",
        )
    })
}

/// Re-project every mounted app that exposes a trigger, after a **restore** —
/// the one caller left, now that an ordinary trigger change re-projects through
/// the dispatcher's [`TriggerObserver`](sc_action::TriggerObserver) instead
/// ([`AppMounts::refresh_triggers`]).
///
/// **Reported, not returned**, which is the one place this differs from the
/// table-settings handlers. An app can genuinely stop projecting: deleting a
/// trigger an app exposes is allowed (blocking it would leave an admin unable to
/// remove a trigger they no longer want), and it makes that app's declaration
/// dangle. The trigger *is* deleted at that point, so failing the request would
/// report the opposite of what happened; the app keeps its previous mount, the
/// operator gets the reason, and the app names the missing trigger when it is
/// next built or mounted.
fn reproject_apps(apps: &AppMounts) {
    if let Err(e) = apps.refresh_triggers() {
        eprintln!(
            "saltcorn: the trigger set changed, but an application could not be \
             re-projected and keeps its previous mount: {}",
            sc_error::format_chain(&e)
        );
    }
}

/// One stored trigger as JSON, with the reason it is not usable when there is
/// one (§10.2 — a broken trigger stays listed and editable).
pub(crate) fn trigger_json(trigger: &Trigger, problem: Option<String>) -> Json {
    json!({
        "id": trigger.id.0,
        "name": trigger.name,
        "description": trigger.description,
        "when": trigger.when.as_str(),
        "channel": trigger.channel,
        "only_if": trigger.only_if,
        // Which engine runs it, and — for an action body — what it runs with.
        // A workflow answers `null` for both: its steps are a version of their
        // own, fetched with `getWorkflow` (§10.3).
        "body": trigger.body.as_str(),
        "action": trigger.action(),
        "configuration": trigger.configuration().map(|c| Json::Object(c.clone().into_iter().collect())),
        "min_role": trigger.min_role,
        "enabled": trigger.is_enabled(),
        // The timing, read back the way it was posted: absent is null, not 0, so
        // "never set" and "set to midnight" stay distinguishable in the form.
        "minute": trigger.attributes.get(ATTR_MINUTE),
        "hour": trigger.attributes.get(ATTR_HOUR),
        "day_of_week": trigger.attributes.get(ATTR_DAY_OF_WEEK),
        "error": problem,
        // The scheduler's record, RFC 3339 — what the list shows so an admin can
        // see that a nightly job is actually running.
        "last_run_at": trigger.last_run_at.map(|t| t.to_rfc3339()),
    })
}

/// A trigger from a request body, under the id the caller's route decided.
///
/// Only the shape is checked here — that the fields are present and of the right
/// kind. Whether the *values* mean anything (the event exists, the table exists,
/// the action is registered and configured the way it declares, the `only_if`
/// resolves) is `validate_trigger`'s, called by `save_trigger`, so there is one
/// authority for it and the admin gets the same message the loader would.
pub(crate) fn trigger_from_body(id: TriggerId, body: &Json) -> Result<Trigger> {
    let obj = require_object(body)?;
    let when = EventKind::parse(non_empty_str_field(obj, "when")?)?;
    // The body decides what the rest of the request means: an `action` body
    // needs an action name and may carry a configuration, and a `workflow` body
    // has neither. Absent is `action`, because that is what every trigger was
    // before §10.3 and what a client that has not heard of workflows sends.
    let kind = obj
        .get("body")
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .unwrap_or(TriggerBody::ACTION);
    let action = match kind {
        TriggerBody::WORKFLOW => None,
        _ => Some(non_empty_str_field(obj, "action")?.to_owned()),
    };
    let configuration = match obj.get("configuration").filter(|v| !v.is_null()) {
        Some(Json::Object(config)) => config.clone().into_iter().collect(),
        Some(_) => return Err(Error::invalid("field `configuration` must be an object")),
        None => sc_types::Attrs::new(),
    };
    // One reading of the three, the same one the stored row goes through, so the
    // API refuses exactly what the loader refuses.
    let body = TriggerBody::parse(kind, action.as_deref(), configuration)?;
    let mut trigger = Trigger::with_body(id, non_empty_str_field(obj, "name")?, when, body)
        .description(optional_str(obj, "description"));
    // Absent, null, or blank are all "no channel" — a form posts the empty
    // string for a picker it did not show.
    if let Some(channel) = obj.get("channel").and_then(Json::as_str)
        && !channel.trim().is_empty()
    {
        trigger = trigger.on(channel.trim());
    }
    if let Some(only_if) = obj.get("only_if").and_then(Json::as_str)
        && !only_if.trim().is_empty()
    {
        trigger = trigger.only_if(only_if.trim());
    }
    if let Some(min_role) = obj.get("min_role").filter(|v| !v.is_null()) {
        let raw = min_role
            .as_i64()
            .ok_or_else(|| Error::invalid("field `min_role` must be a number"))?;
        trigger = trigger.min_role(u8::try_from(raw).map_err(|_| {
            Error::invalid(format!(
                "`min_role` must be a role between 1 and 100, got {raw}"
            ))
        })?);
    }
    // Absent means enabled, as the stored attribute does: a trigger is created
    // to run.
    if obj.get("enabled").and_then(Json::as_bool) == Some(false) {
        trigger.set_enabled(false);
    }
    // The periodic timing (§10.2). A null or absent value is *not* stored, which
    // is what keeps the attributes sparse (§9) and what lets `Schedule::of`
    // refuse a value on a kind that has no use for it — a form posts null for an
    // input it did not show, exactly as it does for `channel`.
    for key in [ATTR_MINUTE, ATTR_HOUR, ATTR_DAY_OF_WEEK] {
        if let Some(value) = obj.get(key).filter(|v| !v.is_null()) {
            let n = value
                .as_u64()
                .ok_or_else(|| Error::invalid(format!("field `{key}` must be a whole number")))?;
            trigger.attributes.insert(key.to_owned(), json!(n));
        }
    }
    // `last_run_at` is deliberately not read: it is the scheduler's record of
    // what happened, and an edit must not be able to rewrite history.
    Ok(trigger)
}

fn parse_trigger_id(raw: &str) -> Result<TriggerId> {
    uuid::Uuid::parse_str(raw)
        .map(TriggerId)
        .map_err(|_| Error::invalid(format!("`{raw}` is not a valid trigger id")))
}

fn parse_app_id(raw: &str) -> Result<AppId> {
    uuid::Uuid::parse_str(raw)
        .map(AppId)
        .map_err(|_| Error::invalid(format!("`{raw}` is not a valid application id")))
}

fn parse_user_id(raw: &str) -> Result<uuid::Uuid> {
    uuid::Uuid::parse_str(raw)
        .map_err(|_| Error::invalid(format!("`{raw}` is not a valid user id")))
}

/// The `role` of a user body, narrowed to the `1..=100` byte.
fn user_role_field(obj: &Map<String, Json>) -> Result<u8> {
    let role = int_field(obj, "role")?;
    u8::try_from(role).map_err(|_| Error::invalid(format!("role {role} is not in 1..=100")))
}

/// The `extra` bag of a user body: the columns the admin has added to the users
/// table, coerced to each column's type.
///
/// Unknown columns and system columns are both refused — the first by
/// [`rows::column_value`], which is the same coercion every row write goes
/// through, and the second by `sc-auth`, since each system column has its own way
/// in. A body with no `extra` at all is an empty bag, not an error: a users table
/// nobody has added a column to is the ordinary case.
fn user_extra_values(users: &Table, obj: &Map<String, Json>) -> Result<BTreeMap<String, Value>> {
    let Some(extra) = obj.get("extra") else {
        return Ok(BTreeMap::new());
    };
    if extra.is_null() {
        return Ok(BTreeMap::new());
    }
    let extra = require_object(extra)?;
    let mut out = BTreeMap::new();
    for (column, json) in extra {
        if sc_auth::is_system_user_column(column) {
            return Err(Error::invalid(format!(
                "`{column}` is not an admin-defined field of the users table"
            )));
        }
        out.insert(column.clone(), rows::column_value(users, column, json)?);
    }
    Ok(out)
}

/// Refuse an operation an admin is aiming at their own account.
///
/// Disabling or deleting yourself is a request to lock yourself out mid-session,
/// and the users screen is reached only *through* that session. Nothing about it
/// is unsafe for the system — an installation can have another admin do it — so
/// this is a guard rail, not an access rule, and it names the account rather than
/// pretending the operation does not exist.
fn refuse_self(ctx: &crate::handler::HandlerCtx, id: uuid::Uuid, verb: &str) -> Result<()> {
    if ctx.user.as_ref().is_some_and(|u| u.id == id) {
        return Err(Error::invalid(format!(
            "you cannot {verb} the account you are signed in as"
        )));
    }
    Ok(())
}

/// The bundler's combined output, for the build log: stdout then stderr.
fn build_log(report: &sc_app::BuildReport) -> String {
    let mut log = String::new();
    // The first build of a scaffolded app is mostly the install, and an admin
    // watching a build that takes a minute needs to see why (§2.3).
    if let Some(install) = &report.install_log {
        log.push_str(install);
        if !log.is_empty() && !log.ends_with('\n') {
            log.push('\n');
        }
    }
    log.push_str(&report.stdout);
    if !report.stderr.is_empty() {
        if !log.is_empty() && !log.ends_with('\n') {
            log.push('\n');
        }
        log.push_str(&report.stderr);
    }
    log
}

/// The settings screen's whole payload: what may be set, and what is set
/// (matching `settings_schema`).
///
/// The values are [`redacted`](sc_types::redact_attrs) here, at the one place
/// they are serialised, so a secret setting cannot leak through a second reader
/// added later — the rule §11.1 states for an API key, applied to the private
/// key an admin pastes into the TLS section.
async fn settings_json(catalog: &Catalog) -> Result<Json> {
    let sections: Vec<Json> = sc_config::config_sections()
        .iter()
        .map(|section| {
            json!({
                "name": section.name,
                "label": section.label,
                "description": section.description,
                "fields": section
                    .fields
                    .iter()
                    .map(|def| {
                        let mut field = form_field_json(&def.field);
                        if let Json::Object(map) = &mut field {
                            map.insert("help".to_owned(), Json::from(def.help));
                        }
                        field
                    })
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    let values = sc_types::redact_attrs(
        &sc_config::config_spec(),
        &sc_config::all_config(catalog).await?,
    );
    Ok(json!({ "sections": sections, "values": Json::Object(values) }))
}

/// The selection an admin last made, or "everything" when they never have.
async fn stored_backup_preferences(catalog: &Catalog) -> Result<crate::backup::BackupPreferences> {
    let stored = sc_config::stored_config(catalog, sc_config::BACKUP_INCLUDE).await?;
    Ok(match stored {
        Some(value) => crate::backup::BackupPreferences::from_json(&value),
        None => crate::backup::BackupPreferences::default(),
    })
}

/// What a downloaded backup is called: the date and time it was taken, to the
/// second, so a directory of them sorts chronologically and two taken the same
/// afternoon do not overwrite each other.
fn backup_filename() -> String {
    format!(
        "saltcorn-backup-{}.zip",
        chrono::Utc::now().format("%Y-%m-%d-%H%M%S")
    )
}

/// A [`FormField`] as the API returns it (matching `form_field_schema`): enough
/// for the admin UI to render and label a control for it.
fn form_field_json(field: &FormField) -> Json {
    let type_name = field
        .base
        .type_
        .as_basic()
        .map(|b| b.name())
        .unwrap_or("text");
    json!({
        "name": field.base.name,
        "label": field.base.label,
        "type": type_name,
        "required": field.required,
        "default": field.default.clone().unwrap_or(Json::Null),
        "options": field.static_options(),
        "multiline": field.multiline,
        "secret": field.secret,
        "create_only": field.create_only,
        "code_language": field.code_language,
    })
}

/// An [`Operation`] as the API returns it (matching `operation_schema`): what a
/// backend can be asked to *do*, in the same declared-as-data shape its settings
/// use.
fn operation_json(op: &Operation) -> Json {
    json!({
        "name": op.name,
        "label": op.label,
        "description": op.description,
        "scope": match op.scope {
            OperationScope::Configure => "configure",
            OperationScope::Instance => "instance",
        },
        "input_spec": op.input_spec.iter().map(form_field_json).collect::<Vec<_>>(),
        "on_create": op.on_create,
        "automatic": op.automatic,
    })
}

/// A [`FileStore`](sc_files::FileStore) directory listing entry as JSON, matching
/// the API's `file_entry_schema` (`name`, `path`, `is_dir`, `size?`).
fn entry_json(entry: &Entry) -> Json {
    json!({
        "name": entry.name,
        "path": entry.path,
        "is_dir": entry.is_dir,
        "size": entry.size,
    })
}

/// The entry JSON returned after a successful `writeFile`: a file (never a
/// directory) at `path` with the byte count just written.
fn file_entry_written_json(path: &str, size: usize) -> Json {
    let name = path.rsplit('/').next().unwrap_or(path);
    json!({
        "name": name,
        "path": path,
        "is_dir": false,
        "size": size,
    })
}

/// Decode a `writeFile` body's contents: exactly one of `base64` (arbitrary
/// bytes) or `text` (UTF-8) must be present.
fn file_body_bytes(obj: &Map<String, Json>) -> Result<Bytes> {
    let base64 = obj.get("base64").and_then(Json::as_str);
    let text = obj.get("text").and_then(Json::as_str);
    match (base64, text) {
        (Some(_), Some(_)) => Err(Error::invalid(
            "provide either `base64` or `text`, not both",
        )),
        (Some(encoded), None) => BASE64
            .decode(encoded)
            .map(Bytes::from)
            .map_err(|e| Error::invalid(format!("invalid base64 in `base64`: {e}"))),
        (None, Some(text)) => Ok(Bytes::from(text.to_owned().into_bytes())),
        (None, None) => Err(Error::invalid(
            "missing file contents: set `base64` or `text`",
        )),
    }
}

// --- body accessors ------------------------------------------------------------

/// A required string field of an object body.
/// A table as the admin UI sees it: its name plus the `_sc_tables` overlay
/// merged onto it (§9).
///
/// `configured` is the overlay's *presence*, not its content. A table an admin
/// deliberately set to admin-only and one nobody has ever opened both report
/// `1`/`1`; only the first has a row, and only the first can be "forgotten".
pub(crate) fn table_json(catalog: &Catalog, table: &Table, rls_available: bool) -> Json {
    json!({
        "name": table.name,
        "label": table.label,
        "description": table.description,
        "min_role_read": table.access.min_role_read,
        "min_role_write": table.access.min_role_write,
        "configured": table.overlay.is_some(),
        // The live formula's source — or, when the stored source failed to
        // parse/validate at merge time, still that source (from attributes) so
        // the admin edits what they typed, with `ownership_error` saying what
        // is wrong and `ownership` granting nothing meanwhile (fail closed).
        "ownership_formula": table.ownership.as_ref().map(|f| f.source().to_owned())
            .or_else(|| table.attributes.get(ATTR_OWNERSHIP_FORMULA)
                .and_then(Json::as_str).map(str::to_owned))
            .unwrap_or_default(),
        "ownership_error": table.ownership_error,
        "rls_enabled": table.rls_enabled,
        // The backend's capability *and* whose database this is: RLS is DDL
        // against the primary driver, so it is never available on a table a
        // connection contributed, nor on a **provided** table, whose rows are
        // not in any database. The SPA renders the toggle from this, and a
        // toggle that can only ever be refused is not a setting, it is a trap.
        "rls_available": rls_available
            && table.database == sc_catalog::DbId::primary()
            && table.provider().is_none(),
        // Which database hosts it: `primary` for Saltcorn's own, otherwise the
        // name of the connection an admin added. The list screen shows anything
        // that is not the primary as a badge beside the table's name, because a
        // list mixing two databases and saying so nowhere would be a list an
        // admin could act on wrongly.
        "database": table.database.0,
        // The table provider serving its rows, or null (§8.3).
        "provider": provided_json(catalog, table),
    })
}

/// A provided table's definition as the table page reads it: which provider,
/// what it was configured with, what it asks for, and what is wrong.
///
/// `null` for a table in a database, which is most of them.
///
/// The **declaration travels with the values** because the settings form on the
/// table's own page needs both and one round trip is what it has — and because
/// the declaration is not stored anywhere: it is read from the package at load,
/// so a module upgraded this morning asks for what it asks for now rather than
/// for what it asked for when the table was made.
fn provided_json(catalog: &Catalog, table: &Table) -> Json {
    let Some((module, provider)) = table.provider() else {
        return Json::Null;
    };
    let config_spec = catalog
        .table_provider_kinds()
        .into_iter()
        .find(|k| k.module == module && k.provider == provider)
        .map(|k| k.config_spec)
        .unwrap_or_default();
    let issues: Vec<String> = catalog
        .provided_table_issues()
        .into_iter()
        .filter(|issue| issue.table == table.name)
        .map(|issue| issue.problem)
        .collect();
    json!({
        "module": module,
        "provider": provider,
        // Which of v1's three write methods `get_table` answered for these
        // settings (§8.3). The data screens draw their buttons from it: a
        // provider configured read-only offers a viewer, and one that answers
        // `insertRow` and nothing else offers "Add row" without "Delete".
        "writes": {
            "insert": table.provided_writes().insert,
            "update": table.provided_writes().update,
            "delete": table.provided_writes().delete,
        },
        // Redacted the way every other secret is: a provider's `password` field
        // comes back as the sentinel, and submitting it unchanged keeps what is
        // stored (`sc_types::merge_secrets`, applied on the way in).
        "configuration": Json::Object(sc_types::redact_attrs(
            &config_spec,
            &provided_configuration(table),
        )),
        "config_spec": config_spec.iter().map(form_field_json).collect::<Vec<_>>(),
        "issues": issues,
    })
}

/// The configuration stored on a provided table's row.
fn provided_configuration(table: &Table) -> sc_types::Attrs {
    match table.attributes.get(sc_catalog::ATTR_PROVIDER_CONFIG) {
        Some(Json::Object(map)) => map.clone(),
        _ => sc_types::Attrs::new(),
    }
}

/// The caller an admin row endpoint runs as: **role 1** — which clears every
/// policy's role floor, so the admin's own row editor sees and edits every row of
/// a FORCE'd table — carrying the signed-in admin's fields.
///
/// It used to be `None` off an RLS table, because a caller context meant only
/// "route this through a policy transaction". It no longer does: `rows` decides
/// that from the table, and the context is *who is writing* — which the table
/// event a write raises has to report (§10.2). The role is explicit rather than
/// the user's own for the reason it always was: this endpoint is admin-only, and
/// the row editor must work on a FORCE'd table.
fn admin_caller(user: Option<&sc_auth::User>) -> sc_catalog::CallerContext {
    sc_api::caller_context_at(ROLE_ADMIN, user)
}

/// A required role field of an object body: an integer on the `1..=100` scale.
///
/// Rejects rather than clamps, for the reason the storage layer does: the
/// nearest legal role is still a decision about who reaches the data, and it is
/// not one the server gets to make on the admin's behalf.
fn role_field(obj: &Map<String, Json>, key: &str) -> Result<u8> {
    let raw = int_field(obj, key)?;
    u8::try_from(raw)
        .ok()
        .filter(|r| (ROLE_ADMIN..=ROLE_PUBLIC).contains(r))
        .ok_or_else(|| {
            Error::invalid(format!(
                "field `{key}` must be a role between 1 and 100, got {raw}"
            ))
        })
}

/// One role on the wire (§7.1, §9).
///
/// `builtin` travels with the role so the UI can decline to offer a delete it
/// would only be refused for — admin and public are what "administer" and
/// "anonymous caller" mean, and neither is an installation's to remove.
pub(crate) fn role_json(role: &Role) -> Json {
    json!({
        "role": role.role,
        "name": role.name,
        "description": role.description,
        "builtin": role.is_builtin(),
    })
}

fn str_field<'a>(obj: &'a Map<String, Json>, key: &str) -> Result<&'a str> {
    obj.get(key)
        .and_then(Json::as_str)
        .ok_or_else(|| Error::invalid(format!("missing or non-string field `{key}`")))
}

/// A required, non-empty (after trimming) string field.
fn non_empty_str_field<'a>(obj: &'a Map<String, Json>, key: &str) -> Result<&'a str> {
    let value = str_field(obj, key)?;
    if value.trim().is_empty() {
        return Err(Error::invalid(format!("field `{key}` must not be empty")));
    }
    Ok(value)
}

/// An optional string field: the value if present and a string, else "". A
/// present-but-non-string value is treated as absent, which is what an optional
/// wire field means.
fn optional_str(obj: &Map<String, Json>, key: &str) -> String {
    obj.get(key)
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// An optional string field distinguishing "absent/null" (→ `None`) from a
/// present string (→ `Some`) — for a kind parameter like a folder or summary
/// field. A present non-string is an error.
fn optional_present_str(obj: &Map<String, Json>, key: &str) -> Option<String> {
    match obj.get(key) {
        Some(Json::String(s)) => Some(s.clone()),
        _ => None,
    }
}

/// An optional boolean field, defaulting to `false` when absent or null; a
/// present non-boolean is an error.
fn optional_bool(obj: &Map<String, Json>, key: &str) -> Result<bool> {
    match obj.get(key) {
        None | Some(Json::Null) => Ok(false),
        Some(Json::Bool(b)) => Ok(*b),
        Some(_) => Err(Error::invalid(format!("field `{key}` must be a boolean"))),
    }
}

/// An optional JSON-object field read as an [`Attrs`] bag, empty when absent or
/// null; a present non-object is an error.
fn attributes_field(obj: &Map<String, Json>) -> Result<Attrs> {
    match obj.get("attributes") {
        None | Some(Json::Null) => Ok(Attrs::new()),
        Some(Json::Object(map)) => Ok(map.clone()),
        Some(_) => Err(Error::invalid("`attributes` must be an object")),
    }
}

/// An optional array-of-strings field, empty when absent or null; a present value
/// that is not an array of strings is an error.
fn string_array_field(obj: &Map<String, Json>, key: &str) -> Result<Vec<String>> {
    match obj.get(key) {
        None | Some(Json::Null) => Ok(Vec::new()),
        Some(Json::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| Error::invalid(format!("`{key}` must be an array of strings")))
            })
            .collect(),
        Some(_) => Err(Error::invalid(format!(
            "`{key}` must be an array of strings"
        ))),
    }
}

/// A required integer field of an object body.
fn int_field(obj: &Map<String, Json>, key: &str) -> Result<i64> {
    obj.get(key)
        .and_then(Json::as_i64)
        .ok_or_else(|| Error::invalid(format!("missing or non-integer field `{key}`")))
}

/// A required boolean field of an object body.
fn bool_field(obj: &Map<String, Json>, key: &str) -> Result<bool> {
    obj.get(key)
        .and_then(Json::as_bool)
        .ok_or_else(|| Error::invalid(format!("missing or non-boolean field `{key}`")))
}
