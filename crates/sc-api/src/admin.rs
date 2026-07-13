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

    // --- files (file manager) ----------------------------------------------

    // The connected file stores (name + whether they are a git repo).
    set.register(
        Endpoint::new("listFileStores", Method::Get, api().lit("file-stores"))
            .output(TypeSchema::array(file_store_schema()))
            .auth(AuthRequirement::admin()),
    );

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

/// Email + password, shared by first-user and login.
fn credentials_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("email", TypeSchema::text()),
        StructField::new("password", TypeSchema::text()),
    ])
}

/// The public view of a user (never the password hash).
fn user_summary_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("id", TypeSchema::uuid()),
        StructField::new("email", TypeSchema::text()),
        StructField::new("role", TypeSchema::int()),
    ])
}

/// A table in the catalog.
fn table_schema() -> TypeSchema {
    TypeSchema::struct_of([StructField::new("name", TypeSchema::text())])
}

/// A field (column) of a table.
fn field_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("name", TypeSchema::text()),
        StructField::new("sql_type", TypeSchema::text()),
        StructField::new("nullable", TypeSchema::bool()),
    ])
}

/// A connected file store.
fn file_store_schema() -> TypeSchema {
    TypeSchema::struct_of([
        StructField::new("name", TypeSchema::text()),
        StructField::new("is_git_repo", TypeSchema::bool()),
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
