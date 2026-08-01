#![allow(clippy::unwrap_used, clippy::expect_used)]

//! The coding traits against a **real local file store** (§11.3, TODO Phase 5).
//!
//! Real directories and real bytes, for the reason the other suites use a real
//! database: what these traits do *is* the meeting of paths and bytes, so a
//! stubbed store would confirm only that the seam was called. What is pinned
//! here is the behaviour a model depends on and an admin relies on:
//!
//! - a read/write/list round trip, with paths reported the way they may be sent
//!   back;
//! - `edit_file`'s three outcomes — the unique match applied, the absent one
//!   refused, the ambiguous one refused with the count;
//! - the configured sub-directory as a **confinement**: a path climbing out of it
//!   is refused even though the store itself would have allowed it;
//! - `search_files` finding a literal and a regular expression across
//!   directories, bounded, with the bound reported;
//! - §9's access rule applying to a *listing* and to a *search*, so a role that
//!   cannot open a directory cannot learn its contents through an agent either;
//! - `run_project_script` refusing a script `package.json` does not declare and
//!   running one it does;
//! - a trait configured against a store that is gone leaving its agent invalid
//!   **with a reason**.
//!
//! `build_application` has its own suite (`build_application.rs`), because it
//! needs an application row and a bundler to run.

mod common;

use common::{Env, as_user, config};
use sc_agent::RunCaller;
use sc_core_traits::{
    CFG_MAX_CHARS, CFG_MAX_RESULTS, CFG_ROOT, CFG_STORE, CFG_TIMEOUT, FileScope, tool_names,
};
use sc_error::Result;
use sc_files::FileMeta;
use serde_json::json;

/// The scope every test in this file is configured against.
fn scope(store: &str, root: &str) -> FileScope {
    FileScope {
        store: store.to_owned(),
        root: root.to_owned(),
    }
}

/// A store-and-root configuration.
fn at(store: &str, root: &str) -> sc_types::Attrs {
    config(&[(CFG_STORE, json!(store)), (CFG_ROOT, json!(root))])
}

/// The admin, who clears every rule — the caller a chat with an admin has.
fn admin() -> RunCaller {
    RunCaller::system()
}

#[tokio::test]
async fn a_file_written_by_the_agent_is_read_and_listed_back() -> Result<()> {
    let env = Env::new().await?;
    let dir = env.with_file_store("code", None).await?;
    env.put(&dir, "src/existing.ts", "export const a = 1;\n")?;

    let cfg = at("code", "");
    let written = env
        .call(
            "write_file",
            &cfg,
            json!({"path": "src/app.ts", "content": "export const app = 2;\n"}),
            &admin(),
        )
        .await?;
    assert_eq!(written["written"], json!(true));
    // The write went through the store, so it is on the disk the store roots at.
    assert_eq!(env.slurp(&dir, "src/app.ts")?, "export const app = 2;\n");

    let read = env
        .call("read_file", &cfg, json!({"path": "src/app.ts"}), &admin())
        .await?;
    assert_eq!(read["text"], json!("export const app = 2;\n"));
    assert_eq!(read["truncated"], json!(false));

    let listed = env
        .call("list_files", &cfg, json!({"dir": "src"}), &admin())
        .await?;
    let mut paths: Vec<&str> = listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    paths.sort();
    assert_eq!(paths, ["src/app.ts", "src/existing.ts"]);
    Ok(())
}

#[tokio::test]
async fn a_read_is_bounded_and_says_when_it_truncated() -> Result<()> {
    let env = Env::new().await?;
    let dir = env.with_file_store("code", None).await?;
    env.put(&dir, "big.txt", &"x".repeat(500))?;

    let cfg = config(&[
        (CFG_STORE, json!("code")),
        (CFG_ROOT, json!("")),
        (CFG_MAX_CHARS, json!(100)),
    ]);
    let read = env
        .call("read_file", &cfg, json!({"path": "big.txt"}), &admin())
        .await?;
    assert_eq!(read["text"].as_str().unwrap().len(), 100);
    assert_eq!(read["truncated"], json!(true));
    assert_eq!(read["bytes"], json!(500));

    // The model may ask for less, and cannot ask for more than the ceiling.
    let read = env
        .call(
            "read_file",
            &cfg,
            json!({"path": "big.txt", "max_chars": 5}),
            &admin(),
        )
        .await?;
    assert_eq!(read["text"], json!("xxxxx"));
    let read = env
        .call(
            "read_file",
            &cfg,
            json!({"path": "big.txt", "max_chars": 5000}),
            &admin(),
        )
        .await?;
    assert_eq!(read["text"].as_str().unwrap().len(), 100);
    Ok(())
}

