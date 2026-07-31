//! `query_table` against a real database (TODO Phase 3).
//!
//! What is pinned here — each of these is a decision the trait would otherwise be
//! free to get wrong quietly:
//!
//! - the tool's **description and schema are the table's**: its fields, their
//!   types, and an `order_by` enumerating exactly the ones that can be ordered on;
//! - **the caller's access is the tool's access**: the same table read by two
//!   users under an ownership formula gives two answers, on both the translated
//!   and the reified paths, and a caller the formula grants nothing gets nothing
//!   rather than an error that leaks the row count;
//! - the **field allow-list** narrows what comes back *and* what may be filtered
//!   or ordered on, so a field left out of it cannot be read through a `where`;
//! - `max_rows` is a **ceiling**, not a default: a larger `limit` is clamped and
//!   the answer says more rows were available;
//! - the `where` operators translate to the comparisons they name, and a field or
//!   an argument the schema did not describe is refused **by name**;
//! - and a trait configured against a **dropped table** leaves its agent out of
//!   the live set with a reason, while still offering its tool under the name the
//!   configuration gives it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use sc_agent::testing::{FakeProvider, Reply};
use sc_agent::{
    Agent, Agents, EnabledTrait, RunCaller, RunId, Runner, TraitCheck, TraitContext,
    bootstrap_agents, bootstrap_runs, save_agent,
};
use sc_auth::User;
use sc_catalog::{Catalog, TableMeta, bootstrap_table_meta, save_table_meta};
use sc_core_traits::{CFG_FIELDS, CFG_MAX_ROWS, CFG_TABLE, builtin_traits};
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_error::{Error, Result};
use sc_expr::{DenoEvaluator, JsEvaluator};
use sc_llm::{LlmProviderDef, bootstrap_llm_providers, save_llm_provider};
use sc_query::Value;
use sc_test_harness::TestDb;
use sc_types::Attrs;
use serde_json::{Value as Json, json};
use uuid::Uuid;

/// A library with two owners, so an ownership formula has something to divide,
/// and a `notes` column an allow-list can keep from the model.
const SCHEMA: &str = "
    CREATE TABLE books (
        id bigint primary key,
        title text,
        pages bigint,
        owner text,
        notes text
    );
    INSERT INTO books VALUES
        (1, 'Dune',   412, 'ada@example.com', 'ada''s copy'),
        (2, 'Emma',   474, 'bob@example.com', 'bob''s copy'),
        (3, 'Ilium',  576, 'ada@example.com', 'also ada''s'),
        (4, 'Ubik',   224, 'bob@example.com', NULL);
";

/// A catalog over a per-test database with the schema, the overlay table and the
/// agent tables, plus one connected provider — an agent that names no connected
/// provider does not validate, so every test would otherwise write the same row.
async fn setup(db: &TestDb) -> Result<Catalog> {
    db.client()
        .await?
        .batch_execute(SCHEMA)
        .await
        .map_err(|e| Error::database(e.to_string()))?;
    let driver = Arc::new(PgDriver::from_pool(db.pool().clone()));
    let catalog = Catalog::init(driver as Arc<dyn DatabaseDriver>).await?;
    bootstrap_table_meta(&catalog).await?;
    bootstrap_llm_providers(&catalog).await?;
    bootstrap_agents(&catalog).await?;
    bootstrap_runs(&catalog).await?;
    save_llm_provider(
        &catalog,
        &LlmProviderDef::anthropic("main", "sk-ant-not-a-real-key", "claude-sonnet-4-5"),
    )
    .await?;
    catalog.reload().await?;
    Ok(catalog)
}

/// Put `formula` on `books` as a runtime ownership rule — **not** RLS, so the
/// §7.3 checks run in `sc-api` rather than in the database, which is the path a
/// tool takes on an ordinary table.
async fn own_books(catalog: &Catalog, formula: &str) -> Result<()> {
    let mut meta = TableMeta::new("books");
    meta.set_ownership_formula(Some(formula));
    save_table_meta(catalog, &meta).await?;
    let books = catalog.require("books")?;
    assert!(books.ownership.is_some(), "the formula is live");
    Ok(())
}

