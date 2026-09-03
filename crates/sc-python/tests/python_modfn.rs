//! `modfn` from a Python body (phase 3.4 and 3.5).
//!
//! No database and no module worker: what a real [`ModuleFnHost`] adds over the
//! one here is a `deno` pool, and the pool is `sc-module`'s to test. What is
//! under test is the surface — both spellings, the resolution of a short name
//! against the list this run was given, and what happens when that resolution
//! has no answer or two.
//!
//! The list is the **run's**, read off its own thread. That matters because one
//! compiled body serves every run and installing or configuring a module reloads
//! the set between two of them, so a body that resolved a name once must not go
//! on resolving it that way.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use sc_error::{Error, Result};
use sc_expr::{CodeCall, ModuleFnHost, ModuleFunction};
use sc_python::PythonRuntime;
use serde_json::{Value as Json, json};

/// A host that supplies whatever functions a test names, and answers a call by
/// echoing what it was asked — so the plan a body built can be read back out.
struct Modules {
    functions: Vec<ModuleFunction>,
    calls: Mutex<Vec<Json>>,
    fail: Option<String>,
}

impl Modules {
    /// `(module, name)` pairs, as the set a run is given.
    fn supplying(pairs: &[(&str, &str)]) -> Arc<Modules> {
        Arc::new(Modules {
            functions: pairs
                .iter()
                .map(|(module, name)| ModuleFunction {
                    module: (*module).to_owned(),
                    name: (*name).to_owned(),
                    description: format!("what {name} does"),
                    is_async: false,
                    arguments: Vec::new(),
                })
                .collect(),
            calls: Mutex::new(Vec::new()),
            fail: None,
        })
    }

    /// The same, for a module function that raises.
    fn failing(pairs: &[(&str, &str)], with: &str) -> Arc<Modules> {
        let mut host = Modules::supplying(pairs);
        Arc::get_mut(&mut host).expect("sole owner").fail = Some(with.to_owned());
        host
    }

    fn calls(&self) -> Vec<Json> {
        self.calls.lock().expect("not poisoned").clone()
    }
}

#[async_trait]
impl ModuleFnHost for Modules {
    async fn call(&self, request: Json) -> Result<Json> {
        self.calls
            .lock()
            .expect("not poisoned")
            .push(request.clone());
        if let Some(why) = &self.fail {
            return Err(Error::invalid(why.clone()));
        }
        Ok(json!({
            "module": request.get("module"),
            "function": request.get("function"),
            "args": request.get("args"),
        }))
    }

    fn functions(&self) -> Vec<ModuleFunction> {
        self.functions.clone()
    }
}

async fn run(host: &Arc<Modules>, code: &str) -> Result<Json> {
    PythonRuntime::new()
        .run(CodeCall {
            code: code.to_owned(),
            module_fns: Some(host.as_ref() as &dyn ModuleFnHost),
            ..CodeCall::default()
        })
        .await
}

