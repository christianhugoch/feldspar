//! The admin UI's API, expressed as a fixed [`EndpointSet`] (design §13.1).
//!
//! The admin API is compile-time-known, but rather than a bespoke statically
//! typed router it is built as a set of constant [`Endpoint`] values fed through
//! the **same** [`EndpointSet`] machinery an application uses. This maximises
//! reuse: `ui/admin` consumes a generated typed client (see
//! [`crate::typescript`]) exactly as an application would, and `sc-server`
//! mounts these endpoints the same way it mounts a runtime application's.
//!
//! The [`HandlerRef::Named`] handlers here are resolved by `sc-server` when it
//! mounts the set (the "Server" subphase of Phase 6). This module defines the
//! *contract* — methods, paths, schemas, and auth — that both the server and the
//! generated client are held to.

use crate::auth::{credentials_schema, user_row_schema, user_summary_schema};
use crate::endpoint::{AuthRequirement, Endpoint, EndpointSet, Method, PathSpec};
use crate::schema::{StructField, TypeSchema, ValueType};

/// Path prefix every admin endpoint is mounted under.
pub const ADMIN_API_PREFIX: &str = "api";

/// The full set of admin API endpoints.
///
/// Grouped as: bootstrap/auth (first-user, login, logout, whoami), catalog
/// (tables + fields), row CRUD, and user management. Each maps to a
/// [`HandlerRef::Named`] the server resolves at mount time.
pub fn admin_endpoints() -> EndpointSet {
    let mut set = EndpointSet::new();

    // --- bootstrap & auth ---------------------------------------------------

    // Whether any user exists yet — drives the create-first-user screen.
    set.register(
        Endpoint::new("authStatus", Method::Get, api().lit("auth/status"))
            .output(TypeSchema::struct_of([
                StructField::new("any_user_exists", TypeSchema::bool()),
                StructField::new("current_user", TypeSchema::optional(user_summary_schema())),
            ]))
            .auth(AuthRequirement::Public),
    );

    // Create the very first (admin) user. Only valid while no user exists.
    set.register(
        Endpoint::new("createFirstUser", Method::Post, api().lit("first-user"))
            .input(credentials_schema())
            .output(user_summary_schema())
            .auth(AuthRequirement::Public),
    );

    set.register(
        Endpoint::new("login", Method::Post, api().lit("login"))
            .input(credentials_schema())
            .output(user_summary_schema())
            .auth(AuthRequirement::Public),
    );

    set.register(
        Endpoint::new("logout", Method::Post, api().lit("logout")).auth(AuthRequirement::LoggedIn),
    );

    // --- catalog: tables & fields ------------------------------------------

    set.register(
        Endpoint::new("listTables", Method::Get, api().lit("tables"))
            .output(TypeSchema::array(table_schema()))
            .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new("createTable", Method::Post, api().lit("tables"))
            .input(TypeSchema::struct_of([StructField::new(
                "name",
                TypeSchema::text(),
            )]))
            .output(table_schema())
            .auth(AuthRequirement::admin()),
    );

    // Create a table **from a CSV file**: the fields deduced from the header and
    // the values under it, then every row imported (§13.1). A separate endpoint
    // rather than an optional `csv` on `createTable`, because it is a different
    // operation with a different failure mode — this one can fail *after* the
    // table exists, and answers by dropping it, which is not something the plain
    // create can do.
    set.register(
        Endpoint::new(
            "createTableFromCsv",
            Method::Post,
            api().lit("tables").lit("csv"),
        )
        .input(TypeSchema::struct_of([
            StructField::new("name", TypeSchema::text()),
            StructField::new("csv", TypeSchema::text()),
        ]))
        .output(TypeSchema::struct_of([
            StructField::new("table", table_schema()),
            StructField::new("inserted", TypeSchema::int()),
        ]))
        .auth(AuthRequirement::admin()),
    );

    // Set a table's configuration: the `_sc_tables` overlay fields, and only
    // those (§9). A `PUT` on the table's own path rather than a nested
    // `…/settings` resource, because from the admin's side there is one table
    // with settings, not a table plus a settings object hanging off it — and
    // renaming, the other thing a `PUT` on a table might mean, is a schema
    // change with references to chase and is deliberately not offered.
    set.register(
        Endpoint::new(
            "updateTable",
            Method::Put,
            api().lit("tables").param("table", ValueType::Text),
        )
        .input(table_settings_schema())
        .output(table_schema())
        .auth(AuthRequirement::admin()),
    );

    // Drop a table: its columns, its rows and its overlay row. Distinct from
    // `deleteTableSettings`, which forgets a *configuration* and leaves the table
    // exactly where it was — the two verbs are a `DELETE` apart on purpose, and
    // the settings one carries the `/settings` suffix because it is the narrower
    // of the pair.
    //
    // It exists because an agent can now do this (§11.3), and an agent must not
    // be able to do something the admin UI cannot.
    set.register(
        Endpoint::new(
            "dropTable",
            Method::Delete,
            api().lit("tables").param("table", ValueType::Text),
        )
        .output(TypeSchema::struct_of([StructField::new(
            "dropped",
            TypeSchema::text(),
        )]))
        .auth(AuthRequirement::admin()),
    );

    // Forget a table's configuration, returning it to the closed default. Also
    // the way an *orphan* row — one whose table is gone (§1.1) — is cleaned up,
    // which is why the path is addressed by name and does not require the table
    // to exist.
    set.register(
        Endpoint::new(
            "deleteTableSettings",
            Method::Delete,
            api()
                .lit("tables")
                .param("table", ValueType::Text)
                .lit("settings"),
        )
        .output(TypeSchema::struct_of([StructField::new(
            "deleted",
            TypeSchema::bool(),
        )]))
        .auth(AuthRequirement::admin()),
    );

    // Stored settings whose table is not in the database. These are deliberately
    // kept rather than deleted (§1.1) — a restore or an external migration can
    // drop and recreate a table, and the configuration must be waiting when it
    // returns — so something has to be able to *show* them, or "kept" becomes
    // "invisible and inexplicable".
    set.register(
        Endpoint::new(
            "listOrphanTableSettings",
            Method::Get,
            api().lit("table-settings").lit("orphans"),
        )
        .output(TypeSchema::array(orphan_table_settings_schema()))
        .auth(AuthRequirement::admin()),
    );

    // --- roles --------------------------------------------------------------
    // A role is a row in `_sc_roles` (§7.1, §9), not a bare integer: it carries
    // a name and, in `attributes`, whatever role-specific settings arrive later.
    // `users.role` is a foreign key onto it, so creating a role is a
    // prerequisite for assigning a user to it — which is why creating and
    // deleting roles has to be reachable from the admin UI and not only from a
    // SQL prompt.

    set.register(
        Endpoint::new("listRoles", Method::Get, api().lit("roles"))
            .output(TypeSchema::array(role_schema()))
            .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new("createRole", Method::Post, api().lit("roles"))
            .input(TypeSchema::struct_of([
                StructField::new("role", TypeSchema::int()),
                StructField::new("name", TypeSchema::text()),
                StructField::new("description", TypeSchema::text()),
            ]))
            .output(role_schema())
            .auth(AuthRequirement::admin()),
    );

    // Addressed by the role *number*, not the row id: the number is what
    // `users.role` and every `min_role` holds, so it is the handle an admin
    // already has in front of them.
    set.register(
        Endpoint::new(
            "deleteRole",
            Method::Delete,
            api().lit("roles").param("role", ValueType::Int),
        )
        .output(TypeSchema::struct_of([StructField::new(
            "deleted",
            TypeSchema::bool(),
        )]))
        .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new(
            "listFields",
            Method::Get,
            api()
                .lit("tables")
                .param("table", ValueType::Text)
                .lit("fields"),
        )
        .output(TypeSchema::array(field_schema()))
        .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new(
            "createField",
            Method::Post,
            api()
                .lit("tables")
                .param("table", ValueType::Text)
                .lit("fields"),
        )
        .input(create_field_schema())
        .output(field_schema())
        .auth(AuthRequirement::admin()),
    );

    // Overlay-only edits: label, description, rich type, kind parameters,
    // attributes. Renaming or retyping a *column* is a schema change and is out
    // of scope (§3.3), so this endpoint cannot touch `name`, `required`, `unique`
    // or the storage SQL type.
    set.register(
        Endpoint::new(
            "updateField",
            Method::Put,
            api()
                .lit("tables")
                .param("table", ValueType::Text)
                .lit("fields")
                .param("field", ValueType::Text),
        )
        .input(field_settings_schema())
        .output(field_schema())
        .auth(AuthRequirement::admin()),
    );

    // Drop a field: the column, its data and its overlay row. Refused by name
    // when it is a primary key, a built-in column of `users`/`_sc_roles`, the
    // target of another table's key, or read by a calculated field — each of
    // which the database would otherwise refuse with an error nobody can act on.
    set.register(
        Endpoint::new(
            "deleteField",
            Method::Delete,
            api()
                .lit("tables")
                .param("table", ValueType::Text)
                .lit("fields")
                .param("field", ValueType::Text),
        )
        .output(TypeSchema::struct_of([StructField::new(
            "dropped",
            TypeSchema::text(),
        )]))
        .auth(AuthRequirement::admin()),
    );

    // The registered field types (basic, rich) and kinds (Key, File) with their
    // attribute specs, so the field editor can render a form for a type it knows
    // nothing about — the same contract `listFrameworks` has (§13.3).
    set.register(
        Endpoint::new("listFieldTypes", Method::Get, api().lit("field-types"))
            .output(TypeSchema::array(field_type_schema()))
            .auth(AuthRequirement::admin()),
    );

    // --- row CRUD -----------------------------------------------------------
    // Every row endpoint here is `admin()`, and that is **not** governed by a
    // table's `min_role_read`/`min_role_write`. Those rules are the table's
    // *application-facing* access (§7), enforced by an application's REST
    // provider (`sc_api::RestProvider`); this is the admin's own view of the
    // data, reached only by role 1 through the admin SPA. A table an admin
    // opened to role 80 for its application is still admin-only here — the two
    // are different surfaces onto the same rows, and reading this as an
    // oversight would be the mistake.

    set.register(
        Endpoint::new(
            "listRows",
            Method::Get,
            api()
                .lit("tables")
                .param("table", ValueType::Text)
                .lit("rows"),
        )
        .output(TypeSchema::array(TypeSchema::json()))
        .auth(AuthRequirement::admin()),
    );

    // How many rows there are, without reading them. The table page shows this
    // beside the link to the rows themselves, and a page that had to fetch every
    // row to print one number would cost what the data costs.
    set.register(
        Endpoint::new(
            "countRows",
            Method::Get,
            api()
                .lit("tables")
                .param("table", ValueType::Text)
                .lit("rows")
                .lit("count"),
        )
        .output(TypeSchema::struct_of([StructField::new(
            "count",
            TypeSchema::int(),
        )]))
        .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new(
            "createRow",
            Method::Post,
            api()
                .lit("tables")
                .param("table", ValueType::Text)
                .lit("rows"),
        )
        .input(TypeSchema::json())
        .output(TypeSchema::json())
        .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new(
            "updateRow",
            Method::Put,
            api()
                .lit("tables")
                .param("table", ValueType::Text)
                .lit("rows")
                .param("id", ValueType::Text),
        )
        .input(TypeSchema::json())
        .output(TypeSchema::json())
        .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new(
            "deleteRow",
            Method::Delete,
            api()
                .lit("tables")
                .param("table", ValueType::Text)
                .lit("rows")
                .param("id", ValueType::Text),
        )
        .auth(AuthRequirement::admin()),
    );

    // --- rows in bulk, as CSV ----------------------------------------------
    //
    // The document crosses as a **string** in a JSON envelope rather than as a
    // file download and a file upload. Both directions could have been raw
    // routes outside this set — as the binary file upload is — but neither has
    // to be: CSV is text, so it fits the endpoint model exactly, and keeping it
    // inside means the typed client carries both and the CSRF and auth
    // machinery applies without a second path to remember. The export names the
    // file it should be saved as, because the browser is the one doing the
    // saving and only the server knows the table.

    set.register(
        Endpoint::new(
            "exportTableCsv",
            Method::Get,
            api()
                .lit("tables")
                .param("table", ValueType::Text)
                .lit("csv"),
        )
        .output(TypeSchema::struct_of([
            StructField::new("filename", TypeSchema::text()),
            StructField::new("csv", TypeSchema::text()),
        ]))
        .auth(AuthRequirement::admin()),
    );

    // Every row that parses and validates is written; the rest come back as
    // messages naming their line. The three numbers are the answer — "it worked"
    // and "it failed" are both wrong for a file with three bad rows in it, and a
    // file naming primary keys **replaces** rows as well as adding them, which
    // an admin must be told apart from having added them all over again.
    set.register(
        Endpoint::new(
            "importTableCsv",
            Method::Post,
            api()
                .lit("tables")
                .param("table", ValueType::Text)
                .lit("csv"),
        )
        .input(TypeSchema::struct_of([StructField::new(
            "csv",
            TypeSchema::text(),
        )]))
        .output(TypeSchema::struct_of([
            StructField::new("inserted", TypeSchema::int()),
            StructField::new("updated", TypeSchema::int()),
            StructField::new("errors", TypeSchema::array(TypeSchema::text())),
        ]))
        .auth(AuthRequirement::admin()),
    );

    // --- file stores (configuration) ---------------------------------------
    // A file store, like an application, exists only as its stored row (§9,
    // §14.1); these endpoints are the row ⇄ connected-store path the SPA drives.
    // Create/update/delete manage the definition and keep the live registry in
    // step — a renamed store's old handle is disconnected, a deleted store's
    // handle too, so nothing goes on serving a store the admin has removed.
    //
    // Note the addressing split, which is deliberate: these operate on a store's
    // **id**, because the row's identity survives a rename, while the file
    // manager below operates on a store's **name**, because that is what an
    // admin picked and what everything else references.

    // Every *defined* store — not merely every connected one — with whether it
    // is currently connected and, if not, why. A store whose directory has been
    // unmounted must still be listed and editable: editing it is the repair.
    set.register(
        Endpoint::new("listFileStores", Method::Get, api().lit("file-stores"))
            .output(TypeSchema::array(file_store_schema()))
            .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new("createFileStore", Method::Post, api().lit("file-stores"))
            .input(file_store_input_schema())
            .output(file_store_schema())
            .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new(
            "updateFileStore",
            Method::Put,
            api().lit("file-stores").param("id", ValueType::Uuid),
        )
        .input(file_store_input_schema())
        .output(file_store_schema())
        .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new(
            "deleteFileStore",
            Method::Delete,
            api().lit("file-stores").param("id", ValueType::Uuid),
        )
        .output(TypeSchema::struct_of([StructField::new(
            "deleted",
            TypeSchema::bool(),
        )]))
        .auth(AuthRequirement::admin()),
    );

    // The registered backends with their settings spec, so the create/edit form
    // can render controls for a backend it knows nothing about — the same move
    // `listFrameworks` makes for frameworks (§13.3).
    set.register(
        Endpoint::new(
            "listFileStoreBackends",
            Method::Get,
            api().lit("file-store-backends"),
        )
        .output(TypeSchema::array(backend_info_schema()))
        .auth(AuthRequirement::admin()),
    );

    // --- backend operations -------------------------------------------------
    // Some backends offer *acts* as well as settings — a git store generates a
    // deploy key, clones, pulls, pushes and commits. Those are declared as data
    // (`Operation`, §6.2) exactly as settings are, and run through these two
    // endpoints, so the admin UI renders a button per declared operation and
    // knows nothing about any particular one. A backend supplied by a plugin
    // gets its buttons the same way a built-in one does; without this the UI
    // would need a branch per backend, and an operation would be something only
    // a built-in backend could have.
    //
    // There are two endpoints because there are two scopes, and the difference
    // is real rather than bookkeeping:

    // **Configure scope** — runs against configuration the admin is still
    // editing, so it is addressed by *backend name* and carries the unsaved
    // config in its body. This is what lets "generate a deploy key" happen
    // before the store exists, which it must: saving a git store clones it, and
    // cloning needs a key the remote already accepts. What comes back is the
    // config with the operation's changes merged in, for the form to adopt.
    set.register(
        Endpoint::new(
            "runBackendOperation",
            Method::Post,
            api()
                .lit("file-store-backends")
                .param("backend", ValueType::Text)
                .lit("operations")
                .param("operation", ValueType::Text),
        )
        .input(TypeSchema::struct_of([
            StructField::new("name", TypeSchema::text()),
            StructField::new("config", TypeSchema::json()),
            StructField::new("input", TypeSchema::json()),
        ]))
        .output(TypeSchema::struct_of([
            StructField::new("config", TypeSchema::json()),
            StructField::new("output", TypeSchema::text()),
            StructField::new("data", TypeSchema::optional(TypeSchema::json())),
        ]))
        .auth(AuthRequirement::admin()),
    );

    // **Instance scope** — runs against a saved store, addressed by id like the
    // rest of the configuration endpoints. Anything the operation changed in the
    // definition is persisted, and the store is reconnected afterwards, since an
    // operation may be exactly what makes it connectable (a clone).
    //
    // It works from the stored *definition*, not from a connected instance, and
    // that is the point: a git store that has never been cloned has no instance,
    // and cloning it is the operation that would otherwise be unreachable.
    set.register(
        Endpoint::new(
            "runFileStoreOperation",
            Method::Post,
            api()
                .lit("file-stores")
                .param("id", ValueType::Uuid)
                .lit("operations")
                .param("operation", ValueType::Text),
        )
        .input(TypeSchema::struct_of([StructField::new(
            "input",
            TypeSchema::json(),
        )]))
        .output(TypeSchema::struct_of([
            StructField::new("config", TypeSchema::json()),
            StructField::new("output", TypeSchema::text()),
            // Optional, and shaped by whichever backend filled it: `output` is
            // what every client renders, and this is for the one that has to act
            // on the result rather than show it — the IDE's source-control view,
            // which cannot list changed files from a paragraph of prose (§12.1).
            StructField::new("data", TypeSchema::optional(TypeSchema::json())),
            StructField::new("connected", TypeSchema::bool()),
        ]))
        .auth(AuthRequirement::admin()),
    );

    // --- LLM providers (configuration) --------------------------------------
    // A provider, like a file store, exists only as its stored row (§9, §11.1),
    // and these endpoints are that row's lifecycle. The shape is deliberately
    // the file stores' shape one crate over: id-addressed configuration, a
    // backends endpoint carrying each backend's declared settings so the form is
    // generic, and one act (`testLlmProvider`) that is not a save.
    //
    // The one thing that is *not* a copy is what the config carries. A provider's
    // config holds an API key, so every response redacts it
    // (`sc_types::redact_attrs`) and every save merges the sentinel back
    // (`merge_secrets`). That happens where the record is serialised, in the
    // handler, rather than in the screen — see §11.1.

    set.register(
        Endpoint::new("listLlmProviders", Method::Get, api().lit("llm-providers"))
            .output(TypeSchema::array(llm_provider_schema()))
            .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new(
            "createLlmProvider",
            Method::Post,
            api().lit("llm-providers"),
        )
        .input(llm_provider_input_schema())
        .output(llm_provider_schema())
        .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new(
            "updateLlmProvider",
            Method::Put,
            api().lit("llm-providers").param("id", ValueType::Uuid),
        )
        .input(llm_provider_input_schema())
        .output(llm_provider_schema())
        .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new(
            "deleteLlmProvider",
            Method::Delete,
            api().lit("llm-providers").param("id", ValueType::Uuid),
        )
        .output(TypeSchema::struct_of([StructField::new(
            "deleted",
            TypeSchema::bool(),
        )]))
        .auth(AuthRequirement::admin()),
    );

    // The registered backends with their settings spec, so the create/edit form
    // renders controls for a backend it knows nothing about — the same move
    // `listFileStoreBackends` and `listFrameworks` make.
    set.register(
        Endpoint::new(
            "listLlmProviderBackends",
            Method::Get,
            api().lit("llm-provider-backends"),
        )
        .output(TypeSchema::array(llm_backend_info_schema()))
        .auth(AuthRequirement::admin()),
    );

    // **Test connection.** Sends one trivial prompt and reports what came back,
    // or the provider's own error text. It exists because a wrong key is
    // otherwise discovered inside a chat transcript, which is the worst place
    // for it: the admin is no longer looking at the form, and the failure looks
    // like the agent rather than the configuration.
    //
    // It takes the *config in the body* rather than working from the saved row,
    // so a provider can be tested before it is saved — the same reason a git
    // store's deploy key is generated at `Configure` scope. A submitted secret
    // sentinel still resolves against what is stored, when there is a stored row
    // to resolve against, so testing an existing provider does not require
    // retyping its key.
    set.register(
        Endpoint::new(
            "testLlmProvider",
            Method::Post,
            api().lit("llm-provider-test"),
        )
        .input(TypeSchema::struct_of([
            StructField::new("id", TypeSchema::optional(TypeSchema::uuid())),
            StructField::new("backend", TypeSchema::text()),
            StructField::new("config", TypeSchema::json()),
            StructField::new("model", TypeSchema::optional(TypeSchema::text())),
        ]))
        .output(TypeSchema::struct_of([
            StructField::new("ok", TypeSchema::bool()),
            // The model's reply on success, the provider's own words on
            // failure. One field because the admin reads one thing either
            // way: "did this work, and what did it say".
            StructField::new("message", TypeSchema::text()),
            StructField::new("model", TypeSchema::text()),
        ]))
        .auth(AuthRequirement::admin()),
    );

    // --- agents (configuration) ---------------------------------------------
    // An agent is its own record (§11.2, decision 4): a provider, a model, a
    // system prompt and a list of enabled traits. These endpoints are that
    // row's lifecycle, in the shape the triggers' are — because the two records
    // have the same problem. A stored agent that does not validate is **not in
    // the live set**, will not answer, and is still listed here with its reason,
    // because editing it is the repair.
    //
    // The chat *turn* is deliberately not here: it is a WebSocket (§11.4), and
    // this model describes request/response pairs.

    set.register(
        Endpoint::new("listAgents", Method::Get, api().lit("agents"))
            .output(TypeSchema::array(agent_schema()))
            .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new("createAgent", Method::Post, api().lit("agents"))
            .input(agent_input_schema())
            .output(agent_schema())
            .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new(
            "updateAgent",
            Method::Put,
            api().lit("agents").param("id", ValueType::Uuid),
        )
        .input(agent_input_schema())
        .output(agent_schema())
        .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new(
            "deleteAgent",
            Method::Delete,
            api().lit("agents").param("id", ValueType::Uuid),
        )
        .output(TypeSchema::struct_of([StructField::new(
            "deleted",
            TypeSchema::bool(),
        )]))
        .auth(AuthRequirement::admin()),
    );

    // The registered traits with the configuration each declares, so the agent
    // form renders a form for a trait it knows nothing about — the same move
    // `listActions` and `listLlmProviderBackends` make. `tool_names` is the one
    // addition: a trait's tools are named from its configuration (§11.2), and an
    // admin about to save two traits whose names would collide is better told
    // what they are called than left to discover it from the refusal.
    set.register(
        Endpoint::new("listAgentTraits", Method::Get, api().lit("agent-traits"))
            .output(TypeSchema::array(agent_trait_info_schema()))
            .auth(AuthRequirement::admin()),
    );

    // --- runs ---------------------------------------------------------------
    // A chat session **is** a run (§11.4), so the history the chat panel shows
    // and the record a triggered run leaves behind are one list. Runs are keyed
    // by the agent's *name*, which is what `_sc_runs.subject` holds — a run
    // outlives the agent it was of, deliberately.

    set.register(
        Endpoint::new(
            "listRuns",
            Method::Get,
            api().lit("agent-runs").param("agent", ValueType::Text),
        )
        .output(TypeSchema::array(run_summary_schema()))
        .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new(
            "getRun",
            Method::Get,
            api().lit("runs").param("id", ValueType::Uuid),
        )
        .output(run_schema())
        .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new(
            "deleteRun",
            Method::Delete,
            api().lit("runs").param("id", ValueType::Uuid),
        )
        .output(TypeSchema::struct_of([StructField::new(
            "deleted",
            TypeSchema::bool(),
        )]))
        .auth(AuthRequirement::admin()),
    );

    // --- files (file manager) ----------------------------------------------

    // Browse one directory of a store. A POST (not GET) so the directory — which
    // may contain `/` and would not fit a single path segment — rides in the body.
    set.register(
        Endpoint::new(
            "browseFiles",
            Method::Post,
            api()
                .lit("file-stores")
                .param("store", ValueType::Text)
                .lit("browse"),
        )
        .input(TypeSchema::struct_of([StructField::new(
            "dir",
            TypeSchema::text(),
        )]))
        .output(TypeSchema::array(file_entry_schema()))
        .auth(AuthRequirement::admin()),
    );

    // Search a store's text files, server-side. This is the endpoint the IDE's
    // find-in-files runs on (§12.1): walking the tree through the filesystem
    // provider is one request per directory, and the same walk done where the
    // bytes are is one request in total. `search_files` (§11.3) runs the same
    // search, so what a person finds in the editor is what a model finds.
    set.register(
        Endpoint::new(
            "searchFiles",
            Method::Post,
            api()
                .lit("file-stores")
                .param("store", ValueType::Text)
                .lit("search"),
        )
        .input(TypeSchema::struct_of([
            StructField::new("pattern", TypeSchema::text()),
            StructField::new("regex", TypeSchema::optional(TypeSchema::bool())),
            StructField::new("case_sensitive", TypeSchema::optional(TypeSchema::bool())),
            StructField::new("whole_word", TypeSchema::optional(TypeSchema::bool())),
            StructField::new("glob", TypeSchema::optional(TypeSchema::text())),
            StructField::new("dir", TypeSchema::optional(TypeSchema::text())),
            StructField::new("max_results", TypeSchema::optional(TypeSchema::int())),
        ]))
        .output(file_search_schema())
        .auth(AuthRequirement::admin()),
    );

    // Read one file's bytes (download) — base64 always, plus a UTF-8 `text`
    // shortcut when the contents decode cleanly (for the text editor).
    set.register(
        Endpoint::new(
            "readFile",
            Method::Post,
            api()
                .lit("file-stores")
                .param("store", ValueType::Text)
                .lit("read"),
        )
        .input(TypeSchema::struct_of([StructField::new(
            "path",
            TypeSchema::text(),
        )]))
        .output(file_content_schema())
        .auth(AuthRequirement::admin()),
    );

    // Write one file (upload / save an edited text file). The body carries the
    // contents as either base64 (`base64`) or UTF-8 (`text`); exactly one.
    set.register(
        Endpoint::new(
            "writeFile",
            Method::Post,
            api()
                .lit("file-stores")
                .param("store", ValueType::Text)
                .lit("write"),
        )
        .input(TypeSchema::struct_of([
            StructField::new("path", TypeSchema::text()),
            StructField::new("base64", TypeSchema::optional(TypeSchema::text())),
            StructField::new("text", TypeSchema::optional(TypeSchema::text())),
        ]))
        .output(file_entry_schema())
        .auth(AuthRequirement::admin()),
    );

    // Create a directory (and any missing parents). Idempotent — asking for a
    // directory that exists is a success, since the caller wanted one there.
    set.register(
        Endpoint::new(
            "makeDirectory",
            Method::Post,
            api()
                .lit("file-stores")
                .param("store", ValueType::Text)
                .lit("mkdir"),
        )
        .input(TypeSchema::struct_of([StructField::new(
            "path",
            TypeSchema::text(),
        )]))
        .output(file_entry_schema())
        .auth(AuthRequirement::admin()),
    );

    // Delete a file, or a directory and everything in it. Reports whether
    // anything was there, so the caller need not race an existence check.
    set.register(
        Endpoint::new(
            "deleteFile",
            Method::Post,
            api()
                .lit("file-stores")
                .param("store", ValueType::Text)
                .lit("delete"),
        )
        .input(TypeSchema::struct_of([StructField::new(
            "path",
            TypeSchema::text(),
        )]))
        .output(TypeSchema::struct_of([StructField::new(
            "deleted",
            TypeSchema::bool(),
        )]))
        .auth(AuthRequirement::admin()),
    );

    // Move or rename within the store. Never overwrites an existing destination.
    set.register(
        Endpoint::new(
            "renameFile",
            Method::Post,
            api()
                .lit("file-stores")
                .param("store", ValueType::Text)
                .lit("rename"),
        )
        .input(TypeSchema::struct_of([
            StructField::new("from", TypeSchema::text()),
            StructField::new("to", TypeSchema::text()),
        ]))
        .output(file_entry_schema())
        .auth(AuthRequirement::admin()),
    );

    // Per-file metadata (design §9): the access rule and the free-form
    // attributes kept beside the bytes rather than in a database row. `min_role`
    // is what the path-cumulative rule is built from, so this is how an admin
    // restricts a folder.
    set.register(
        Endpoint::new(
            "getFileMeta",
            Method::Post,
            api()
                .lit("file-stores")
                .param("store", ValueType::Text)
                .lit("meta"),
        )
        .input(TypeSchema::struct_of([StructField::new(
            "path",
            TypeSchema::text(),
        )]))
        .output(file_meta_schema())
        .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new(
            "setFileMeta",
            Method::Post,
            api()
                .lit("file-stores")
                .param("store", ValueType::Text)
                .lit("set-meta"),
        )
        .input(TypeSchema::struct_of([
            StructField::new("path", TypeSchema::text()),
            StructField::new("min_role", TypeSchema::optional(TypeSchema::int())),
            StructField::new("attributes", TypeSchema::json()),
        ]))
        .output(file_meta_schema())
        .auth(AuthRequirement::admin()),
    );

    // --- applications -------------------------------------------------------
    // An application is created in the admin UI and exists only as its stored
    // row (§13.2); these endpoints are the row ⇄ mounted-app path the SPA drives.
    // Create/update/delete manage the definition; `build` builds and mounts it
    // live (§13.2 "no restart"); the build's outcome — including a bundler's
    // diagnostics on failure — comes back as an Application error (§16).

    set.register(
        Endpoint::new("listApplications", Method::Get, api().lit("applications"))
            .output(TypeSchema::array(application_schema()))
            .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new("createApplication", Method::Post, api().lit("applications"))
            .input(application_input_schema())
            .output(created_application_schema())
            .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new(
            "updateApplication",
            Method::Put,
            api().lit("applications").param("id", ValueType::Uuid),
        )
        .input(application_input_schema())
        .output(application_schema())
        .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new(
            "deleteApplication",
            Method::Delete,
            api().lit("applications").param("id", ValueType::Uuid),
        )
        // `agent` names the builder agent that was deleted with the application
        // (§13.3), when there was one to delete — an application's builder can do
        // nothing once the application is gone, and the admin should be told it
        // went rather than discover it missing.
        .output(TypeSchema::struct_of([
            StructField::new("deleted", TypeSchema::bool()),
            StructField::new("agent", TypeSchema::optional(TypeSchema::text())),
        ]))
        .auth(AuthRequirement::admin()),
    );

    // Build (and mount) an application. On success the app is serving on its
    // subdomain; the response reports the build log. A failed build leaves the
    // previously mounted version up and comes back as an Application error whose
    // message carries the bundler's own diagnostics (§16).
    set.register(
        Endpoint::new(
            "buildApplication",
            Method::Post,
            api()
                .lit("applications")
                .param("id", ValueType::Uuid)
                .lit("build"),
        )
        .output(build_result_schema())
        .auth(AuthRequirement::admin()),
    );

    // Rewrite an application's **generated** code from its current definition —
    // `src/saltcorn/**`: the typed client, the hooks, the schema and the README
    // (§13.3). No bundler runs; this is the "if the API definition changes, the
    // client code must be updated automatically" path (decision 10) with a
    // button on it, for the times an admin wants it *now* rather than at the
    // next change.
    //
    // A project directory that is **empty** is scaffolded instead — an app whose
    // store was unreachable when it was created, or whose tree somebody deleted,
    // has nothing to regenerate, and a `src/saltcorn/` with no project around it
    // could not build. Which of the two happened is in the response, because
    // writing a whole project is not the same news as rewriting four files.
    set.register(
        Endpoint::new(
            "updateApplicationClient",
            Method::Post,
            api()
                .lit("applications")
                .param("id", ValueType::Uuid)
                .lit("client"),
        )
        .output(TypeSchema::struct_of([
            StructField::new("scaffolded", TypeSchema::bool()),
            StructField::new("files", TypeSchema::array(TypeSchema::text())),
            StructField::new("log", TypeSchema::text()),
        ]))
        .auth(AuthRequirement::admin()),
    );

    // Run one GraphQL operation against a **mounted** application's own GraphQL
    // provider — what the admin UI's explorer (§13.4) is built on.
    //
    // It is an admin endpoint rather than a browser request to the app's mount
    // because the admin SPA is served under `connect-src 'self'`: a `fetch` from
    // the base domain to `blog.example.com/graphql` is a cross-origin request
    // the page's own policy forbids, and relaxing that policy to let one screen
    // talk to every subdomain is a poor trade for a debugging tool.
    //
    // **The operation runs as the signed-in admin**, through the very same
    // `ApiProvider::handle` a request to the app's mount reaches — same schema,
    // same limits, same authorization at resolve time. The explorer therefore
    // has exactly the authority of the person using it, which is the property
    // that makes it a debugging tool rather than a back door; the screen says so
    // in as many words.
    //
    // The output is `json`: a GraphQL response is `{data, errors, extensions}`
    // whose `data` is the shape the *caller's document* asked for, which no
    // `TypeSchema` can describe ahead of time. That is the same reason the
    // provider's own `graphqlQuery` endpoint declares it.
    set.register(
        Endpoint::new(
            "runApplicationGraphql",
            Method::Post,
            api()
                .lit("applications")
                .param("id", ValueType::Uuid)
                .lit("graphql"),
        )
        .input(TypeSchema::struct_of([
            StructField::new("query", TypeSchema::text()),
            StructField::new("variables", TypeSchema::optional(TypeSchema::json())),
            StructField::new("operationName", TypeSchema::optional(TypeSchema::text())),
        ]))
        .output(TypeSchema::json())
        .auth(AuthRequirement::admin()),
    );

    // --- frameworks ---------------------------------------------------------
    // The registered frameworks with their settings spec, so the create/edit
    // form can render controls for a framework it knows nothing about (§13.3).
    set.register(
        Endpoint::new("listFrameworks", Method::Get, api().lit("frameworks"))
            .output(TypeSchema::array(framework_info_schema()))
            .auth(AuthRequirement::admin()),
    );

    // --- API providers ------------------------------------------------------
    // The registered API providers, so the application form offers them as a
    // **list** rather than a free-text box (§13.4). A provider name is the one
    // field of an application whose typo is not caught until the app is mounted,
    // where it becomes "unknown API provider" on a save that appeared to work.
    // The same move `listFrameworks` makes, for the same reason.
    set.register(
        Endpoint::new("listApiProviders", Method::Get, api().lit("api-providers"))
            .output(TypeSchema::array(api_provider_info_schema()))
            .auth(AuthRequirement::admin()),
    );

    // Prepare one custom SQL query and report the columns the **database** says
    // it returns (§13.4, decision 5), without storing anything.
    //
    // Saving already does this — a query that will not prepare cannot be saved —
    // so why a second route? Because the editor needs the answer *before* the
    // save: the columns are what the admin's generated client method will
    // return, and finding out by saving the whole application means finding out
    // about a typo in one `SELECT` by having every other edit on the screen
    // refused with it. It is the same call on the same catalog; only the moment
    // differs.
    set.register(
        Endpoint::new(
            "describeCustomQuery",
            Method::Post,
            api().lit("custom-queries").lit("describe"),
        )
        .input(custom_query_input_schema())
        .output(TypeSchema::struct_of([StructField::new(
            "columns",
            TypeSchema::array(query_column_schema()),
        )]))
        .auth(AuthRequirement::admin()),
    );

    // --- users --------------------------------------------------------------

    // The users screen manages accounts the same way every other screen manages
    // its objects — a list, a form, and the operations that are not edits.
    //
    // Four of these are *not* row edits and that is why they are endpoints of
    // their own rather than fields of `updateUser`: disabling an account,
    // dropping its sessions, becoming it, and resetting its password each do
    // something a column write cannot (they touch sessions, or they hand back a
    // secret that exists only in that response). Spelling them as booleans in an
    // update body would hide that.

    set.register(
        Endpoint::new("listUsers", Method::Get, api().lit("users"))
            .output(TypeSchema::array(user_row_schema()))
            .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new("createUser", Method::Post, api().lit("users"))
            .input(user_input_schema())
            // The row, plus the password if one was generated because the admin
            // left it blank — the only moment it is readable (§7.1).
            .output(TypeSchema::struct_of([
                StructField::new("user", user_row_schema()),
                StructField::new(
                    "generated_password",
                    TypeSchema::optional(TypeSchema::text()),
                ),
            ]))
            .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new(
            "updateUser",
            Method::Put,
            api().lit("users").param("id", ValueType::Uuid),
        )
        .input(user_input_schema())
        .output(user_row_schema())
        .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new(
            "deleteUser",
            Method::Delete,
            api().lit("users").param("id", ValueType::Uuid),
        )
        .output(TypeSchema::struct_of([StructField::new(
            "deleted",
            TypeSchema::bool(),
        )]))
        .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new(
            "setUserDisabled",
            Method::Post,
            api()
                .lit("users")
                .param("id", ValueType::Uuid)
                .lit("disabled"),
        )
        .input(TypeSchema::struct_of([StructField::new(
            "disabled",
            TypeSchema::bool(),
        )]))
        .output(user_row_schema())
        .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new(
            "forceLogoutUser",
            Method::Post,
            api()
                .lit("users")
                .param("id", ValueType::Uuid)
                .lit("force-logout"),
        )
        // `ok`, not a count of sessions ended: the sessions are dropped by the
        // transport (which owns the store) after the handler has returned, so a
        // number here would be one the handler had to guess.
        .output(TypeSchema::struct_of([StructField::new(
            "ok",
            TypeSchema::bool(),
        )]))
        .auth(AuthRequirement::admin()),
    );

    // Swap the caller's own session for one belonging to this user, with no
    // password: an admin who can reset that password can already sign in as them
    // (§7.1), so this adds convenience rather than authority — and it costs the
    // admin their admin session, which is the honest price of the swap.
    set.register(
        Endpoint::new(
            "becomeUser",
            Method::Post,
            api()
                .lit("users")
                .param("id", ValueType::Uuid)
                .lit("become"),
        )
        .output(user_summary_schema())
        .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new(
            "setRandomPassword",
            Method::Post,
            api()
                .lit("users")
                .param("id", ValueType::Uuid)
                .lit("random-password"),
        )
        .output(TypeSchema::struct_of([
            StructField::new("email", TypeSchema::text()),
            StructField::new("password", TypeSchema::text()),
        ]))
        .auth(AuthRequirement::admin()),
    );

    // --- triggers -----------------------------------------------------------
    // A trigger is one event bound to one configured action (§10.2), stored in
    // `_sc_triggers`. These endpoints are the row ⇄ live-set path the SPA
    // drives: every save is validated and the live set is reloaded, so what the
    // list shows is what will fire.

    set.register(
        Endpoint::new("listTriggers", Method::Get, api().lit("triggers"))
            .output(TypeSchema::array(trigger_schema()))
            .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new("createTrigger", Method::Post, api().lit("triggers"))
            .input(trigger_input_schema())
            .output(trigger_schema())
            .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new(
            "updateTrigger",
            Method::Put,
            api().lit("triggers").param("id", ValueType::Uuid),
        )
        .input(trigger_input_schema())
        .output(trigger_schema())
        .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new(
            "deleteTrigger",
            Method::Delete,
            api().lit("triggers").param("id", ValueType::Uuid),
        )
        .output(TypeSchema::struct_of([StructField::new(
            "deleted",
            TypeSchema::bool(),
        )]))
        .auth(AuthRequirement::admin()),
    );

    // Run one trigger now — the admin's "test this now". The posted body is the
    // event's payload and the action's result comes back; an action that fails
    // comes back as an **error**, not a 200 carrying a failure nobody reads.
    set.register(
        Endpoint::new(
            "runTrigger",
            Method::Post,
            api()
                .lit("triggers")
                .param("id", ValueType::Uuid)
                .lit("run"),
        )
        .input(TypeSchema::json())
        .output(TypeSchema::struct_of([StructField::new(
            "result",
            TypeSchema::json(),
        )]))
        .auth(AuthRequirement::admin()),
    );

    // --- actions ------------------------------------------------------------
    // The registered actions with the settings each declares, so the trigger
    // form renders a configuration form for an action it knows nothing about
    // (§13.3) — the same move the framework and file-store pickers make.
    set.register(
        Endpoint::new("listActions", Method::Get, api().lit("actions"))
            .output(TypeSchema::array(action_info_schema()))
            .auth(AuthRequirement::admin()),
    );

    // --- settings -----------------------------------------------------------
    // The `_sc_config` values an admin edits (§9, §13.5). Two endpoints, and
    // both carry the **declarations** alongside the values, for the same reason
    // the file-store and LLM-provider screens are handed a `config_spec`: the
    // settings screen renders whatever the server declares and knows nothing
    // about any particular setting. Adding one is a Rust declaration and a
    // redeployed server, with no matching change in the SPA.
    //
    // The save returns the settings as they now stand rather than an
    // acknowledgement, so the screen shows what was stored — including the
    // defaults a cleared box fell back to, and the sentinel standing in for a
    // secret it must not be handed back.
    set.register(
        Endpoint::new("getSettings", Method::Get, api().lit("settings"))
            .output(settings_schema())
            .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new("updateSettings", Method::Post, api().lit("settings"))
            .input(TypeSchema::struct_of([StructField::new(
                "values",
                TypeSchema::json(),
            )]))
            .output(settings_schema())
            .auth(AuthRequirement::admin()),
    );

    // --- backup & restore ---------------------------------------------------
    // Two of the four backup operations are here; the other two are routes outside
    // this set, because one *is* a file and the other *takes* one, and a
    // `TypeSchema` has no bytes shape (the same reason the binary file upload is
    // outside it). The split is along that line and no other: what an admin
    // includes, and what a restore did, are ordinary typed JSON and belong in the
    // generated client.

    // What this server has to offer a backup, and the selection the admin last
    // made — one response, because the dialog needs both to open and asking for
    // them separately would let them disagree.
    set.register(
        Endpoint::new("getBackupOptions", Method::Get, api().lit("backup"))
            .output(TypeSchema::struct_of([
                StructField::new("available", backup_contents_schema()),
                StructField::new("include", backup_selection_schema()),
            ]))
            .auth(AuthRequirement::admin()),
    );

    // Restore from an archive already uploaded (`/backup/upload` handed back its
    // `id`), taking the parts `include` names.
    //
    // The two lists rather than a single "ok": a restore is dozens of independent
    // acts and some of them are routinely skipped — an account that is already
    // here, a trigger on a table the admin left out. Reporting that as success
    // would hide it and as failure would be wrong.
    set.register(
        Endpoint::new(
            "restoreBackup",
            Method::Post,
            api().lit("backup").lit("restore"),
        )
        .input(TypeSchema::struct_of([
            StructField::new("id", TypeSchema::text()),
            StructField::new("include", backup_selection_schema()),
        ]))
        .output(TypeSchema::struct_of([
            StructField::new("restored", TypeSchema::array(TypeSchema::text())),
            StructField::new("warnings", TypeSchema::array(TypeSchema::text())),
        ]))
        .auth(AuthRequirement::admin()),
    );

    set
}