/// A configuration from `(key, value)` pairs.
fn config(entries: &[(&str, Json)]) -> Attrs {
    entries
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

/// The plainest configuration: every field of `books`, default ceiling.
fn books() -> Attrs {
    config(&[(CFG_TABLE, json!("books"))])
}

/// A reader at role 40 — below `books`' admin-only read floor, so their access is
/// whatever the ownership formula grants and nothing else.
fn reader(email: &str) -> User {
    let mut user = User::new(Uuid::new_v4(), 40).unwrap();
    user.extra = BTreeMap::from([("email".to_owned(), Value::Text(email.to_owned()))]);
    user
}

fn engine() -> Arc<dyn JsEvaluator> {
    Arc::new(DenoEvaluator::new())
}

/// Call the trait's tool directly, as `caller`.
async fn query(
    catalog: &Catalog,
    cfg: &Attrs,
    args: Json,
    caller: &RunCaller,
    evaluator: Option<&Arc<dyn JsEvaluator>>,
) -> Result<Json> {
    let registry = builtin_traits()?;
    let trait_ = registry.require("query_table")?.clone();
    let tool = trait_.tools(catalog, cfg)[0].name.clone();
    let mut ctx = TraitContext {
        catalog,
        caller,
        agent: "librarian",
        run: RunId::new(),
        evaluator,
    };
    trait_.call(cfg, &tool, &args, &mut ctx).await
}

/// The `title` of every returned row, in the order they came back.
fn titles(result: &Json) -> Vec<String> {
    result["rows"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| r["title"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[tokio::test]
async fn the_tool_describes_the_table_it_is_configured_against() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = setup(&db).await?;
    let registry = builtin_traits()?;
    let trait_ = registry.require("query_table")?;

    let tools = trait_.tools(&catalog, &books());
    assert_eq!(tools.len(), 1);
    let tool = &tools[0];
    // The name is the configuration's, so two instances are distinguishable.
    assert_eq!(tool.name, "query_books");
    // The description names every field with its type — what the model chooses on.
    for field in ["id", "title", "pages", "owner", "notes"] {
        assert!(tool.description.contains(field), "{}", tool.description);
    }
    assert!(
        tool.description.contains("primary key"),
        "{}",
        tool.description
    );

    // …and so does the schema: the filterable fields are enumerated rather than
    // left to be guessed, and `order_by` is a closed list.
    let props = &tool.parameters["properties"];
    let conditions = props["where"]["properties"].as_object().unwrap();
    assert_eq!(conditions.len(), 5);
    assert!(conditions.contains_key("pages"));
    assert_eq!(props["where"]["additionalProperties"], json!(false));
    let order: Vec<&str> = props["order_by"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(order, vec!["id", "title", "pages", "owner", "notes"]);
    assert_eq!(props["limit"]["maximum"], json!(50));
    Ok(())
}

#[tokio::test]
async fn an_allow_list_narrows_what_is_returned_and_what_may_be_filtered() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = setup(&db).await?;
    let cfg = config(&[
        (CFG_TABLE, json!("books")),
        (CFG_FIELDS, json!(["title", "id"])),
    ]);

    // Declared: the tool only ever mentions the two fields, in the *table's*
    // order rather than the order they were typed.
    let registry = builtin_traits()?;
    let tools = registry.require("query_table")?.tools(&catalog, &cfg);
    let order: Vec<&str> = tools[0].parameters["properties"]["order_by"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(order, vec!["id", "title"]);

    // Returned: `notes` and `owner` are not in the rows…
    let caller = RunCaller::system();
    let result = query(&catalog, &cfg, json!({"limit": 1}), &caller, None).await?;
    let row = result["rows"][0].as_object().unwrap();
    assert_eq!(row.keys().collect::<Vec<_>>(), vec!["id", "title"]);

    // …and, the point of an allow-list, they cannot be read *through* a filter
    // either. A `where` on a hidden field is refused by name.
    let err = query(
        &catalog,
        &cfg,
        json!({"where": {"notes": "ada's copy"}}),
        &caller,
        None,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("notes"), "{err}");
    assert!(err.to_string().contains("id, title"), "{err}");
    Ok(())
}

#[tokio::test]
async fn the_where_operators_translate_to_the_comparisons_they_name() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = setup(&db).await?;
    let cfg = books();
    let caller = RunCaller::system();
    let ask = async |args: Json| -> Result<Vec<String>> {
        Ok(titles(&query(&catalog, &cfg, args, &caller, None).await?))
    };

    // A bare value is equality; `in` is a list; the comparisons compare.
    assert_eq!(ask(json!({"where": {"title": "Emma"}})).await?, ["Emma"]);
    assert_eq!(
        ask(json!({"where": {"title": {"in": ["Dune", "Ubik"]}}, "order_by": "id"})).await?,
        ["Dune", "Ubik"]
    );
    assert_eq!(
        ask(json!({"where": {"pages": {"gte": 474}}, "order_by": "pages"})).await?,
        ["Emma", "Ilium"]
    );
    assert_eq!(
        ask(json!({"where": {"pages": {"lt": 300}}})).await?,
        ["Ubik"]
    );
    assert_eq!(
        ask(json!({"where": {"title": {"like": "U%"}}})).await?,
        ["Ubik"]
    );
    assert_eq!(
        ask(json!({"where": {"title": {"ilike": "u%"}}})).await?,
        ["Ubik"]
    );
    // Null is a test, not a comparison: SQL's `=` is never true of one, and a
    // model writing `{"eq": null}` means "unset".
    assert_eq!(ask(json!({"where": {"notes": null}})).await?, ["Ubik"]);
    assert_eq!(
        ask(json!({"where": {"notes": {"is_null": true}}})).await?,
        ["Ubik"]
    );
    assert_eq!(
        ask(json!({"where": {"notes": {"eq": null}}})).await?,
        ["Ubik"]
    );
    // Two entries are ANDed.
    assert_eq!(
        ask(json!({"where": {"owner": "ada@example.com", "pages": {"gt": 500}}})).await?,
        ["Ilium"]
    );
    // Ordering, both ways.
    assert_eq!(
        ask(json!({"order_by": "pages", "descending": true})).await?,
        ["Ilium", "Emma", "Dune", "Ubik"]
    );

    // A field that does not exist, an operator that does not, and an argument
    // the schema never described are each refused **by name**, because a model
    // that cannot read what it got wrong cannot fix it.
    for (args, needle) in [
        (json!({"where": {"author": "Herbert"}}), "author"),
        (json!({"where": {"pages": {"between": [1, 2]}}}), "pages"),
        (json!({"sql": "drop table books"}), "sql"),
        (json!({"order_by": "nope"}), "nope"),
    ] {
        let err = query(&catalog, &cfg, args, &caller, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains(needle), "{err}");
    }
    Ok(())
}

#[tokio::test]
async fn max_rows_is_a_ceiling_and_a_truncated_answer_says_so() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = setup(&db).await?;
    let cfg = config(&[(CFG_TABLE, json!("books")), (CFG_MAX_ROWS, json!(2))]);
    let caller = RunCaller::system();

    // No `limit` at all: the ceiling applies, and the answer admits it is short.
    let result = query(&catalog, &cfg, json!({"order_by": "id"}), &caller, None).await?;
    assert_eq!(titles(&result), ["Dune", "Emma"]);
    assert_eq!(result["count"], json!(2));
    assert_eq!(result["more_rows_available"], json!(true));

    // A larger `limit` is clamped rather than refused — a recoverable mistake,
    // and the model is told exactly what it got.
    let result = query(
        &catalog,
        &cfg,
        json!({"limit": 100, "order_by": "id"}),
        &caller,
        None,
    )
    .await?;
    assert_eq!(titles(&result), ["Dune", "Emma"]);

    // A smaller one is honoured, and a complete answer says it is complete.
    let result = query(&catalog, &cfg, json!({"limit": 1}), &caller, None).await?;
    assert_eq!(result["count"], json!(1));
    assert_eq!(result["more_rows_available"], json!(true));
    let result = query(
        &catalog,
        &cfg,
        json!({"where": {"title": "Emma"}}),
        &caller,
        None,
    )
    .await?;
    assert_eq!(result["count"], json!(1));
    assert_eq!(result["more_rows_available"], json!(false));
    Ok(())
}

#[tokio::test]
async fn the_same_table_read_by_two_callers_gives_two_answers() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = setup(&db).await?;
    own_books(&catalog, "owner === user.email").await?;
    let cfg = books();

    // Each reader sees their own rows and nobody else's, from one configuration
    // and one tool. This is not a check the trait performs — it is one it cannot
    // skip, because the read goes through `sc_api::read_rows_as`.
    let ada = RunCaller::user(reader("ada@example.com"));
    let bob = RunCaller::user(reader("bob@example.com"));
    let sorted = json!({"order_by": "id"});
    assert_eq!(
        titles(&query(&catalog, &cfg, sorted.clone(), &ada, None).await?),
        ["Dune", "Ilium"]
    );
    assert_eq!(
        titles(&query(&catalog, &cfg, sorted.clone(), &bob, None).await?),
        ["Emma", "Ubik"]
    );

    // A filter cannot reach past the formula: asking for a row you do not own
    // returns nothing, not that row.
    let hidden = json!({"where": {"title": "Emma"}});
    let result = query(&catalog, &cfg, hidden, &ada, None).await?;
    assert_eq!(result["count"], json!(0));

    // …and a reader the formula grants nothing sees an empty table rather than
    // an error that would tell them the rows are there.
    let nobody = RunCaller::user(reader("nobody@example.com"));
    let result = query(&catalog, &cfg, sorted.clone(), &nobody, None).await?;
    assert_eq!(result["count"], json!(0));

    // A trigger-started run carries the trigger's authority instead (decision 5),
    // which clears the floor and sees everything.
    let result = query(&catalog, &cfg, sorted, &RunCaller::system(), None).await?;
    assert_eq!(result["count"], json!(4));
    Ok(())
}

#[tokio::test]
async fn an_untranslatable_formula_is_enforced_row_by_row_and_still_bounds_the_answer() -> Result<()>
{
    let db = TestDb::new().await?;
    let catalog = setup(&db).await?;
    // The same rule, spelled so it cannot become SQL — the reified path, where
    // the evaluator decides per row.
    own_books(&catalog, "[owner].some(o => o === user.email)").await?;
    let cfg = config(&[(CFG_TABLE, json!("books")), (CFG_MAX_ROWS, json!(1))]);
    let engine = engine();
    let ada = RunCaller::user(reader("ada@example.com"));

    // The ceiling counts rows the caller may **see**: applied before the
    // evaluator spoke it would return one of Ada's two and claim there were no
    // more, or return nothing at all when row 1 happened to be Bob's.
    let result = query(
        &catalog,
        &cfg,
        json!({"order_by": "id"}),
        &ada,
        Some(&engine),
    )
    .await?;
    assert_eq!(titles(&result), ["Dune"]);
    assert_eq!(result["more_rows_available"], json!(true));

    // Widening the ceiling shows exactly Ada's rows, in the ordering the
    // database applied before the filtering.
    let cfg = books();
    let result = query(
        &catalog,
        &cfg,
        json!({"order_by": "id", "descending": true}),
        &ada,
        Some(&engine),
    )
    .await?;
    assert_eq!(titles(&result), ["Ilium", "Dune"]);

    // Without an engine the tool says so rather than reading anyway: a formula
    // that cannot be evaluated must not become a formula that is not applied.
    let err = query(&catalog, &cfg, json!({}), &ada, None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("evaluator"), "{err}");
    Ok(())
}

#[tokio::test]
async fn a_configuration_the_catalog_contradicts_is_refused_on_save() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = setup(&db).await?;
    let registry = builtin_traits()?;
    let trait_ = registry.require("query_table")?;
    let check = async |cfg: Attrs| -> Result<()> {
        trait_
            .validate_config(&TraitCheck {
                catalog: &catalog,
                config: &cfg,
                agent: "librarian",
            })
            .await
    };

    check(books()).await?;
    // A table that is not there, and a field that is not on the table that is.
    let err = check(config(&[(CFG_TABLE, json!("shelves"))]))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("shelves"), "{err}");
    let err = check(config(&[
        (CFG_TABLE, json!("books")),
        (CFG_FIELDS, json!(["title", "athor"])),
    ]))
    .await
    .unwrap_err();
    assert!(err.to_string().contains("athor"), "{err}");
    // A table with no single-column primary key cannot be addressed by one.
    db.client()
        .await?
        .batch_execute("CREATE TABLE keyless (a bigint)")
        .await
        .map_err(|e| Error::database(e.to_string()))?;
    catalog.reload().await?;
    assert!(
        check(config(&[(CFG_TABLE, json!("keyless"))]))
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn a_trait_configured_against_a_dropped_table_leaves_its_agent_with_a_reason() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = setup(&db).await?;
    let registry = builtin_traits()?;
    let agent = Agent::new("librarian", "main")
        .with_trait(EnabledTrait::new("query_table").config(CFG_TABLE, "books"));
    save_agent(&catalog, &registry, &agent).await?;
    assert_eq!(Agents::load(&catalog, &registry).await?.all().len(), 1);

    db.client()
        .await?
        .batch_execute("DROP TABLE books")
        .await
        .map_err(|e| Error::database(e.to_string()))?;
    catalog.reload().await?;

    // Out of the live set, with the reason kept — and still stored, listed and
    // editable, because editing it is the repair.
    let agents = Agents::load(&catalog, &registry).await?;
    assert!(agents.all().is_empty());
    let issue = &agents.issues()[0];
    assert_eq!(issue.agent, "librarian");
    assert!(issue.problem.contains("books"), "{}", issue.problem);
    let err = agents.require("librarian").unwrap_err().to_string();
    assert!(err.contains("not usable"), "{err}");

    // The tool keeps the name the configuration gives it even now, so the
    // collision check and the admin UI still have something to say.
    let tools = registry
        .require("query_table")?
        .tools(&catalog, &agent.traits[0].config);
    assert_eq!(tools[0].name, "query_books");
    Ok(())
}

