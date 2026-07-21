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

use crate::auth::{credentials_schema, user_summary_schema};
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
        .input(TypeSchema::struct_of([
            StructField::new("name", TypeSchema::text()),
            StructField::new("sql_type", TypeSchema::text()),
            StructField::new("nullable", TypeSchema::bool()),
        ]))
        .output(field_schema())
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
        .output(TypeSchema::struct_of([StructField::new(
            "deleted",
            TypeSchema::bool(),
        )]))
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

    // --- frameworks ---------------------------------------------------------
    // The registered frameworks with their settings spec, so the create/edit
    // form can render controls for a framework it knows nothing about (§13.3).
    set.register(
        Endpoint::new("listFrameworks", Method::Get, api().lit("frameworks"))
            .output(TypeSchema::array(framework_info_schema()))
            .auth(AuthRequirement::admin()),
    );

    // --- users --------------------------------------------------------------

    set.register(
        Endpoint::new("listUsers", Method::Get, api().lit("users"))
            .output(TypeSchema::array(user_summary_schema()))
            .auth(AuthRequirement::admin()),
    );

    set.register(
        Endpoint::new("createUser", Method::Post, api().lit("users"))
            .input(TypeSchema::struct_of([
                StructField::new("email", TypeSchema::text()),
                StructField::new("password", TypeSchema::text()),
                StructField::new("role", TypeSchema::int()),
            ]))
            .output(user_summary_schema())
            .auth(AuthRequirement::admin()),
    );

    set
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

/// A field (column) of a table.
fn field_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("name", TypeSchema::text()),
        StructField::new("sql_type", TypeSchema::text()),
        StructField::new("nullable", TypeSchema::bool()),
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

/// A registered file-store backend and the settings it declares, so the admin UI
/// can render a form for a backend it knows nothing about. Each setting is a
/// [`form_field_schema`] — the same `FormField` vocabulary a row editor and a
/// framework's settings use.
fn backend_info_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("name", TypeSchema::text()),
        StructField::new("config_spec", TypeSchema::array(form_field_schema())),
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

/// A freshly created application, plus what scaffolding it did (§2.3).
///
/// Only `create` carries these: a `react` app's project is generated on its first
/// save, and the admin should see that it happened — or why it did not — without
/// a second request. `scaffolded` is a summary line; `scaffold_error` explains a
/// scaffold that was refused (an occupied directory, an unreachable store) on an
/// application that was nonetheless created, since the row is valid either way.
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
    ])
}