/// One thing a backup can include or leave out: what it is called, what to show,
/// and how much of it there is (rows or files; `null` where counting it would cost
/// more than the number is worth).
fn backup_item_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("name", TypeSchema::text()),
        StructField::new("label", TypeSchema::text()),
        StructField::new("count", TypeSchema::optional(TypeSchema::int())),
    ])
}

/// Everything that could go into a backup — of this server, or of a backup file
/// that has been uploaded. **One schema for both**, which is what lets one dialog
/// drive the backup and the restore: the difference between them is where the
/// value came from, not what it is.
fn backup_contents_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("tables", TypeSchema::array(backup_item_schema())),
        StructField::new("applications", TypeSchema::array(backup_item_schema())),
        StructField::new("file_stores", TypeSchema::array(backup_item_schema())),
        // Counts rather than flags, because "back up the users" is a different
        // decision when there are two of them and when there are twelve thousand.
        StructField::new("users", TypeSchema::int()),
        StructField::new("agents", TypeSchema::int()),
        StructField::new("triggers", TypeSchema::int()),
        StructField::new("ssl", TypeSchema::bool()),
    ])
}

/// What one backup or restore includes.
///
/// `table_data` is separate from `tables` because metadata and rows are separate
/// decisions — a schema-only backup is a real thing to want — and it is a **subset**
/// of it: rows restored into a table nobody described would be unreadable, so the
/// server narrows this to `tables` on the way in rather than trusting it.
fn backup_selection_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("tables", TypeSchema::array(TypeSchema::text())),
        StructField::new("table_data", TypeSchema::array(TypeSchema::text())),
        StructField::new("applications", TypeSchema::array(TypeSchema::text())),
        StructField::new("file_stores", TypeSchema::array(TypeSchema::text())),
        StructField::new("users", TypeSchema::bool()),
        StructField::new("agents", TypeSchema::bool()),
        StructField::new("triggers", TypeSchema::bool()),
        StructField::new("ssl", TypeSchema::bool()),
    ])
}