async fn fails(host: &Arc<Modules>, code: &str) -> String {
    run(host, code)
        .await
        .expect_err("this body was supposed to fail")
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn both_spellings_reach_the_same_function_with_the_same_plan() {
    let host = Modules::supplying(&[("@saltcorn/markdown", "md_to_html")]);
    let out = run(
        &host,
        r##"
short = modfn.md_to_html("# hi")
long = modfn("@saltcorn/markdown").md_to_html("# hi")
return {"short": short, "long": long}
"##,
    )
    .await
    .expect("both spellings resolve");

    // The short form is what an author writes; the long one is what they write
    // when two modules supply the name. They are the same call.
    assert_eq!(out["short"], out["long"]);
    let calls = host.calls();
    assert_eq!(calls.len(), 2);
    // The same plan from either spelling, but for the clock the bridge fills in
    // — which is what is left of *this* run and so is a millisecond smaller the
    // second time.
    let named = |plan: &Json| {
        json!({ "module": plan["module"], "function": plan["function"],
                                      "args": plan["args"] })
    };
    assert_eq!(
        named(&calls[0]),
        json!({
            "module": "@saltcorn/markdown",
            "function": "md_to_html",
            "args": ["# hi"],
        })
    );
    assert_eq!(named(&calls[0]), named(&calls[1]));
    // And the clock travels with it, less the margin that keeps a module which
    // hangs catchable inside the body rather than fatal to the run.
    for plan in &calls {
        let ms = plan["timeout_ms"].as_u64().unwrap_or(0);
        assert!(ms > 0 && ms < 5000, "{plan}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_is_synchronous_and_its_arguments_are_json() {
    // Synchronous even for a function v1 itself made `async`: everything in this
    // surface is, and the wait is a host call with the GIL released like every
    // other one. And the arguments are positional, as v1's signatures are.
    let host = Modules::supplying(&[("@saltcorn/geo", "geocode")]);
    let out = run(
        &host,
        r#"
answer = modfn.geocode({"city": "Oslo"}, 2, True, None)
return {"args": answer["args"], "kind": type(answer).__name__}
"#,
    )
    .await
    .expect("a call answers a value, not a future");
    assert_eq!(out["args"], json!([{ "city": "Oslo" }, 2, true, null]));
    assert_eq!(out["kind"], json!("dict"));

    // A keyword is refused naming the function, rather than dropped on the way.
    let said = fails(&host, r#"return modfn.geocode(city="Oslo")"#).await;
    assert!(said.contains("positional arguments"), "{said}");
    assert!(said.contains("geocode"), "{said}");

    // An argument with no JSON form is refused naming where it is, rather than
    // arriving at the module as something else.
    let said = fails(&host, "return modfn.geocode({1, 2})").await;
    assert!(said.contains("no JSON form"), "{said}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ambiguous_short_name_names_both_modules_and_the_spelling_that_works() {
    let host = Modules::supplying(&[
        ("@saltcorn/nominatim", "geocode_lat"),
        ("@acme/geo", "geocode_lat"),
    ]);
    // Not resolved by luck: choosing the module that happened to load first is a
    // wrong answer inside somebody's trigger.
    let said = fails(&host, "return modfn.geocode_lat('Oslo')").await;
    assert!(said.contains("@saltcorn/nominatim"), "{said}");
    assert!(said.contains("@acme/geo"), "{said}");
    assert!(said.contains("modfn(\"@saltcorn/nominatim\")"), "{said}");
    assert!(host.calls().is_empty(), "nothing was called");

    // And the qualified form always works.
    let out = run(
        &host,
        "return modfn('@acme/geo').geocode_lat('Oslo')['module']",
    )
    .await
    .expect("the long form is unambiguous");
    assert_eq!(out, json!("@acme/geo"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_name_nothing_supplies_says_what_this_server_does_have() {
    let host = Modules::supplying(&[("@saltcorn/markdown", "md_to_html")]);

    // An `AttributeError`, which is Python's own answer to an attribute that is
    // not there — so `getattr(modfn, name, None)` still works and a body may ask
    // before it calls.
    let out = run(
        &host,
        r#"
said = {}
try:
    modfn.md_to_htm
except AttributeError as e:
    said["typo"] = str(e)
said["asked"] = getattr(modfn, "not_here", None) is None
said["found"] = getattr(modfn, "md_to_html", None) is not None
try:
    modfn("@saltcorn/markdwn")
except Exception as e:
    said["module"] = str(e)
said["functions"] = [f["name"] for f in modfn.functions]
return said
"#,
    )
    .await
    .expect("asking is not failing");

    let typo = out["typo"].as_str().unwrap_or_default();
    assert!(
        typo.contains("no module function named `md_to_htm`"),
        "{typo}"
    );
    assert!(typo.contains("md_to_html"), "and what there is: {typo}");
    assert_eq!(out["asked"], json!(true));
    assert_eq!(out["found"], json!(true));
    let module = out["module"].as_str().unwrap_or_default();
    assert!(module.contains("@saltcorn/markdwn"), "{module}");
    assert!(module.contains("@saltcorn/markdown"), "{module}");
    assert_eq!(out["functions"], json!(["md_to_html"]));
    assert!(host.calls().is_empty(), "and nothing reached the host");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_module_that_fails_is_a_catchable_module_error() {
    let host = Modules::failing(&[("@saltcorn/markdown", "md_to_html")], "the worker died");
    let out = run(
        &host,
        r#"
import saltcorn as sc
try:
    modfn.md_to_html("x")
    return "answered"
except sc.ModuleError as e:
    return {"caught": str(e), "is_saltcorn": isinstance(e, sc.SaltcornError)}
"#,
    )
    .await
    .expect("a failing module is not a failing body");
    assert!(
        out["caught"].as_str().unwrap().contains("the worker died"),
        "{out}"
    );
    assert_eq!(out["is_saltcorn"], json!(true));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_body_with_no_modules_has_no_name_for_modfn() {
    // Presence is scope, as for the other four.
    let error = PythonRuntime::new()
        .run(CodeCall {
            code: "return modfn.md_to_html(\"x\")".to_owned(),
            ..CodeCall::default()
        })
        .await
        .expect_err("a pure body calls no module functions");
    let said = error.to_string();
    assert!(said.contains("NameError"), "{said}");
    assert!(said.contains("modfn"), "{said}");

    // A server that has the surface but no modules on it is a different
    // sentence, and it is the one an admin can act on.
    let empty = Modules::supplying(&[]);
    let said = fails(&empty, "return modfn.anything('x')").await;
    assert!(said.contains("no installed module supplies one"), "{said}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_module_budget_is_the_javascript_ones_and_names_what_it_is_for() {
    let host = Modules::supplying(&[("@saltcorn/markdown", "md_to_html")]);
    let out = run(
        &host,
        r#"
import saltcorn as sc
done = 0
try:
    while True:
        modfn.md_to_html("x")
        done += 1
except sc.ModuleError as e:
    return {"done": done, "said": str(e)}
return {"done": done, "said": None}
"#,
    )
    .await
    .expect("the budget is a refusal, not a crash");
    assert_eq!(
        out["done"].as_u64(),
        Some(u64::from(sc_expr::DEFAULT_MAX_MODULE_CALLS)),
        "{out}"
    );
    let said = out["said"].as_str().unwrap_or_default();
    assert!(said.contains("module functions in one run"), "{said}");
    assert!(
        said.contains("a call per row"),
        "the bound says what it is for: {said}"
    );
}
