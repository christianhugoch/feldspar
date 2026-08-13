//! The generated TypeScript client must type-check against the declared
//! endpoints (design §13.1; Phase 6 "typed client" tasks).
//!
//! This exercises the full contract for both API surfaces the crate generates a
//! client for: the fixed admin endpoint set, and a [`sc_api::RestProvider`]
//! projection of an application's tables. Each generates the client, writes it
//! next to a strict-mode usage file that
//! calls a representative set of endpoints with correctly-typed arguments, and
//! runs the TypeScript compiler in `--noEmit --strict` mode. A drift between a
//! declared endpoint and the generated call (wrong arg shape, wrong return type)
//! is a `tsc` error, so a green run proves the client is genuinely typed rather
//! than `any`.
//!
//! `tsc` is not always present, so the test resolves a compiler from (in order)
//! the `SC_TSC` env var or a `node_modules/.bin/tsc` under the crate, and
//! **skips** (rather than fails) when none is available — mirroring how the
//! DB-backed tests gate on a configured database. Run it locally with, e.g.:
//!
//! ```text
//! npm install typescript@5 --no-save --prefix crates/sc-api
//! cargo test -p sc-api --test typescript_typecheck
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use sc_api::{ApiProvider, RestProvider};
use sc_catalog::{AccessRules, DataField, DbId, Table, TableId, TableSource};
use sc_types::{BasicType, TypeRef};

/// A usage module that calls a cross-section of the admin endpoints with
/// correctly-typed arguments and consumes their typed results. It fails to
/// compile if the generated client drifts from the declared endpoints.
const USAGE_TS: &str = r#"
import { createClient, LoginResponse } from "./client";

export async function exercise(): Promise<void> {
  const api = createClient({ baseUrl: "http://localhost:3032" });

  // No-body GET returning a struct with a nested optional.
  const status = await api.authStatus();
  const exists: boolean = status.any_user_exists;

  // POST with a typed body and typed response.
  const user: LoginResponse = await api.login({ email: "a@b.c", password: "pw" });
  const role: number = user.role;

  // Path-parameter endpoints.
  const rows = await api.listRows("mytable");
  await api.updateRow("mytable", "some-id", { any: "json" });
  await api.deleteRow("mytable", "some-id");

  void exists; void role; void rows;
}
"#;

/// A usage module for an application's REST client. The app declares one table,
/// `posts`, with a `bigint` key and a `text` column, so its projected client must
/// type `updatePosts`/`deletePosts` to take a `number` id — passing a string is a
/// compile error, which is the whole point of generating the client.
const APP_USAGE_TS: &str = r#"
import { createClient, ListPostsQuery } from "./client";

export async function exercise(): Promise<void> {
  const api = createClient({ baseUrl: "https://blog.example.com" });

  // The app's own tables are methods, not stringly-typed arguments.
  const posts = await api.listPosts();
  const created = await api.createPosts({ title: "hello" });

  // A read's query string is typed too (§13.4): `select`/`order`/`limit`/
  // `offset` are scalars, and the filters are an explicit map of column to
  // `op.value` — honest about being a string vocabulary.
  const query: ListPostsQuery = {
    select: "title,author(name,country)",
    order: "published.desc",
    limit: 20,
    filter: { published: "gte.2020-01-01" },
  };
  const page = await api.listPosts(query);

  // The primary key is typed from the column: `id bigint` => number.
  await api.updatePosts(1, { title: "goodbye" });
  await api.deletePosts(1);

  void posts; void created; void page;
}
"#;

/// A usage module for an endpoint set that declares query parameters. It proves
/// the three shapes are genuinely typed: the options object is omissible when
/// every parameter is optional, required when one is not, a repeated parameter
/// is an array, and each parameter carries the type it was declared with.
const QUERY_USAGE_TS: &str = r#"
import { createClient, ListBooksQuery } from "./client";

export async function exercise(): Promise<void> {
  const api = createClient({});

  // All-optional parameters: the argument itself may be left out.
  await api.listBooks();
  const query: ListBooksQuery = {
    select: "title,author(name)",
    limit: 20,
    published: ["gte.2020-01-01", "lt.2024-01-01"],
  };
  const books = await api.listBooks(query);

  // A required parameter makes the object required, and it is typed.
  const found = await api.searchBooks({ q: "dune" });

  void books; void found;
}
"#;