/// A `PathSpec` rooted at the admin API prefix.
fn api() -> PathSpec {
    PathSpec::root().lit(ADMIN_API_PREFIX)
}

/// A table in the catalog: its name, plus the `_sc_tables` overlay merged onto
/// it (§9).
///
/// The access roles are in the *list* response, not only in a detail one,
/// because "which of these tables can the public read?" is a question an admin
/// asks about the whole set and should not have to open eight screens to answer.
///
/// `configured` distinguishes a table an admin has set to admin-only from one
/// nobody has touched — both read `1`/`1`, and only the first has a row to
/// delete. Without it the UI could not offer "forget these settings" honestly.
fn table_schema() -> TypeSchema {
    let mut fields = vec![StructField::new("name", TypeSchema::text())];
    fields.extend(table_settings_fields());
    fields.push(StructField::new("configured", TypeSchema::bool()));
    // Why the stored ownership formula is not in effect, when it is not — a
    // stored formula can stop validating when the schema changes under it
    // (fail closed, §7.3), and the admin fixes it where they typed it.
    fields.push(StructField::new(
        "ownership_error",
        TypeSchema::optional(TypeSchema::text()),
    ));
    // Whether the backend can enforce RLS at all (`DbCapabilities`). The SPA
    // renders the RLS toggle only when this is true — a toggle that can only
    // ever be refused is not a setting, it is a trap.
    fields.push(StructField::new("rls_available", TypeSchema::bool()));
    TypeSchema::Struct(fields)
}

