//! `run_js_code` — run a JavaScript code body against the event.

use std::collections::BTreeMap;

use sc_error::{Error, Result};
use sc_expr::CodeCall;
use sc_types::{BasicType, FormField};
use serde_json::Value as Json;

use sc_action::{Action, ActionContext, ConfigCheck, Event, config_str};

/// The code body.
const CFG_CODE: &str = "code";

/// Run a configured JavaScript body in the server's sandboxed engine, and return
/// what it returns.
///
/// ## What is in scope
///
/// `row`, `old`, `user` and `payload` — the same rule the formula scope follows
/// (decision 7), so a trigger's `only_if` and its code see the same world: `row`
/// and `old` exist exactly where the event has rows, so naming `row` in a `login`
/// trigger's code is a `ReferenceError` rather than a silent `undefined`; `old` on
/// an insert is in scope *and null*; `user` is the caller's fields or `null`.
/// `payload` is the addition: it is what a directly-run or scheduled trigger was
/// called with, the formula language has no way to reach it, and a `none`
/// trigger's code is exactly what wants it.
///
/// The escape hatch for the thing an elementary action cannot anticipate: a
/// computation over the event that no combination of `insert_row`/`fetch` and
/// formulas expresses. It is **bounded on purpose** — there is no host API, so
/// the code cannot read or write the catalog, reach the network, or touch the
/// disk. Catalog access from a guest language is `sc-code`'s milestone (§15) and
/// this is its seed, not a preview of it.
///
/// Three consequences of running on the *same* isolate as every ownership formula
/// (§7.3), all deliberate:
///
/// - the per-run **timeout is the engine's**, not this action's. A configurable
///   one would be a configurable hold on every formula evaluation in the process,
///   since the isolate serves them serially;
/// - the code is **synchronous**. Nothing in the sandbox is awaitable, so a body
///   that returns a Promise is refused rather than stringified to `{}`;
/// - a **syntax error surfaces at fire time**, not on save. Checking it would
///   mean compiling in the engine, which the save path has no access to — an
///   admin tests a body with the Run button, as they would with any code.
pub struct RunJsCode;

#[async_trait::async_trait]
impl Action for RunJsCode {
    fn name(&self) -> &str {
        "run_js_code"
    }

    fn description(&self) -> &str {
        "Run JavaScript against the event and return its result"
    }

    fn config_spec(&self) -> Vec<FormField> {
        vec![
            FormField::new(CFG_CODE, BasicType::Text)
                .label("Code")
                .required(),
        ]
    }

    async fn validate_config(&self, check: &ConfigCheck<'_>) -> Result<()> {
        // The same reader `run` uses, for the one thing that *can* be checked
        // without an engine: that there is a body at all. A blank one passes the
        // generic spec check (it is a string) and would then fire doing nothing,
        // which is the silent failure principle 5 refuses.
        config_str(check.config, CFG_CODE)?;
        Ok(())
    }

    async fn run(&self, ctx: &mut ActionContext<'_>) -> Result<Json> {
        let code = config_str(ctx.config, CFG_CODE)
            .map_err(|e| Error::invalid(format!("trigger `{}`: {e}", ctx.trigger)))?;
        let call = CodeCall {
            code,
            bindings: bindings(ctx.event),
            // The table handle arrives in phase 5 of this milestone; until then a
            // body is the pure one this action shipped with.
            ..CodeCall::default()
        };
        // The result is the action's result: a directly-run trigger returns it to
        // its caller, and a workflow step will put it in the run context.
        ctx.evaluator()?
            .run_code(call)
            .await
            .map_err(|e| Error::invalid(format!("trigger `{}`: `{CFG_CODE}`: {e}", ctx.trigger)))
    }
}

/// What the event binds in the code's scope (`row`, `old`, `user`, `payload`).
///
/// Presence is scope, exactly as it is for a formula: an absent binding is a
/// `ReferenceError` naming it, a binding present as `null` is a value. `old` on an
/// insert is the case that distinguishes the two — in scope, null.
fn bindings(event: &Event) -> BTreeMap<String, Json> {
    let mut bindings = BTreeMap::new();
    if event.kind.is_table_event() {
        bindings.insert("row".to_owned(), Json::Object(event.row_object()));
        bindings.insert(
            "old".to_owned(),
            match event.old_row {
                Some(_) => Json::Object(event.old_row_object()),
                None => Json::Null,
            },
        );
    }
    bindings.insert("user".to_owned(), event.user.clone().unwrap_or(Json::Null));
    bindings.insert("payload".to_owned(), event.payload.clone());
    bindings
}

#[cfg(test)]
mod tests {
    use super::*;
    use sc_action::EventKind;
    use sc_types::Attrs;
    use serde_json::json;

    #[test]
    fn the_code_is_the_only_setting_and_it_is_required() {
        let spec = RunJsCode.config_spec();
        let names: Vec<&str> = spec.iter().map(|f| f.name()).collect();
        assert_eq!(names, vec![CFG_CODE]);
        assert!(spec[0].required);
        // A configuration with nothing in it is a named error rather than an
        // empty body that silently returns null.
        let msg = config_str(&Attrs::new(), CFG_CODE).unwrap_err().to_string();
        assert!(msg.contains(CFG_CODE) && msg.contains("required"), "{msg}");
    }

    #[test]
    fn presence_is_scope_for_the_events_bindings() {
        // An update binds both rows; every name in the scope is there.
        let update = Event::new(EventKind::Update)
            .on("books")
            .row(json!({ "id": 1, "title": "now" }))
            .old_row(json!({ "id": 1, "title": "was" }))
            .caller(1, Some(json!({ "email": "a@b.c" })));
        let bound = bindings(&update);
        assert_eq!(
            bound.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["old", "payload", "row", "user"]
        );
        assert_eq!(bound["old"]["title"], json!("was"));
        assert_eq!(bound["user"]["email"], json!("a@b.c"));
        assert_eq!(bound["payload"], Json::Null);

        // An insert has `old` in scope and null — a value, not an absence.
        let insert = Event::new(EventKind::Insert)
            .on("books")
            .row(json!({ "id": 1 }));
        let bound = bindings(&insert);
        assert_eq!(bound["old"], Json::Null);
        assert_eq!(bound["user"], Json::Null, "anonymous binds null");

        // An event with no row binds neither, so code naming `row` there fails
        // in the engine instead of reading undefined.
        let called = Event::new(EventKind::None).payload(json!({ "n": 2 }));
        let bound = bindings(&called);
        assert!(!bound.contains_key("row") && !bound.contains_key("old"));
        assert_eq!(bound["payload"], json!({ "n": 2 }));
    }
}
