#![allow(clippy::unwrap_used, clippy::expect_used)]

//! `manage_table_admin` against a **real** Postgres (TODO Phase 7).
//!
//! Everything this trait does is DDL, transactions and catalog reloads, so a
//! mock would confirm only that the seam was called. What is pinned here is the
//! behaviour a mock cannot have: a two-table batch whose foreign key actually
//! constrains rows, a batch whose third operation is refused leaving *nothing*
//! applied, the overlay rows going with the things they describe, and — for the
//! access controls — real row-level-security policies deciding two callers'
//! reads differently.

mod common;

use common::{Env, as_user, config};
use sc_agent::RunCaller;
use sc_error::Result;
use serde_json::{Value as Json, json};

const TRAIT: &str = "manage_table_admin";

/// Everything granted — what an admin who ticked all four boxes has.
fn all_grants() -> sc_types::Attrs {
    config(&[
        ("allow_create", json!(true)),
        ("allow_edit", json!(true)),
        ("allow_drop", json!(true)),
        ("allow_access_changes", json!(true)),
    ])
}

/// The default configuration: build and edit, never drop, never widen access.
fn default_grants() -> sc_types::Attrs {
    config(&[])
}

/// An admin conversation — the only caller either tool answers.
fn admin() -> RunCaller {
    RunCaller::system()
}

async fn edit(env: &Env, cfg: &sc_types::Attrs, args: Json) -> Result<Json> {
    env.call_tool(TRAIT, cfg, "edit_schema", args, &admin())
        .await
}

async fn describe(env: &Env, cfg: &sc_types::Attrs, args: Json) -> Result<Json> {
    env.call_tool(TRAIT, cfg, "describe_schema", args, &admin())
        .await
}

/// The law-firm batch, in miniature: two connected tables in one call.
///
/// Each declares its own key, because nothing invents one (GOALS) — and the
/// second table's foreign key resolves against the first's, in the same batch.
fn erp_batch() -> Json {
    json!({"operations": [
        {
            "op": "create_table", "table": "clients",
            "description": "A client of the firm",
            "fields": [
                {"name": "id", "type": "int", "primary_key": true},
                {"name": "name", "type": "text", "required": true},
                {"name": "vat_number", "type": "text", "unique": true},
            ],
        },
        {
            "op": "create_table", "table": "matters",
            "fields": [
                {"name": "id", "type": "int", "primary_key": true},
                {"name": "title", "type": "text", "required": true},
                {"name": "client", "references": "clients", "summary_field": "name"},
            ],
        },
    ]})
}

#[tokio::test]
async fn a_batch_builds_connected_tables_and_the_key_really_constrains_rows() -> Result<()> {
    let env = Env::new().await?;
    let result = edit(&env, &default_grants(), erp_batch()).await?;
    assert_eq!(result["applied"], json!(true));
    assert_eq!(
        result["tables_created"],
        json!(["clients", "matters"]),
        "{result}"
    );

    // The key is in the catalog as a `Key` field pointing at `clients`, with the
    // storage type taken from that table's primary key — never asked for.
    let matters = env.catalog.require("matters")?;
    let client = matters.field("client").expect("the key field");
    match &client.kind {
        sc_catalog::DataFieldKind::Key {
            target_table,
            target_field,
            summary_field,
        } => {
            assert_eq!(target_table.0, "clients");
            assert_eq!(target_field.0, "id");
            assert_eq!(summary_field.as_ref().map(|f| f.0.as_str()), Some("name"));
        }
        other => panic!("expected a key, got {other:?}"),
    }
    // The key the batch declared — nothing invents one (GOALS) — and an `int`
    // key numbers itself, which is why the inserts below name no id.
    assert_eq!(matters.primary_key, vec!["id".to_owned()]);
    assert!(matters.field("id").expect("the key field").primary_key);

    // And the foreign key is real: a row inserts through it, and one pointing at
    // a client that does not exist does not.
    env.execute("INSERT INTO clients (name) VALUES ('Acme');")
        .await?;
    env.execute("INSERT INTO matters (title, client) VALUES ('Acme v. Rival', 1);")
        .await?;
    assert_eq!(env.rows("matters").await?.len(), 1);
    assert!(
        env.execute("INSERT INTO matters (title, client) VALUES ('Nobody', 999);")
            .await
            .is_err(),
        "the key constrains rows, it is not decoration"
    );
    Ok(())
}