/// The overlay fields of a table — everything an admin may set, and nothing the
/// database is the authority on (§9's precedence rule).
fn table_settings_fields() -> Vec<StructField> {
    vec![
        StructField::new("label", TypeSchema::text()),
        StructField::new("description", TypeSchema::text()),
        StructField::new("min_role_read", TypeSchema::int()),
        StructField::new("min_role_write", TypeSchema::int()),
        // The ownership formula source (§7.3); empty means "none". Validated
        // on save: an unknown identifier or a broken Ⱶ-path is a 400 naming
        // it, and nothing is written.
        StructField::new("ownership_formula", TypeSchema::text()),
        // Stored and surfaced in this phase; §6 makes it enforce. Refused on
        // save when the backend cannot do RLS or the formula cannot become a
        // policy.
        StructField::new("rls_enabled", TypeSchema::bool()),
    ]
}

/// The body accepted when configuring a table.
///
/// Not optional fields: a settings save states the whole configuration, so an
/// omitted role would have to mean either "leave it" or "reset it" and the wire
/// cannot say which. The admin UI edits a loaded table and sends it back, so it
/// always has every value to hand.
fn table_settings_schema() -> TypeSchema {
    TypeSchema::Struct(table_settings_fields())
}

/// Stored settings for a table that is not in the database (§1.1).
fn orphan_table_settings_schema() -> TypeSchema {
    let mut fields = vec![StructField::new("name", TypeSchema::text())];
    fields.extend(table_settings_fields());
    TypeSchema::Struct(fields)
}