#[tokio::test]
async fn an_edit_applies_a_unique_match_and_refuses_the_other_two_cases() -> Result<()> {
    let env = Env::new().await?;
    let dir = env.with_file_store("code", None).await?;
    env.put(
        &dir,
        "src/app.ts",
        "const a = 1;\nconst b = 2;\nconst a = 3;\n",
    )?;
    let cfg = at("code", "");

    // Unique: applied, and the file on disk has changed.
    let edited = env
        .call(
            "edit_file",
            &cfg,
            json!({"path": "src/app.ts", "find": "const b = 2;", "replace": "const b = 20;"}),
            &admin(),
        )
        .await?;
    assert_eq!(edited["replacements"], json!(1));
    assert_eq!(
        env.slurp(&dir, "src/app.ts")?,
        "const a = 1;\nconst b = 20;\nconst a = 3;\n"
    );

    // Absent: refused, and the file is untouched.
    let err = env
        .call(
            "edit_file",
            &cfg,
            json!({"path": "src/app.ts", "find": "const zz = 9;", "replace": "x"}),
            &admin(),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("does not appear"), "{err}");

    // Ambiguous: refused, with the count and the way out — and, again, nothing
    // written. A fuzzy edit here would be a corrupted file nobody noticed.
    let err = env
        .call(
            "edit_file",
            &cfg,
            json!({"path": "src/app.ts", "find": "const a", "replace": "let a"}),
            &admin(),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("2 times"), "{err}");
    assert_eq!(
        env.slurp(&dir, "src/app.ts")?,
        "const a = 1;\nconst b = 20;\nconst a = 3;\n"
    );
    Ok(())
}

#[tokio::test]
async fn a_path_that_escapes_the_configured_sub_directory_is_refused() -> Result<()> {
    let env = Env::new().await?;
    let dir = env.with_file_store("code", None).await?;
    env.put(&dir, "web/app.ts", "inside\n")?;
    env.put(&dir, "secrets.txt", "outside\n")?;

    // The agent is confined to `web`, so `web/app.ts` is `app.ts` to it…
    let cfg = at("code", "web");
    let read = env
        .call("read_file", &cfg, json!({"path": "app.ts"}), &admin())
        .await?;
    assert_eq!(read["text"], json!("inside\n"));

    // …and the file one level up is not reachable, by any spelling. The store
    // itself would have served it: this is the configured root refusing.
    for path in ["../secrets.txt", "web/../secrets.txt", "/../secrets.txt"] {
        let err = env
            .call("read_file", &cfg, json!({"path": path}), &admin())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("outside"), "{path}: {err}");
    }
    // A write cannot climb out either.
    let err = env
        .call(
            "write_file",
            &cfg,
            json!({"path": "../planted.ts", "content": "no"}),
            &admin(),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("outside"), "{err}");
    assert!(!dir.join("planted.ts").exists());
    Ok(())
}