#[tokio::test]
async fn an_agent_answers_a_question_about_its_table_through_a_whole_run() -> Result<()> {
    // Phase 3's "done when", for the read half: an agent given `query_table` over
    // a real table answers a question about the data, with the whole exchange in
    // its run's context — and it sees only what the person chatting may see.
    let db = TestDb::new().await?;
    let catalog = setup(&db).await?;
    own_books(&catalog, "owner === user.email").await?;
    let registry = builtin_traits()?;
    let agent = Agent::new("librarian", "main")
        .system_prompt("You answer questions about the library.")
        .with_trait(
            EnabledTrait::new("query_table")
                .config(CFG_TABLE, "books")
                .config(CFG_FIELDS, json!(["id", "title", "pages"])),
        );
    save_agent(&catalog, &registry, &agent).await?;

    let provider = Arc::new(FakeProvider::new([
        Reply::calls(
            "query_books",
            json!({"order_by": "pages", "descending": true}),
        )
        .with_preamble("Let me look."),
        Reply::says("Your longest book is Ilium, at 576 pages."),
    ]));
    let caller = RunCaller::user(reader("ada@example.com"));
    let runner = Runner::new(&catalog, &registry, &agent, provider.clone(), caller);

    let (run, conclusion) = runner.start("what is my longest book?").await?;
    assert_eq!(
        conclusion.answer(),
        Some("Your longest book is Ilium, at 576 pages.")
    );

    // The tool was offered under its derived name, described from the table.
    let first = &provider.requests()[0];
    assert_eq!(first.tools.len(), 1);
    assert_eq!(first.tools[0].name, "query_books");
    assert!(first.tools[0].description.contains("pages"));

    // The result the model was given holds Ada's two books and neither of Bob's,
    // narrowed to the three allowed fields — the whole exchange, on the run.
    let state = sc_agent::load_run(&catalog, run.id)
        .await?
        .expect("the run row")
        .agent_loop()?;
    let result = state
        .messages()
        .iter()
        .find_map(|m| match m {
            sc_llm::LlmMessage::ToolResult { content, name, .. } if name == "query_books" => {
                Some(content.clone())
            }
            _ => None,
        })
        .expect("the tool result is in the transcript");
    let result: Json = serde_json::from_str(&result).unwrap();
    assert_eq!(titles(&result), ["Ilium", "Dune"]);
    assert!(!result["rows"][0].as_object().unwrap().contains_key("owner"));
    Ok(())
}