/// One role (technical design §7.1, §9).
///
/// **Why roles are rows and not a constant list.** Roles are the fixed scale
/// `1..=100`, and for a while an integer was all a role was: `1` meant admin
/// because a constant said so, `100` meant public, and the ninety-eight numbers
/// between meant whatever an installation's users made them mean. That stops
/// working the moment a role has to *carry* something — a name to show in a
/// pick-list, settings that apply to everyone holding it — because a row can
/// carry those and an integer cannot. So `_sc_roles` holds them, `users.role`
/// references it, and this endpoint reports what is there rather than what a
/// constant asserts.
///
/// `builtin` marks the two the system itself depends on (admin and public):
/// they are not deletable, and the UI has to know that before offering the
/// button rather than after refusing the request.
fn role_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("role", TypeSchema::int()),
        StructField::new("name", TypeSchema::text()),
        StructField::new("description", TypeSchema::text()),
        StructField::new("builtin", TypeSchema::bool()),
    ])
}

/// The body of a user create or edit — the same shape for both, since a user
/// form is a user form.
///
/// `password` is optional and its blank means two different things by design, one
/// per operation: **on create** it asks for a generated password (returned once,
/// in the response), and **on update** it leaves the stored hash alone. The
/// alternative — making the admin type a password to change a role — is what
/// makes people reuse one.
///
/// `extra` carries the columns the admin has added to the users table (§7.1),
/// keyed by column name; the system's own columns are refused there, since each
/// has its own way in.
fn user_input_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("email", TypeSchema::text()),
        StructField::new("password", TypeSchema::optional(TypeSchema::text())),
        StructField::new("role", TypeSchema::int()),
        StructField::new("extra", TypeSchema::optional(TypeSchema::json())),
    ])
}