#[tokio::test]
async fn a_batch_whose_third_operation_is_invalid_applies_nothing_and_names_the_index() -> Result<()>
{
    let env = Env::new().await?;
    let err = edit(
        &env,
        &default_grants(),
        json!({"operations": [
            {"op": "create_table", "table": "clients",
             "fields": [{"name": "id", "type": "int", "primary_key": true},
                        {"name": "name", "type": "text"}]},
            {"op": "create_table", "table": "matters",
             "fields": [{"name": "client", "references": "clients"}]},
            {"op": "add_field", "table": "matters", "field": "fee", "type": "guilders"},
        ]}),
    )
    .await
    .unwrap_err()
    .to_string();

    assert!(err.contains("operation 2"), "{err}");
    assert!(err.contains("guilders"), "{err}");
    // Nothing applied — not the two tables the first two operations would have
    // made, not the columns of either.
    env.catalog.reload().await?;
    assert!(env.catalog.get("clients")?.is_none(), "nothing was applied");
    assert!(env.catalog.get("matters")?.is_none(), "nothing was applied");
    Ok(())
}

#[tokio::test]
async fn a_dry_run_refuses_the_same_way_and_changes_nothing() -> Result<()> {
    let env = Env::new().await?;
    // The same refusal as the wet run, and no tables.
    let mut bad = erp_batch();
    bad["operations"][1]["fields"][1]["references"] = json!("no_such_table");
    bad["dry_run"] = json!(true);
    let err = edit(&env, &default_grants(), bad)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no_such_table"), "{err}");
    env.catalog.reload().await?;
    assert!(env.catalog.get("clients")?.is_none());

    // A dry run of a *valid* batch reports what it would do and does none of it.
    let mut ok = erp_batch();
    ok["dry_run"] = json!(true);
    let result = edit(&env, &default_grants(), ok).await?;
    assert_eq!(result["dry_run"], json!(true));
    assert_eq!(result["applied"], json!(false));
    assert_eq!(result["tables_created"], json!(["clients", "matters"]));
    env.catalog.reload().await?;
    assert!(
        env.catalog.get("clients")?.is_none(),
        "a dry run applies nothing"
    );
    Ok(())
}