#[test]
fn generated_query_parameter_client_type_checks() -> std::io::Result<()> {
    use sc_api::{Endpoint, EndpointSet, Method, PathSpec, QueryParam, TypeSchema, ValueType};

    let set = EndpointSet::new()
        .with(
            Endpoint::new("listBooks", Method::Get, PathSpec::root().lit("api/books"))
                .query([
                    QueryParam::new("select", ValueType::Text),
                    QueryParam::new("limit", ValueType::Int),
                    QueryParam::new("published", ValueType::Text).repeated(),
                ])
                .output(TypeSchema::array(TypeSchema::json())),
        )
        .with(
            Endpoint::new(
                "searchBooks",
                Method::Get,
                PathSpec::root().lit("api/search"),
            )
            .query([
                QueryParam::new("q", ValueType::Text).required(),
                QueryParam::new("limit", ValueType::Int),
            ])
            .output(TypeSchema::array(TypeSchema::json())),
        );
    let client_ts = sc_api::generate_client(&set);
    type_check("query-params", &client_ts, QUERY_USAGE_TS)
}

#[test]
fn generated_admin_client_type_checks() -> std::io::Result<()> {
    let client_ts = sc_api::generate_client(&sc_api::admin_endpoints());
    type_check("admin", &client_ts, USAGE_TS)
}

#[test]
fn generated_app_rest_client_type_checks() -> std::io::Result<()> {
    // A projection needs only `Table` values, so this stays a unit-speed test
    // with no database: the client an app gets is a pure function of its tables.
    let posts = Table {
        id: TableId("posts".to_owned()),
        name: "posts".to_owned(),
        database: DbId::primary(),
        source: TableSource::Database,
        fields: vec![
            DataField::plain("id", TypeRef::Basic(BasicType::Int)).primary_key(),
            DataField::plain("title", TypeRef::Basic(BasicType::Text)),
        ],
        primary_key: vec!["id".to_owned()],
        label: "posts".to_owned(),
        description: String::new(),
        access: AccessRules::default(),
        attributes: Default::default(),
        overlay: None,
        ownership: None,
        ownership_error: None,
        rls_enabled: false,
    };
    let provider = RestProvider::project("/api", &[posts]);
    let client_ts = sc_api::generate_client(ApiProvider::endpoints(&provider));
    type_check("app-rest", &client_ts, APP_USAGE_TS)
}

/// Type-check `client_ts` against `usage_ts` with `tsc --noEmit --strict`,
/// skipping when no compiler is available.
fn type_check(tag: &str, client_ts: &str, usage_ts: &str) -> std::io::Result<()> {
    let Some(tsc) = resolve_tsc() else {
        eprintln!(
            "skipping: no TypeScript compiler found. Set SC_TSC=/path/to/tsc or run \
             `npm install typescript@5 --no-save --prefix crates/sc-api`."
        );
        return Ok(());
    };

    let dir = std::env::temp_dir().join(format!("sc-api-tsc-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("client.ts"), client_ts)?;
    std::fs::write(dir.join("usage.ts"), usage_ts)?;

    let output = Command::new(&tsc)
        .args([
            "--noEmit",
            "--strict",
            "--lib",
            "es2020,dom",
            "--moduleResolution",
            "node",
            "--module",
            "esnext",
        ])
        .arg(dir.join("client.ts"))
        .arg(dir.join("usage.ts"))
        .output()
        .unwrap_or_else(|e| panic!("failed to run tsc at {}: {e}", tsc.display()));

    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        output.status.success(),
        "generated {tag} TypeScript failed to type-check:\n--- stdout ---\n{}\n--- stderr ---\n{}\n--- client.ts ---\n{client_ts}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    Ok(())
}

/// Resolve a TypeScript compiler: `SC_TSC`, else a `node_modules/.bin/tsc`
/// installed under the crate. Returns `None` when neither is present.
fn resolve_tsc() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SC_TSC") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    let local = Path::new(env!("CARGO_MANIFEST_DIR")).join("node_modules/.bin/tsc");
    local.exists().then_some(local)
}