/// A field (column) of a table, with the `_sc_fields` overlay merged onto it
/// (§3.2): the introspected `sql_type`/`nullable`/`unique`, plus the overlay's
/// `type` (a rich type's name, or the basic type's), `kind` (with its
/// parameters), `label`, `description` and `attributes`.
fn field_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("name", TypeSchema::text()),
        StructField::new("label", TypeSchema::text()),
        StructField::new("description", TypeSchema::text()),
        StructField::new("sql_type", TypeSchema::text()),
        StructField::new("type", TypeSchema::text()),
        StructField::new("nullable", TypeSchema::bool()),
        StructField::new("required", TypeSchema::bool()),
        StructField::new("unique", TypeSchema::bool()),
        StructField::new("primary_key", TypeSchema::bool()),
        // Whether the column fills itself in when a write omits it. A fact about
        // the column read back by introspection, not a wish recorded when it was
        // created, so it stays true however the field came to be a key.
        StructField::new("generated", TypeSchema::bool()),
        // `kind` and `attributes` are opaque JSON: their shape depends on the
        // field's kind and type, which the API cannot know statically any more
        // than it can a framework's settings.
        StructField::new("kind", TypeSchema::json()),
        StructField::new("attributes", TypeSchema::json()),
    ])
}

/// The body accepted when **creating** a field. `type` is a basic-type or
/// registered-rich-type name — `sql_type` is derived from it, not asked for, so
/// the two can never disagree (§3.3). Everything past `name` is optional:
/// `type` may be omitted for a `Key`, whose storage type is its target's and so
/// is not the caller's to choose (`schema_edit::FieldSpec`).
fn create_field_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("name", TypeSchema::text()),
        StructField::new("type", TypeSchema::optional(TypeSchema::text())),
        StructField::new("kind", TypeSchema::optional(TypeSchema::json())),
        StructField::new("attributes", TypeSchema::optional(TypeSchema::json())),
        StructField::new("label", TypeSchema::optional(TypeSchema::text())),
        StructField::new("description", TypeSchema::optional(TypeSchema::text())),
        StructField::new("required", TypeSchema::optional(TypeSchema::bool())),
        StructField::new("unique", TypeSchema::optional(TypeSchema::bool())),
        // The key is a field like any other (GOALS): a table is created with no
        // primary key at all, and gets one when a field says it is one. More
        // than one field may, and then the key is composite in field order.
        StructField::new("primary_key", TypeSchema::optional(TypeSchema::bool())),
    ])
}

/// The body accepted when **editing** a field — the overlay-only subset, plus
/// the one column property that must be reachable after the fact. No `name`,
/// `required`, `unique` or storage type: those are the database's, and changing
/// them is a schema change out of scope for this milestone.
///
/// `primary_key` is the exception, and a considered one: since no table is
/// created with a key it did not declare, a table that has none — imported from
/// a CSV with no key column, or built a field at a time — could otherwise only
/// get one by being dropped and recreated with its rows thrown away.
fn field_settings_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("type", TypeSchema::optional(TypeSchema::text())),
        StructField::new("kind", TypeSchema::optional(TypeSchema::json())),
        StructField::new("attributes", TypeSchema::optional(TypeSchema::json())),
        StructField::new("label", TypeSchema::optional(TypeSchema::text())),
        StructField::new("description", TypeSchema::optional(TypeSchema::text())),
        StructField::new("primary_key", TypeSchema::optional(TypeSchema::bool())),
    ])
}

/// One entry of `listFieldTypes`: a basic type, a rich type, or a field kind,
/// each with the `config_spec` its attribute form is rendered from (empty for a
/// basic type). `category` lets the editor group them; `name` is what
/// `createField`/`updateField` take as `type` (basic/rich) or `kind.type` (kind).
fn field_type_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("name", TypeSchema::text()),
        StructField::new("label", TypeSchema::text()),
        StructField::new("category", TypeSchema::text()),
        StructField::new("config_spec", TypeSchema::array(form_field_schema())),
    ])
}

/// The fields common to a file store on the wire — everything but its id.
///
/// `config` is opaque JSON: it is whatever the chosen backend's `config_spec`
/// declares, and the API cannot know that statically any more than it can know a
/// framework's settings (see [`framework_ref_schema`]).
fn file_store_fields() -> Vec<StructField> {
    vec![
        StructField::new("name", TypeSchema::text()),
        StructField::new("description", TypeSchema::text()),
        StructField::new("backend", TypeSchema::text()),
        StructField::new("config", TypeSchema::json()),
        // Null means unrestricted, which is distinct from any particular role.
        StructField::new("min_role", TypeSchema::optional(TypeSchema::int())),
    ]
}

/// A file store as reported to the admin UI: its definition, plus the live state
/// that only the running server knows.
///
/// The trailing fields are why a store is more than its row. `connected` and
/// `error` exist because a definition can be perfectly valid and still not
/// usable — a directory unmounted since it was saved — and the UI has to show
/// that state with its reason rather than silently omitting the store or
/// pretending it works. `is_git_repo` is a property of the connected instance,
/// so it is null when there is no instance to ask.
///
/// **`id` is nullable, and that is the interesting case.** A store connected by
/// the `--file-store` flag is real and usable but has no row (§1.3: the flag is
/// deliberately ephemeral), so it has no id. Listing only stored definitions
/// would hide it — a developer running with the flag would see an empty store
/// list and no store to browse — so the listing is the *union* of defined and
/// connected stores. A null id is precisely what tells the UI that a store
/// cannot be edited or deleted: there is no row to edit, and it will be gone on
/// the next boot unless the flag is passed again.
fn file_store_schema() -> TypeSchema {
    let mut fields = vec![StructField::new(
        "id",
        TypeSchema::optional(TypeSchema::uuid()),
    )];
    fields.extend(file_store_fields());
    fields.extend([
        StructField::new("connected", TypeSchema::bool()),
        StructField::new("error", TypeSchema::optional(TypeSchema::text())),
        StructField::new("is_git_repo", TypeSchema::optional(TypeSchema::bool())),
    ]);
    TypeSchema::Struct(fields)
}

/// The body accepted when creating or updating a file store: the definition's
/// fields minus the id (server-assigned on create, taken from the path on
/// update) and minus the live state, which is observed rather than set.
fn file_store_input_schema() -> TypeSchema {
    TypeSchema::Struct(file_store_fields())
}

/// One file's metadata (design §9): the access rule and the free-form attributes
/// kept beside the bytes rather than in a database row.
///
/// `effective_min_role` is the *computed* answer — the most restrictive rule on
/// the whole path, including the store's own floor and every parent directory —
/// while `min_role` is only what is set on this entry. The UI needs both: the
/// second is what an admin edits, the first is what actually applies, and
/// showing only the second would let an admin believe a file is public when a
/// parent directory has locked it.
fn file_meta_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("path", TypeSchema::text()),
        StructField::new("min_role", TypeSchema::optional(TypeSchema::int())),
        StructField::new(
            "effective_min_role",
            TypeSchema::optional(TypeSchema::int()),
        ),
        StructField::new("attributes", TypeSchema::json()),
    ])
}

/// The fields of an LLM provider's definition that an admin sets.
///
/// Shorter than a file store's by one: there is no `min_role`, because a
/// provider is reached only through an agent and it is the agent that carries
/// who may chat with it (§11.2). A floor here as well would be a second
/// authority over the same question.
fn llm_provider_fields() -> Vec<StructField> {
    vec![
        StructField::new("name", TypeSchema::text()),
        StructField::new("description", TypeSchema::text()),
        StructField::new("backend", TypeSchema::text()),
        // The backend's settings. **On the way out this is redacted**: a
        // `secret` setting reads back as the sentinel, never as the key.
        StructField::new("config", TypeSchema::json()),
    ]
}

/// An LLM provider as reported to the admin UI.
///
/// `id` is not optional here, unlike a file store's: there is no `--llm-provider`
/// flag and no such thing as a provider without a row, so every provider in a
/// listing is one that can be edited and deleted.
///
/// There is no `connected` either, and its absence is the design: connecting a
/// provider builds an HTTP client and sends nothing, so "connected" would be a
/// word for "the configuration parsed" — which the admin already knows, because
/// the save succeeded. Whether the provider *works* is a request, and that is
/// what `testLlmProvider` is.
fn llm_provider_schema() -> TypeSchema {
    let mut fields = vec![StructField::new("id", TypeSchema::uuid())];
    fields.extend(llm_provider_fields());
    TypeSchema::Struct(fields)
}

/// The body accepted when creating or updating a provider: the definition's
/// fields minus the id (server-assigned on create, taken from the path on
/// update).
fn llm_provider_input_schema() -> TypeSchema {
    TypeSchema::Struct(llm_provider_fields())
}

/// A registered LLM provider backend and the settings it declares.
///
/// No `operations`: nothing an LLM provider offers is an *act* on its own
/// configuration the way a git store's clone is. Testing the connection is one
/// endpoint rather than a declared operation because it is the same act for
/// every backend — there is no per-backend list for the UI to render.
fn llm_backend_info_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("name", TypeSchema::text()),
        StructField::new("config_spec", TypeSchema::array(form_field_schema())),
    ])
}