#[tokio::test]
async fn a_drop_needs_its_grant_and_takes_the_overlay_rows_with_it() -> Result<()> {
    let env = Env::new().await?;
    edit(&env, &default_grants(), erp_batch()).await?;
    // `create_table` carrying a description already wrote `clients` an overlay
    // row; edit *that* row rather than making a second, which is refused.
    let mut meta = sc_catalog::load_table_meta_by_name(&env.catalog, "clients")
        .await?
        .expect("the description's row");
    meta.label = "Clients".to_owned();
    meta.access = sc_catalog::AccessRules {
        min_role_read: 40,
        min_role_write: 40,
    };
    sc_catalog::save_table_meta(&env.catalog, &meta).await?;

    // Dropping the *referenced* table is refused by name, listing the fields to
    // remove first — a foreign-key error out of Postgres is not something a
    // model can act on.
    let err = edit(
        &env,
        &all_grants(),
        json!({"operations": [{"op": "drop_table", "table": "clients"}]}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("matters.client"), "{err}");
    assert!(env.catalog.get("clients")?.is_some());

    // Without the grant, dropping the *unreferenced* table is refused too — and
    // the refusal names the checkbox.
    let err = edit(
        &env,
        &default_grants(),
        json!({"operations": [{"op": "drop_table", "table": "matters"}]}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("allow_drop"), "{err}");
    assert!(env.catalog.get("matters")?.is_some());

    // With it, both go — `matters` first, then `clients`, in one batch.
    let result = edit(
        &env,
        &all_grants(),
        json!({"operations": [
            {"op": "drop_table", "table": "matters"},
            {"op": "drop_table", "table": "clients"},
        ]}),
    )
    .await?;
    assert_eq!(result["tables_dropped"], json!(["matters", "clients"]));
    assert!(env.catalog.get("matters")?.is_none());
    assert!(env.catalog.get("clients")?.is_none());
    // The overlay went with the table. A row left behind would be
    // indistinguishable from §1.1's deliberately-kept orphan.
    assert!(
        sc_catalog::load_table_meta_by_name(&env.catalog, "clients")
            .await?
            .is_none(),
        "the settings row goes with the table"
    );
    assert!(
        sc_catalog::orphan_table_meta(&env.catalog)
            .await?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn a_dropped_field_takes_its_overlay_row_and_refuses_what_the_database_would() -> Result<()> {
    let env = Env::new().await?;
    edit(&env, &default_grants(), erp_batch()).await?;
    // The `client` key has an overlay row (a key always does).
    assert!(
        sc_catalog::load_field_meta_by_field(&env.catalog, "matters", "client")
            .await?
            .is_some()
    );

    // The primary key is refused by name, before any DDL.
    let err = edit(
        &env,
        &all_grants(),
        json!({"operations": [{"op": "drop_field", "table": "matters", "field": "id"}]}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("primary key"), "{err}");

    // A field another table's key points at is refused, naming that field.
    let err = edit(
        &env,
        &all_grants(),
        json!({"operations": [{"op": "drop_field", "table": "clients", "field": "id"}]}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("primary key") || err.contains("matters.client"),
        "{err}"
    );

    let result = edit(
        &env,
        &all_grants(),
        json!({"operations": [{"op": "drop_field", "table": "matters", "field": "client"}]}),
    )
    .await?;
    assert_eq!(result["fields_dropped"], json!(["matters.client"]));
    assert!(env.catalog.require("matters")?.field("client").is_none());
    assert!(
        sc_catalog::load_field_meta_by_field(&env.catalog, "matters", "client")
            .await?
            .is_none(),
        "the field's settings row goes with the column"
    );
    Ok(())
}

#[tokio::test]
async fn neither_tool_answers_a_caller_who_is_not_an_admin() -> Result<()> {
    let env = Env::new().await?;
    let cfg = all_grants();
    for tool in ["describe_schema", "edit_schema"] {
        let err = env
            .call_tool(TRAIT, &cfg, tool, json!({}), &as_user("ada@example.com"))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("administrator"), "{tool}: {err}");
        assert!(err.contains("role-40"), "{tool}: {err}");
    }
    // …and nothing was made by the refused edit.
    assert!(env.catalog.get("clients")?.is_none());
    Ok(())
}

#[tokio::test]
async fn the_system_tables_are_invisible_to_one_tool_and_refused_by_the_other() -> Result<()> {
    let env = Env::new().await?;
    sc_auth::bootstrap(&env.catalog).await?;

    let described = describe(&env, &all_grants(), json!({})).await?;
    let names: Vec<String> = described["tables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(names.contains(&"books".to_owned()));
    assert!(names.contains(&"users".to_owned()), "{names:?}");
    assert!(
        !names.iter().any(|n| n.starts_with("_sc_")),
        "system tables are not described: {names:?}"
    );

    // `_sc_agents` is refused whatever the grant says.
    for op in [
        json!({"op": "add_field", "table": "_sc_agents", "field": "x", "type": "text"}),
        json!({"op": "drop_table", "table": "_sc_agents"}),
        json!({"op": "create_table", "table": "_sc_mine"}),
    ] {
        let err = edit(&env, &all_grants(), json!({"operations": [op]}))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("system table") || err.contains("reserved"),
            "{err}"
        );
    }

    // `users` is described and may gain a field, but is never dropped and never
    // loses a built-in column.
    let err = edit(
        &env,
        &all_grants(),
        json!({"operations": [{"op": "drop_table", "table": "users"}]}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("built-in table"), "{err}");
    let err = edit(
        &env,
        &all_grants(),
        json!({"operations": [{"op": "drop_field", "table": "users", "field": "email"}]}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("built-in column"), "{err}");
    edit(
        &env,
        &all_grants(),
        json!({"operations": [
            {"op": "add_field", "table": "users", "field": "nickname", "type": "text"},
        ]}),
    )
    .await?;
    assert!(env.catalog.require("users")?.field("nickname").is_some());
    Ok(())
}

#[tokio::test]
async fn describing_a_table_full_of_rows_reports_none_of_their_values() -> Result<()> {
    let env = Env::new().await?;
    env.own("books", "owner === user.email").await?;
    let described = describe(&env, &default_grants(), json!({"table": "books"})).await?;
    let text = described.to_string();

    // Not one row value, and no count of them: a count is data, and this tool has
    // checked nobody's §7.3 grant to report it.
    for value in ["Dune", "Emma", "Ilium", "Ubik", "ada@example.com"] {
        assert!(
            !text.contains(value),
            "`{value}` leaked into the description"
        );
    }
    assert!(!text.contains("row_count"), "{text}");

    let table = &described["tables"][0];
    assert_eq!(table["name"], json!("books"));
    // The formula **source**, not merely a boolean: a tool that may write it and
    // can only read a flag has no way to edit one except by overwriting it blind.
    assert_eq!(table["ownership_formula"], json!("owner === user.email"));
    assert_eq!(table["ownership_error"], Json::Null);
    assert_eq!(table["rls_enabled"], json!(false));
    assert_eq!(table["rls_available"], json!(true));
    assert_eq!(table["min_role_read"], json!(1));
    let fields: Vec<String> = table["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(fields, vec!["id", "title", "pages", "owner", "notes"]);
    assert_eq!(table["fields"][0]["primary_key"], json!(true));
    Ok(())
}

#[tokio::test]
async fn relationships_are_reported_in_both_directions() -> Result<()> {
    let env = Env::new().await?;
    edit(&env, &default_grants(), erp_batch()).await?;
    let described = describe(&env, &default_grants(), json!({})).await?;
    let table = |name: &str| -> Json {
        described["tables"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == json!(name))
            .cloned()
            .unwrap()
    };
    // "what points at clients?" is the question a schema is asked, and no single
    // field answers it.
    assert_eq!(
        table("clients")["referenced_by"],
        json!([{"table": "matters", "field": "client", "target_field": "id"}])
    );
    assert_eq!(
        table("matters")["references"],
        json!([{"field": "client", "target_table": "clients", "target_field": "id"}])
    );
    assert_eq!(table("clients")["references"], json!([]));
    Ok(())
}

#[tokio::test]
async fn altering_a_table_leaves_the_settings_it_does_not_name() -> Result<()> {
    let env = Env::new().await?;
    env.own("books", "owner === user.email").await?;
    let mut meta = sc_catalog::load_table_meta_by_name(&env.catalog, "books")
        .await?
        .expect("the formula's row");
    meta.label = "The Library".to_owned();
    meta.access = sc_catalog::AccessRules {
        min_role_read: 20,
        min_role_write: 10,
    };
    sc_catalog::save_table_meta(&env.catalog, &meta).await?;

    // Without the grant, naming an access setting refuses the whole batch.
    let err = edit(
        &env,
        &default_grants(),
        json!({"operations": [{"op": "alter_table", "table": "books", "min_role_read": 40}]}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("allow_access_changes"), "{err}");
    assert_eq!(env.catalog.require("books")?.access.min_role_read, 20);

    // With it, only what was named moves. This is the omitted-means-leave rule,
    // and the test that would have caught `updateTable`'s whole-object contract
    // arriving here by accident.
    edit(
        &env,
        &all_grants(),
        json!({"operations": [{"op": "alter_table", "table": "books", "min_role_read": 40}]}),
    )
    .await?;
    let books = env.catalog.require("books")?;
    assert_eq!(books.access.min_role_read, 40);
    assert_eq!(
        books.access.min_role_write, 10,
        "the write floor was not named"
    );
    assert_eq!(
        books.ownership.as_ref().map(|f| f.source().to_owned()),
        Some("owner === user.email".to_owned()),
        "the formula was not named, so it stayed"
    );
    assert_eq!(books.label, "The Library", "the label was not named");
    assert!(!books.rls_enabled);
    Ok(())
}

#[tokio::test]
async fn a_formula_may_name_a_field_the_same_batch_added() -> Result<()> {
    let env = Env::new().await?;
    // "add an `owner` field, then set the ownership formula to `owner === user.id`"
    // is the obvious thing to ask for, and must not fail on the second operation.
    edit(
        &env,
        &all_grants(),
        json!({"operations": [
            {"op": "create_table", "table": "notes",
             "fields": [{"name": "body", "type": "text"}]},
            {"op": "add_field", "table": "notes", "field": "owner", "type": "text"},
            {"op": "alter_table", "table": "notes",
             "ownership_formula": "owner === user.email"},
        ]}),
    )
    .await?;
    let notes = env.catalog.require("notes")?;
    assert_eq!(
        notes.ownership.as_ref().map(|f| f.source().to_owned()),
        Some("owner === user.email".to_owned())
    );
    assert_eq!(notes.ownership_error, None);

    // A formula naming a field that is *not* there is refused, and nothing of
    // the batch survives.
    let err = edit(
        &env,
        &all_grants(),
        json!({"operations": [
            {"op": "create_table", "table": "memos",
             "fields": [{"name": "body", "type": "text"}]},
            {"op": "alter_table", "table": "memos", "ownership_formula": "keeper === user.email"},
        ]}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("keeper"), "{err}");
    env.catalog.reload().await?;
    assert!(env.catalog.get("memos")?.is_none());
    Ok(())
}

#[tokio::test]
async fn enabling_row_level_security_is_refused_unless_the_formula_translates() -> Result<()> {
    let env = Env::new().await?;
    sc_auth::bootstrap(&env.catalog).await?;

    // A formula that cannot be turned into policies — a JavaScript call the
    // symbolic translator has no SQL for — is refused, with nothing written.
    let err = edit(
        &env,
        &all_grants(),
        json!({"operations": [{
            "op": "alter_table", "table": "books",
            "ownership_formula": "owner.toUpperCase() === user.email",
            "rls_enabled": true,
        }]}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("row-level security"), "{err}");
    let books = env.catalog.require("books")?;
    assert!(!books.rls_enabled, "nothing was written");
    assert!(books.ownership.is_none(), "nothing was written");
    assert_eq!(policy_count(&env, "books").await?, 0);
    Ok(())
}

#[tokio::test]
async fn enabling_row_level_security_makes_two_callers_read_differently() -> Result<()> {
    let env = Env::new().await?;
    sc_auth::bootstrap(&env.catalog).await?;

    edit(
        &env,
        &all_grants(),
        json!({"operations": [{
            "op": "alter_table", "table": "books",
            "ownership_formula": "owner === user.email",
            "rls_enabled": true,
        }]}),
    )
    .await?;
    assert!(env.catalog.require("books")?.rls_enabled);
    assert_eq!(
        policy_count(&env, "books").await?,
        4,
        "one policy per operation"
    );

    // The database is deciding now, and it decides differently for two callers.
    let query = config(&[("table", json!("books"))]);
    let ada = env
        .call(
            "query_table",
            &query,
            json!({}),
            &as_user("ada@example.com"),
        )
        .await?;
    let bob = env
        .call(
            "query_table",
            &query,
            json!({}),
            &as_user("bob@example.com"),
        )
        .await?;
    assert_eq!(common::titles(&ada), vec!["Dune", "Ilium"]);
    assert_eq!(common::titles(&bob), vec!["Emma", "Ubik"]);

    // Turning it off is the one operation whose damage is invisible in the
    // schema afterwards, so the result says so in words.
    let result = edit(
        &env,
        &all_grants(),
        json!({"operations": [{"op": "alter_table", "table": "books", "rls_enabled": false}]}),
    )
    .await?;
    let notes = result["notes"].to_string();
    assert!(notes.contains("no longer enforced"), "{notes}");
    assert_eq!(
        policy_count(&env, "books").await?,
        0,
        "the policies were dropped"
    );
    // The formula was not named, so it is still there — enforcement went, the
    // rule did not.
    assert!(env.catalog.require("books")?.ownership.is_some());
    Ok(())
}

/// How many row-level-security policies Postgres holds for a table.
async fn policy_count(env: &Env, table: &str) -> Result<i64> {
    let client = env.db.client().await?;
    let row = client
        .query_one(
            "SELECT count(*) FROM pg_policies WHERE tablename = $1",
            &[&table],
        )
        .await
        .map_err(|e| sc_error::Error::database(e.to_string()))?;
    Ok(row.get(0))
}