#[tokio::test]
async fn a_search_finds_a_literal_and_a_regex_across_directories() -> Result<()> {
    let env = Env::new().await?;
    let dir = env.with_file_store("code", None).await?;
    env.put(&dir, "src/app.ts", "export function todo() {}\n")?;
    env.put(&dir, "src/deep/list.tsx", "// TODO: paginate\nconst n = 1;\n")?;
    env.put(&dir, "readme.md", "nothing here\n")?;
    // Not descended into, so a store with a dependency tree in it is still
    // searchable.
    env.put(&dir, "node_modules/pkg/index.js", "todo\n")?;

    let cfg = at("code", "");
    let found = env
        .call("search_files", &cfg, json!({"pattern": "todo"}), &admin())
        .await?;
    let mut paths: Vec<&str> = found["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["path"].as_str().unwrap())
        .collect();
    paths.sort();
    assert_eq!(paths, ["src/app.ts", "src/deep/list.tsx"]);
    assert_eq!(found["more_matches_available"], json!(false));

    // A regular expression, narrowed by a glob to one kind of file.
    let found = env
        .call(
            "search_files",
            &cfg,
            json!({"pattern": r"function\s+\w+", "regex": true, "glob": "*.ts"}),
            &admin(),
        )
        .await?;
    assert_eq!(found["count"], json!(1));
    assert_eq!(found["matches"][0]["path"], json!("src/app.ts"));
    assert_eq!(found["matches"][0]["line"], json!(1));

    // Case matters when asked for.
    let found = env
        .call(
            "search_files",
            &cfg,
            json!({"pattern": "TODO", "case_sensitive": true}),
            &admin(),
        )
        .await?;
    assert_eq!(found["count"], json!(1));
    assert_eq!(found["matches"][0]["path"], json!("src/deep/list.tsx"));
    Ok(())
}

#[tokio::test]
async fn a_search_respects_its_bound_and_reports_that_it_did() -> Result<()> {
    let env = Env::new().await?;
    let dir = env.with_file_store("code", None).await?;
    for n in 0..10 {
        env.put(&dir, &format!("f{n}.ts"), "needle\nneedle\n")?;
    }
    let cfg = config(&[
        (CFG_STORE, json!("code")),
        (CFG_ROOT, json!("")),
        (CFG_MAX_RESULTS, json!(5)),
    ]);
    let found = env
        .call("search_files", &cfg, json!({"pattern": "needle"}), &admin())
        .await?;
    assert_eq!(found["count"], json!(5));
    // The whole point of the flag: a caller told "5 matches" and not told there
    // were more would report a complete answer that is not one.
    assert_eq!(found["more_matches_available"], json!(true));
    Ok(())
}

#[tokio::test]
async fn a_directory_the_caller_may_not_open_is_neither_listed_nor_searched() -> Result<()> {
    let env = Env::new().await?;
    let dir = env.with_file_store("code", None).await?;
    env.put(&dir, "public/open.ts", "const secret = 1;\n")?;
    env.put(&dir, "private/closed.ts", "const secret = 2;\n")?;

    // Admin-only on the directory, which the whole path below it inherits (§9).
    let store = env.catalog.require_file_store("code")?;
    store
        .set_meta(
            "private",
            &FileMeta {
                min_role: Some(1),
                ..FileMeta::default()
            },
        )
        .await?;

    let cfg = at("code", "");
    let reader = as_user("ada@example.com");

    let listed = env.call("list_files", &cfg, json!({}), &reader).await?;
    let names: Vec<&str> = listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["public"]);

    // The same rule on the search: a match inside a directory this caller cannot
    // open would leak, one line at a time, exactly what the rule was set to hide.
    let found = env
        .call("search_files", &cfg, json!({"pattern": "secret"}), &reader)
        .await?;
    let paths: Vec<&str> = found["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["public/open.ts"]);

    // The admin, who clears every rule, sees both.
    let found = env
        .call("search_files", &cfg, json!({"pattern": "secret"}), &admin())
        .await?;
    assert_eq!(found["count"], json!(2));

    // And reading the file directly is refused rather than silently empty.
    let err = env
        .call(
            "read_file",
            &cfg,
            json!({"path": "private/closed.ts"}),
            &reader,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("not permitted"), "{err}");
    Ok(())
}

#[tokio::test]
async fn only_a_script_the_project_declares_can_be_run() -> Result<()> {
    let env = Env::new().await?;
    let dir = env.with_file_store("code", None).await?;
    env.put(
        &dir,
        "web/package.json",
        r#"{"name":"todo","scripts":{"greet":"echo hello-from-the-project"}}"#,
    )?;
    let cfg = config(&[
        (CFG_STORE, json!("code")),
        (CFG_ROOT, json!("web")),
        (CFG_TIMEOUT, json!(120)),
    ]);

    // A script the project does not declare is refused, and the refusal names
    // the ones it does — which is what makes the mistake recoverable.
    let err = env
        .call(
            "run_project_script",
            &cfg,
            json!({"script": "rm-rf-everything"}),
            &admin(),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("not a script this project declares"), "{err}");
    assert!(err.contains("greet"), "{err}");

    // There is no shell: arguments of the model's own are not part of the tool.
    let err = env
        .call(
            "run_project_script",
            &cfg,
            json!({"script": "greet", "args": ["--force"]}),
            &admin(),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("args"), "{err}");

    // And one it does declare runs, with its output captured.
    if which_npm() {
        let ran = env
            .call(
                "run_project_script",
                &cfg,
                json!({"script": "greet"}),
                &admin(),
            )
            .await?;
        assert_eq!(ran["succeeded"], json!(true), "{ran}");
        assert_eq!(ran["timed_out"], json!(false));
        assert!(
            ran["stdout"]
                .as_str()
                .unwrap()
                .contains("hello-from-the-project"),
            "{ran}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_trait_configured_against_a_store_that_is_gone_is_invalid_with_a_reason() -> Result<()> {
    let env = Env::new().await?;
    env.with_file_store("code", None).await?;

    // The configured store: valid, and its tool is named after it.
    let cfg = at("code", "web");
    env.check("read_file", &cfg).await?;
    assert_eq!(
        env.tools("read_file", &cfg)[0].name,
        tool_names::read_file(&scope("code", "web"))
    );

    // One that never existed: refused on save *and* on load, naming it.
    let err = env
        .check("read_file", &at("gone", ""))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("gone"), "{err}");

    // A tool name longer than a provider accepts is the other save-time refusal:
    // discovered here, where the admin can shorten the sub-directory, rather
    // than by the vendor in the middle of a conversation.
    let err = env
        .check("read_file", &at("code", &"a/".repeat(40)))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("64"), "{err}");
    Ok(())
}

#[tokio::test]
async fn every_coding_trait_names_its_tool_after_its_scope() -> Result<()> {
    let env = Env::new().await?;
    env.with_file_store("app-src", None).await?;
    let cfg = at("app-src", "web");
    let web = scope("app-src", "web");

    // The names the model chooses between, and the names the collision check
    // (§11.2) compares: one trait enabled twice over two scopes is two tools.
    let named = |trait_: &str| env.tools(trait_, &cfg)[0].name.clone();
    assert_eq!(named("read_file"), tool_names::read_file(&web));
    assert_eq!(named("write_file"), tool_names::write_file(&web));
    assert_eq!(named("list_files"), tool_names::list_files(&web));
    assert_eq!(named("edit_file"), tool_names::edit_file(&web));
    assert_eq!(named("search_files"), tool_names::search_files(&web));
    assert_eq!(
        named("run_project_script"),
        tool_names::run_project_script(&web)
    );
    assert_eq!(named("read_file"), "read_file_app_src_web");

    // Each is callable under the name its own configuration derived — the
    // property that makes two instances distinguishable rather than one of them
    // unreachable.
    let dir = env.with_file_store("other", None).await?;
    env.put(&dir, "a.txt", "hello\n")?;
    let other = at("other", "");
    let read = env
        .call_tool(
            "read_file",
            &other,
            &tool_names::read_file(&scope("other", "")),
            json!({"path": "a.txt"}),
            &admin(),
        )
        .await?;
    assert_eq!(read["text"], json!("hello\n"));
    Ok(())
}

#[tokio::test]
async fn an_argument_no_tool_takes_is_refused_by_name() -> Result<()> {
    let env = Env::new().await?;
    let dir = env.with_file_store("code", None).await?;
    env.put(&dir, "a.txt", "hello\n")?;
    let cfg = at("code", "");

    let err = env
        .call(
            "read_file",
            &cfg,
            json!({"path": "a.txt", "encoding": "utf16"}),
            &admin(),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("encoding"), "{err}");

    // A missing required argument is named too, rather than defaulted.
    let err = env
        .call("read_file", &cfg, json!({}), &admin())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("path"), "{err}");
    Ok(())
}

/// Whether `npm` is on this machine's PATH.
///
/// The script-running assertion is skipped where it is not, rather than failing:
/// the refusal half of that test is the part with the judgement in it, and it
/// runs everywhere.
fn which_npm() -> bool {
    std::process::Command::new("npm")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