/// The fields of an agent's definition that an admin sets (§11.2).
fn agent_fields() -> Vec<StructField> {
    vec![
        StructField::new("name", TypeSchema::text()),
        StructField::new("description", TypeSchema::text()),
        // The `_sc_llm_providers` **name** this agent calls through, not its id:
        // that is what the stored row holds, so that is what round-trips.
        StructField::new("provider", TypeSchema::text()),
        // Null means "the provider's own default model", which is the common
        // case and a real answer rather than a missing one.
        StructField::new("model", TypeSchema::optional(TypeSchema::text())),
        StructField::new("system_prompt", TypeSchema::text()),
        // A **list** of `{trait, config}` pairs, not a map: a trait may be
        // enabled more than once (§11.2), and the order is the order its tools
        // are offered to the model in.
        StructField::new("traits", TypeSchema::array(enabled_trait_schema())),
        // Null is admin-only, the same safe reading a trigger's takes.
        StructField::new("min_role", TypeSchema::optional(TypeSchema::int())),
        // The sparse per-agent values (§9): `temperature`, `max_tokens`,
        // `max_steps`. A bag rather than three fields, because they are exactly
        // §9's sparse attributes and absent means "the provider's default",
        // which no number could stand in for.
        StructField::new("attributes", TypeSchema::json()),
    ]
}

/// One enabled trait: which trait, and how this instance is configured.
fn enabled_trait_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("trait", TypeSchema::text()),
        StructField::new("config", TypeSchema::json()),
    ])
}

/// One stored agent, with the reason it cannot run when there is one.
///
/// `error` is what makes a broken agent fixable rather than merely absent, as a
/// trigger's and a file store's are: an agent naming a provider that was deleted
/// or a trait configured against a dropped table is **not in the live set** and
/// will not answer, but it is still stored, still listed and still editable.
fn agent_schema() -> TypeSchema {
    let mut fields = vec![StructField::new("id", TypeSchema::uuid())];
    fields.extend(agent_fields());
    fields.push(StructField::new(
        "error",
        TypeSchema::optional(TypeSchema::text()),
    ));
    TypeSchema::Struct(fields)
}

/// The body accepted when creating or updating an agent: the definition's fields
/// minus the id (server-assigned on create, taken from the path on update) and
/// minus `error` (the server's answer, not the admin's input).
fn agent_input_schema() -> TypeSchema {
    TypeSchema::Struct(agent_fields())
}

/// A registered agent trait and the configuration it declares, so the agent form
/// renders a form for a trait it has never heard of.
fn agent_trait_info_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("name", TypeSchema::text()),
        StructField::new("description", TypeSchema::text()),
        StructField::new("config_spec", TypeSchema::array(form_field_schema())),
    ])
}

/// One run in a list: everything except the transcript.
///
/// The transcript is deliberately absent. A chat's history is a list of dozens
/// of runs and each `context` is a whole conversation, so a list carrying them
/// would send megabytes to render a sidebar; [`run_schema`] is what the panel
/// asks for when a run is opened.
fn run_summary_schema() -> TypeSchema {
    TypeSchema::Struct(vec![
        StructField::new("id", TypeSchema::uuid()),
        // `agent` or `workflow` (§10.3's engine shares this table).
        StructField::new("kind", TypeSchema::text()),
        // What the run is of: the agent's **name**.
        StructField::new("subject", TypeSchema::text()),
        StructField::new("description", TypeSchema::text()),
        // `running` | `done` | `failed` | `aborted`.
        StructField::new("state", TypeSchema::text()),
        StructField::new("error", TypeSchema::optional(TypeSchema::text())),
        StructField::new("user", TypeSchema::optional(TypeSchema::uuid())),
        StructField::new("created_at", TypeSchema::timestamp()),
        StructField::new("updated_at", TypeSchema::timestamp()),
    ])
}

/// One whole run: the summary plus the loop state it can be read back from.
///
/// `context` is passed through as JSON rather than described field by field: it
/// is `sc-agent`'s `AgentLoop`, whose shape belongs to the loop and changes with
/// it, and a second declaration of it here would be a second thing to keep in
/// step. What the chat panel reads out of it — the messages — is stable.
fn run_schema() -> TypeSchema {
    let TypeSchema::Struct(mut fields) = run_summary_schema() else {
        return run_summary_schema();
    };
    fields.push(StructField::new("context", TypeSchema::json()));
    fields.push(StructField::new("attributes", TypeSchema::json()));
    TypeSchema::Struct(fields)
}

/// A registered file-store backend and the settings it declares, so the admin UI
/// can render a form for a backend it knows nothing about. Each setting is a
/// [`form_field_schema`] — the same `FormField` vocabulary a row editor and a
/// framework's settings use.
fn backend_info_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("name", TypeSchema::text()),
        StructField::new("config_spec", TypeSchema::array(form_field_schema())),
        StructField::new("operations", TypeSchema::array(operation_schema())),
    ])
}

/// One [`Operation`](sc_types::Operation) a backend declares: an *act* it
/// offers, as opposed to a setting it takes.
///
/// The same "as data" move `form_field_schema` makes, for the other half of
/// what an extension can offer. `scope` says whether it runs against unsaved
/// configuration (`configure`) or a saved store (`instance`), which is what
/// tells the UI where to put the button; `input_spec` is whatever the operation
/// asks the admin for, in the ordinary settings vocabulary, so the same code
/// renders a commit-message box that renders a store's settings.
fn operation_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("name", TypeSchema::text()),
        StructField::new("label", TypeSchema::text()),
        StructField::new("description", TypeSchema::text()),
        StructField::new("scope", TypeSchema::text()),
        StructField::new("input_spec", TypeSchema::array(form_field_schema())),
        StructField::new("on_create", TypeSchema::bool()),
        StructField::new("automatic", TypeSchema::bool()),
    ])
}

/// One entry (file or sub-directory) inside a browsed directory.
fn file_entry_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("name", TypeSchema::text()),
        StructField::new("path", TypeSchema::text()),
        StructField::new("is_dir", TypeSchema::bool()),
        StructField::new("size", TypeSchema::optional(TypeSchema::int())),
    ])
}

/// What a search found: the matching lines, and whether a ceiling cut it short.
///
/// `truncated` is not decoration. A caller that renders results without it tells
/// the reader there is nothing else, which is false exactly when it matters — and
/// it is the difference between "no other uses of this symbol" and "the first
/// hundred uses of this symbol".
fn file_search_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new(
            "matches",
            TypeSchema::array(TypeSchema::struct_of([
                StructField::new("path", TypeSchema::text()),
                StructField::new("line", TypeSchema::int()),
                StructField::new("column", TypeSchema::int()),
                StructField::new("length", TypeSchema::int()),
                StructField::new("text", TypeSchema::text()),
            ])),
        ),
        StructField::new("files_searched", TypeSchema::int()),
        StructField::new("truncated", TypeSchema::bool()),
    ])
}

/// A file's contents: base64 always, plus decoded `text` when it is valid UTF-8.
fn file_content_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("path", TypeSchema::text()),
        StructField::new("size", TypeSchema::int()),
        StructField::new("base64", TypeSchema::text()),
        StructField::new("text", TypeSchema::optional(TypeSchema::text())),
    ])
}

/// A reference to a UI framework: its registered name and its settings bag. The
/// settings' shape is the framework's own `config_spec`, so `config` is opaque
/// JSON here (the form the SPA renders comes from [`framework_info_schema`]).
fn framework_ref_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("name", TypeSchema::text()),
        StructField::new("config", TypeSchema::json()),
    ])
}

/// One API provider enabled for an app, on a sub-path.
fn api_config_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("provider", TypeSchema::text()),
        StructField::new("mount", TypeSchema::text()),
        // The provider's own settings, opaque JSON here for the reason a
        // framework's `config` is: what the keys are is the *provider's*
        // declaration (`listApiProviders`' `config_spec`), and the form renders
        // that rather than a shape frozen into this schema.
        StructField::new("config", TypeSchema::json()),
    ])
}

/// A statically-served store subdirectory, on a sub-path.
fn static_dir_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("mount", TypeSchema::text()),
        StructField::new("store", TypeSchema::text()),
        StructField::new("path", TypeSchema::text()),
    ])
}

/// The fields common to an application on the wire — everything but its id. The
/// nested `csp` and `attributes` are opaque JSON (a directive→sources map and a
/// sparse bag respectively), matching how they are stored (§13.2).
fn application_fields() -> Vec<StructField> {
    vec![
        StructField::new("name", TypeSchema::text()),
        StructField::new("description", TypeSchema::text()),
        StructField::new("subdomain", TypeSchema::text()),
        StructField::new("framework", framework_ref_schema()),
        StructField::new(
            "extra_frameworks",
            TypeSchema::array(framework_ref_schema()),
        ),
        StructField::new("tables", TypeSchema::array(TypeSchema::text())),
        StructField::new("file_stores", TypeSchema::array(TypeSchema::text())),
        // The triggers this app exposes as endpoints (§10.2), by name — the same
        // opt-in subset shape the tables and stores have.
        StructField::new("triggers", TypeSchema::array(TypeSchema::text())),
        StructField::new("apis", TypeSchema::array(api_config_schema())),
        StructField::new("static_dirs", TypeSchema::array(static_dir_schema())),
        StructField::new("csp", TypeSchema::json()),
        StructField::new("attributes", TypeSchema::json()),
    ]
}

/// An application as returned by the API: its id, [`application_fields`], and
/// where its source lives.
///
/// `source` is **derived, not stored**: the server resolves the framework's
/// config to a store and a directory (`app_source_from_config`), so the admin UI
/// can link into the file manager at an app's source without knowing how any
/// framework spells that — `code` states it in five settings and `react` derives
/// it from one. `null` for a framework with no source tree.
fn application_schema() -> TypeSchema {
    let mut fields = vec![StructField::new("id", TypeSchema::uuid())];
    fields.extend(application_fields());
    fields.push(StructField::new(
        "source",
        TypeSchema::optional(app_source_schema()),
    ));
    TypeSchema::Struct(fields)
}

/// Where an application's source lives: a file store and a directory in it.
fn app_source_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("store", TypeSchema::text()),
        StructField::new("path", TypeSchema::text()),
    ])
}

/// A freshly created application, plus what scaffolding it did (§2.3) and which
/// agent was created to build it (§13.3).
///
/// Only `create` carries these: a `react` app's project is generated on its first
/// save, and the admin should see that it happened — or why it did not — without
/// a second request. `scaffolded` is a summary line; `scaffold_error` explains a
/// scaffold that was refused (an occupied directory, an unreachable store) on an
/// application that was nonetheless created, since the row is valid either way.
/// `agent` and `agent_error` report the builder agent the same way: its name, or
/// why the deployment could not create one (no LLM provider connected).
fn created_application_schema() -> TypeSchema {
    let TypeSchema::Struct(mut fields) = application_schema() else {
        unreachable!("application_schema is a struct")
    };
    fields.push(StructField::new(
        "scaffolded",
        TypeSchema::optional(TypeSchema::text()),
    ));
    fields.push(StructField::new(
        "scaffold_error",
        TypeSchema::optional(TypeSchema::text()),
    ));
    // The agent that builds this application, which its framework declares
    // (§13.3): its name when one was created, or why one was not — the same
    // alongside-not-instead-of reporting the scaffold gets, and for the same
    // reason. The application is created either way.
    fields.push(StructField::new(
        "agent",
        TypeSchema::optional(TypeSchema::text()),
    ));
    fields.push(StructField::new(
        "agent_error",
        TypeSchema::optional(TypeSchema::text()),
    ));
    TypeSchema::Struct(fields)
}

/// The body accepted when creating or updating an application: the same fields
/// minus the id (server-assigned on create, taken from the path on update).
fn application_input_schema() -> TypeSchema {
    TypeSchema::Struct(application_fields())
}

/// The outcome of a build: whether it built, whether its source is a git repo,
/// and the bundler's log. (A *failed* build is not this shape — it is an
/// Application error whose message carries the diagnostics, §16.)
fn build_result_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("built", TypeSchema::bool()),
        StructField::new("git_repo", TypeSchema::bool()),
        StructField::new("log", TypeSchema::text()),
    ])
}

/// One stored trigger: what fires it, what it runs, and whether it is usable.
///
/// `error` is the part that makes a broken trigger fixable rather than merely
/// absent, exactly as a file store's is: a trigger whose table was dropped or
/// whose action a removed plugin provided is **not in the live set** and will not
/// fire, but it is still stored, still listed and still editable — and the reason
/// is the only thing that says what to fix.
fn trigger_schema() -> TypeSchema {
    let mut fields = vec![StructField::new("id", TypeSchema::uuid())];
    fields.extend(trigger_fields());
    fields.push(StructField::new(
        "error",
        TypeSchema::optional(TypeSchema::text()),
    ));
    // Read-only, and absent from the input shape below: when a periodic trigger
    // last fired is the scheduler's record of what happened, not a field an
    // admin sets. The list shows it; nothing posts it back.
    fields.push(StructField::new(
        "last_run_at",
        TypeSchema::optional(TypeSchema::timestamp()),
    ));
    TypeSchema::Struct(fields)
}

/// The body accepted when creating or updating a trigger: the same fields minus
/// the id (server-assigned on create, taken from the path on update) and minus
/// `error` (which is the server's answer, not the admin's input).
fn trigger_input_schema() -> TypeSchema {
    TypeSchema::Struct(trigger_fields())
}

/// The editable half of a trigger, shared by the read and write shapes.
fn trigger_fields() -> Vec<StructField> {
    vec![
        StructField::new("name", TypeSchema::text()),
        StructField::new("description", TypeSchema::text()),
        // The event kind, as the lowercase word it is stored under
        // (`insert`, `login`, …).
        StructField::new("when", TypeSchema::text()),
        // The table, for a table event; null for every other kind.
        StructField::new("channel", TypeSchema::optional(TypeSchema::text())),
        StructField::new("only_if", TypeSchema::optional(TypeSchema::text())),
        StructField::new("action", TypeSchema::text()),
        StructField::new("configuration", TypeSchema::json()),
        StructField::new("min_role", TypeSchema::optional(TypeSchema::int())),
        StructField::new("enabled", TypeSchema::bool()),
        // The periodic timing (§10.2), null on the kinds that have none. Three
        // flat fields rather than a nested object: each is one number, each is
        // one input in the form, and a kind that does not use one refuses it —
        // so a nested shape would only add a level to say the same thing.
        StructField::new("minute", TypeSchema::optional(TypeSchema::int())),
        StructField::new("hour", TypeSchema::optional(TypeSchema::int())),
        StructField::new("day_of_week", TypeSchema::optional(TypeSchema::int())),
    ]
}

/// A registered action and the settings it declares — name, one-line
/// description, and its `config_spec` in the same [`form_field_schema`]
/// vocabulary a framework's and a file-store backend's settings use.
fn action_info_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("name", TypeSchema::text()),
        StructField::new("description", TypeSchema::text()),
        StructField::new("config_spec", TypeSchema::array(form_field_schema())),
    ])
}

/// A registered framework and the settings it declares, so the admin UI can
/// render a form for a framework it knows nothing about (§13.3). Each setting is
/// a [`form_field_schema`] — the same `FormField` vocabulary a row editor uses.
fn framework_info_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("name", TypeSchema::text()),
        // The editorial half: a human name and a sentence saying who each
        // framework is for. The registry owns it, so the picker can present two
        // frameworks as the different propositions they are (§2.4) while staying
        // free of any knowledge of a particular one.
        StructField::new("label", TypeSchema::text()),
        StructField::new("description", TypeSchema::text()),
        StructField::new("config_spec", TypeSchema::array(form_field_schema())),
    ])
}

/// A registered API provider as the application form needs it: the name it is
/// stored under, how to present it, the sub-path it is usually mounted at, and
/// the settings it takes.
///
/// The `config_spec` is the same [`form_field_schema`] a framework's is, and it
/// is here for the same reason (§13.3): the form renders whatever the provider
/// declares, so GraphQL's aggregation switch is a control on a screen that knows
/// nothing about GraphQL, and a provider that grows a setting grows a control
/// without the admin UI being touched.
fn api_provider_info_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("name", TypeSchema::text()),
        StructField::new("label", TypeSchema::text()),
        StructField::new("description", TypeSchema::text()),
        // What the form fills the mount box in with when this provider is
        // picked, so the common case is no typing at all.
        StructField::new("default_mount", TypeSchema::text()),
        StructField::new("config_spec", TypeSchema::array(form_field_schema())),
        // Whether this provider serves custom SQL queries, i.e. whether the
        // application form offers the query editor for it. Declared by the
        // provider for the same reason its settings are: the form renders what
        // it is told rather than checking for a provider by name.
        StructField::new("supports_custom_queries", TypeSchema::bool()),
    ])
}

/// The body `describeCustomQuery` takes: one custom SQL query as the editor
/// holds it, plus the tables the application it belongs to declares.
///
/// It is the stored [`CustomQuery`](crate::CustomQuery) shape rather than "just
/// the SQL and the parameters" so that the *whole* refusal an eventual save
/// would give arrives from the check button: a name a table endpoint already
/// holds and a path a table's own routes already answer are both about this
/// query, and learning about them at save time — after the SQL has been declared
/// fine — is two round trips to fix one query. `tables` is the application's
/// declared subset; an application not yet created sends the ones typed into the
/// form, and an empty list simply skips those two rules.
fn custom_query_input_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("name", TypeSchema::text()),
        StructField::new("description", TypeSchema::optional(TypeSchema::text())),
        StructField::new("method", TypeSchema::text()),
        StructField::new("path", TypeSchema::text()),
        StructField::new("sql", TypeSchema::text()),
        StructField::new(
            "params",
            TypeSchema::array(TypeSchema::struct_of([
                StructField::new("name", TypeSchema::text()),
                StructField::new("type", TypeSchema::text()),
                StructField::new("required", TypeSchema::optional(TypeSchema::bool())),
            ])),
        ),
        StructField::new("min_role", TypeSchema::optional(TypeSchema::int())),
        StructField::new(
            "tables",
            TypeSchema::optional(TypeSchema::array(TypeSchema::text())),
        ),
    ])
}

/// One column of a custom query's result, as the database described it: the name
/// it arrives under in the JSON, and the wire type it was mapped to.
fn query_column_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("name", TypeSchema::text()),
        StructField::new("type", TypeSchema::text()),
    ])
}

/// The whole settings screen in one response: what may be set, and what is set.
///
/// `values` is opaque JSON — a bag keyed by the declared settings' names — for
/// the same reason a file store's `config` is: its shape is the declarations',
/// which are data, and a static type could only describe it by freezing it.
/// Secrets in it are the redaction sentinel, never the stored value.
fn settings_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("sections", TypeSchema::array(settings_section_schema())),
        StructField::new("values", TypeSchema::json()),
    ])
}

/// One group of settings: its heading, what it is for, and its keys.
fn settings_section_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("name", TypeSchema::text()),
        StructField::new("label", TypeSchema::text()),
        StructField::new("description", TypeSchema::text()),
        StructField::new("fields", TypeSchema::array(settings_field_schema())),
    ])
}

/// A settings key: the same declaration every other configurable thing carries,
/// plus the sentence a settings screen has room to put under the control.
fn settings_field_schema() -> TypeSchema {
    let TypeSchema::Struct(fields) = form_field_schema() else {
        // `form_field_schema` is a struct literal one function away; this arm
        // exists because the type says it might not be, not because it can.
        return form_field_schema();
    };
    TypeSchema::struct_of(
        fields
            .into_iter()
            .chain([StructField::new("help", TypeSchema::text())]),
    )
}

/// One settings field of a framework's `config_spec`: enough for the admin UI to
/// render and label an input control for it.
fn form_field_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("name", TypeSchema::text()),
        StructField::new("label", TypeSchema::text()),
        StructField::new("type", TypeSchema::text()),
        StructField::new("required", TypeSchema::bool()),
        StructField::new("default", TypeSchema::optional(TypeSchema::json())),
        StructField::new("options", TypeSchema::array(TypeSchema::json())),
        StructField::new("multiline", TypeSchema::bool()),
        // Whether the value is a secret (§11.1): the form renders a password
        // input, and what it is handed for this field is the redaction
        // sentinel, never the stored key.
        StructField::new("secret", TypeSchema::bool()),
    ])
}
