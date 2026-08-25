//! `run_js_code` — run a JavaScript code body against the event.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use sc_api::code_host::{FileStoreHost, TableHost, TriggerRunHost};
use sc_error::{Error, Result};
use sc_expr::{CodeCall, DEFAULT_CODE_TIMEOUT, MAX_CODE_TIMEOUT, TriggerHost};
use sc_types::{Attrs, BasicType, FormField};
use serde_json::Value as Json;

use sc_action::{Action, ActionContext, ConfigCheck, Event, config_str};

use crate::code_fetch::CodeFetchHost;

/// The code body.
const CFG_CODE: &str = "code";
/// How long one run may take.
const CFG_TIMEOUT: &str = "timeout_ms";

/// What a trigger that names no timeout gets — the engine's own default, said in
/// the unit the form takes.
const DEFAULT_TIMEOUT_MS: u64 = DEFAULT_CODE_TIMEOUT.as_millis() as u64;

/// The ceiling on a configured timeout.
///
/// Bounded for the reason `fetch`'s is: a trigger runs inside the request or the
/// write that fired it, so an unbounded body is an unbounded hold on that caller.
/// A body now reads and writes tables, so it can genuinely need more than the
/// default — and still not a minute.
const MAX_TIMEOUT_MS: u64 = MAX_CODE_TIMEOUT.as_millis() as u64;

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
/// And five host surfaces: `db`, the **tables**, read and written from the body
/// (§10.1's `db`); `fetch`, an **HTTP request**; `fs`, the **file stores**;
/// `trigger`, this server's **other triggers**; and `modfn`, the **functions
/// this server's modules supply** (all below).
///
/// ```js
/// const html = await modfn.md_to_html(row.notes);
/// const lat = await modfn("@saltcorn/nominatim-geocode").geocode_lat({ city: row.city });
/// ```
///
/// A module function is v1's own, and it runs on the isolate its module was
/// loaded on — which is where the `markdown-it`, the geocoder and the module's
/// configuration are. Everything is awaited, including the ones that are
/// synchronous in v1, and `modfn` is bound only where this server has modules.
///
/// ```js
/// const overdue = await db.invoices
///   .where({ paid: false, due: { lt: payload.today } })
///   .select("id", "amount", "customerⱵemail", { chased: "remindersↃinvoice.length" })
///   .orderBy("due")
///   .limit(50)
///   .rows();
/// for (const inv of overdue) {
///   await db.reminders.insert({ invoice: inv.id, sent_to: inv.customerⱵemail });
/// }
/// return {
///   chased: overdue.length,
///   owed: await db.invoices.where({ paid: false }).sum("amount"),
/// };
/// ```
///
/// A chain is pure and a terminal executes, sending one plan to
/// [`sc_api::code_host::TableHost`] — which resolves every table, column, Ⱶ-path
/// and formula through the catalog, so nothing a chain produces reaches SQL as
/// text. Writes go through the row layer, which means they are coerced against
/// their columns, validated, and **observed by triggers**: a write from a code
/// body is an event like any other, carrying this trigger's chain, so the cascade
/// bound applies to it exactly as it does to `insert_row`.
///
/// ## The body's own SQL
///
/// For the question the chain does not ask — a window function, a recursive CTE,
/// an `ON CONFLICT` — there is `db.sql`:
///
/// ```js
/// const ranked = await db.sql(
///   "select owner, title, rank() over (partition by owner order by pages desc) as r \
///    from books where pages > $1",
///   [200],
/// );
/// const mine = await db.sql("select * from books", [], { asUser: true });  // or db.asUser().sql(…)
/// ```
///
/// The text is the trigger author's and runs as written; the values are
/// **binds** and never part of it. It is the same admission §13.4's custom SQL
/// queries are — a code body is server-side configuration written by an
/// administrator — and it carries the same consequences, which the third
/// argument is an object in order to keep saying: raw SQL does not go through
/// the row layer, so no ownership formula filters it, no rich type coerces it,
/// and **a write inside one raises no table event**. The row cap, the call
/// budget and the caller-context transaction (so an RLS table's policies still
/// decide) all still apply.
///
/// ## Calling an endpoint
///
/// `fetch` is the web's, with the web's rules — an author who already knows a
/// browser knows this one:
///
/// ```js
/// const res = await fetch("https://api.example.com/rates", {
///   headers: { authorization: `Bearer ${payload.token}` },
/// });
/// if (!res.ok) throw new Error(`rates: ${res.status}`);
/// const { usd } = await res.json();
/// await db.invoices.where({ id: row.id }).update({ rate: usd });
/// ```
///
/// A status the endpoint did not like is **not** an error: `res.ok` is false
/// and nothing throws, so the retry or the fallback is written in the body
/// rather than being a trigger that failed. Only a transport failure rejects,
/// with a `TypeError`, as a browser does. `Headers` and `Response` are there;
/// what is not is streaming (`res.body`), because the seam carries one value,
/// and `AbortSignal`, because there are no timers in the sandbox — the bound
/// that matters is already the run's own clock, which every request is clamped
/// to. One difference from the web is deliberate: an object body is sent as
/// JSON, because `[object Object]` on the wire is a bug every time it happens.
///
/// It is the same capability the [`Fetch`](crate::Fetch) action is, reached the
/// other way and bounded the same: absolute `http`/`https` only, 50 requests per
/// run, at most 8 MB of response, and each request clamped to what is left of
/// the body's `timeout_ms` — so a hung endpoint fails inside the body, where it
/// can be caught, rather than holding the trigger's caller.
///
/// ## Reading and writing files
///
/// `fs(name)` is a **file store**, `open` is a reference to a path in it — no
/// I/O, and the path need not exist — and everything that touches bytes is a
/// method on that reference:
///
/// ```js
/// const theFile = fs("myFileStore").open("the_file.txt");
/// if (await theFile.exists()) {
///   const rows = (await theFile.text()).split("\n");
///   await fs("myFileStore").open("reports/summary.json").write({ rows: rows.length });
/// }
/// ```
///
/// Creating is not a second concept: a reference that can be read can be
/// written, and the parent directories are made on the way. `write` replaces
/// what is there, `create` refuses to, and both take a string, bytes, a
/// `Response` (so `await file.write(await fetch(url))` saves a download),
/// another file (copied host-side, so the bytes never enter the sandbox), or any
/// other value — which is stored as JSON, for the reason `fetch`'s object body
/// is sent as JSON.
///
/// The reading vocabulary is a `Response`'s — `text()`, `json()`, `bytes()`,
/// `arrayBuffer()` — so an author who has read a fetch response has read a file.
/// The one departure from a `Blob` is that `size` and `type` are part of
/// `await file.stat()` rather than properties, because there is no synchronous
/// I/O in the sandbox and a property that lies is worse than an await. A missing
/// file is `await file.exists() === false`, and an error only to something that
/// was told to read it.
///
/// Directories are the same shape: `fs(s).dir("notes")` has `list()` — which
/// answers the same file and directory objects, so a listing is walked and acted
/// on rather than read and re-opened by name — `create()`, `exists()` and
/// `delete()`. A file also has `delete()`, `moveTo(dest)`, `copyTo(dest)` (where
/// `dest` may be a file in **another** store), and `meta()` / `setMeta()` over
/// the store's own per-file metadata — which is where §14.1's `min_role` rule is
/// set, and is reported as both what is set on the entry and what actually
/// applies given every directory above it.
///
/// Bounded like the other two: 100 file operations per run, 8 MB across the seam
/// per read and per write — **refused rather than truncated**, since a body
/// handed the first 8 MB of a larger file would compute a wrong answer out of a
/// right-looking one — and 256 MB per copy, which never crosses the seam at all.
/// Two things a body should not assume: nothing streams, and **a file write is
/// not rolled back** by a trigger that throws afterwards.
///
/// ## Running another trigger
///
/// `trigger(name)` is a handle over one of this server's triggers — no dispatch,
/// and `run` is the only verb:
///
/// ```js
/// const archived = await trigger("archive_done").run({ before: payload.today });
/// await trigger("reindex").run();                      // no payload is {}
/// await trigger("send_invoice").asUser().run({ id: row.id });
/// ```
///
/// It runs **the dispatcher's trigger, not a copy of it**: the same call the Run
/// button, `POST {mount}/actions/{name}` and the scheduler make, so the target's
/// `only_if` runs (and `null` comes back when it declines), a disabled trigger
/// stays disabled, one that failed validation says why, and the **cascade bound**
/// counts this run. That last is what makes the recursion safe rather than merely
/// unlikely: the child event carries this trigger's chain, so a body that runs
/// the trigger it is itself the action of stops at `MAX_DEPTH` with the whole
/// chain named. A failing trigger is an ordinary catchable error, so a body may
/// run one and fall back.
///
/// A handle rather than `db.books`'s property access, because a trigger's name is
/// the admin's own words for it; and the names travel into the run, so a typo is
/// refused where it is written, naming the triggers that do exist.
///
/// Bounded like the others: 20 trigger runs per body — the *width* of a cascade,
/// where the chain bounds its depth — and each one clamped to what is left of
/// this body's `timeout_ms`, so a slow child fails inside the body that started
/// it rather than holding the request that fired the outermost trigger.
///
/// ## Whose authority
///
/// Reads and writes — of tables and of files alike — are the **admin's** by
/// default, carrying the event's user —
/// a trigger is server-side configuration, and an audit row the caller may not
/// insert is the archetype of what a trigger exists to write. `db.asUser()`
/// delegates to the event's caller instead, and then §7.3's ownership rule
/// decides every row; a refusal is a catchable error, so a body may try a
/// delegated write and fall back. `fs(name).asUser()` is the same move for
/// files, where what decides is §14.1's path-cumulative rule instead — the
/// store's floor, then every directory on the path, then the entry itself, most
/// restrictive winning — and where a delegated body may **tighten** an access
/// rule with `setMeta` but never loosen one. `trigger(name).asUser()` is the
/// same move again, and there what decides is the target's own `min_role`: under
/// the trigger's own authority no floor is consulted, because running a trigger
/// from a trigger is configuration calling configuration. What does **not**
/// depend on the authority is who the child event says caused it — the event's
/// role and user travel either way, so the trigger that runs sees the same
/// `user` it would have seen had that caller run it directly. On a `db.sql` it
/// means less, and honestly so:
/// the statement runs at the caller's role and user, which is what row-level
/// security reads, and an ownership formula does not reach it.
///
/// The escape hatch for the thing an elementary action cannot anticipate: a
/// computation over the event, its tables and what an endpoint says that no
/// combination of `insert_row`/`fetch` and formulas expresses. It is still
/// bounded — no subprocess, no schema changes, no transactions across
/// statements, no path to a file except through a store an admin connected, and
/// nine named bounds (1000 rows per read, 200 database calls per run, 50
/// fetches per run, 100 file operations per run, 20 trigger runs per run, 8 MB
/// per response, 8 MB per file read or written, the `timeout_ms` wall clock, and
/// one second of JavaScript at a time without awaiting anything — a body shares
/// its isolate with every other body, and the sharing works because a body
/// awaiting a query leaves it free). A table larger than one read is walked with
/// `.iter()`, which yields the same rows a batch at a time — one database call
/// each, so what bounds it is the call budget rather than the row cap.
///
/// Three consequences of the runtime, all deliberate:
///
/// - a code body runs on the **code** isolate pool, not the formula isolate a
///   `only_if` or an ownership formula uses. Which is why the timeout here is
///   configurable at all: a bound on a pool nothing else depends on is not a
///   bound on every authorization decision in the process;
/// - the code is **asynchronous**: every terminal answers a promise, `.iter()` is
///   walked with `for await`, and `await` is legal at the top level of a body,
///   which is the inside of an `async function`. A `Promise.all([…])` of two
///   queries really does issue them together. The chain itself stays synchronous
///   — `await` goes at the front of a whole chain, never inside one — and a
///   forgotten `await` is a named error rather than `{}` in the result, because
///   the promise a terminal answers refuses to be stringified, coerced or
///   iterated. `db`, `fetch`, `fs` and `trigger` are the four awaitable things
///   there are — no timers, no second way to the network, no path to the disk
///   that is not a store an admin connected — and the one shape the
///   runtime refuses outright is a body that computes for a second without
///   yielding, because that is the isolate held against every other trigger;
/// - a **syntax error surfaces at fire time**, not on save. Checking it would
///   mean compiling in the engine, which the save path has no access to — an
///   admin tests a body with the Run button, as they would with any code.
pub struct RunJsCode {
    /// The HTTP client a body's `fetch` sends through, built at registration
    /// like the [`Fetch`](crate::Fetch) action's and for the same reason: it
    /// carries the connection pool and the TLS configuration, and building
    /// those per firing would pay for a handshake every time a trigger runs.
    client: reqwest::Client,
}

impl RunJsCode {
    /// Build the action and the client its bodies reach the network with.
    ///
    /// Fallible because constructing the client initialises the TLS stack: a
    /// deployment where that fails should say so at boot rather than at the
    /// first body that calls an endpoint.
    pub fn new() -> Result<RunJsCode> {
        Ok(RunJsCode {
            client: crate::fetch::http_client()?,
        })
    }
}

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
                // Declared as JavaScript so the admin UI gives it an editor with
                // highlighting and completions over the scope below, rather than
                // a text area. The declaration is the whole coupling: no screen
                // knows this setting by name.
                .code("javascript")
                .required(),
            FormField::new(CFG_TIMEOUT, BasicType::Int)
                .label("Timeout (ms)")
                .default_value(DEFAULT_TIMEOUT_MS),
        ]
    }

    async fn validate_config(&self, check: &ConfigCheck<'_>) -> Result<()> {
        // The same readers `run` uses, for the two things that *can* be checked
        // without an engine: that there is a body at all — a blank one passes the
        // generic spec check (it is a string) and would then fire doing nothing,
        // which is the silent failure principle 5 refuses — and that the timeout
        // is a number in range, which is a message on the form rather than a
        // firing that will not start.
        config_str(check.config, CFG_CODE)?;
        timeout(check.config)?;
        Ok(())
    }

    async fn run(&self, ctx: &mut ActionContext<'_>) -> Result<Json> {
        let code = config_str(ctx.config, CFG_CODE)
            .map_err(|e| Error::invalid(format!("trigger `{}`: {e}", ctx.trigger)))?;
        let timeout = timeout(ctx.config)
            .map_err(|e| Error::invalid(format!("trigger `{}`: {e}", ctx.trigger)))?;
        let evaluator = ctx.evaluator()?;
        // The `db` handle, for this run only: the call budget is counted on it,
        // and the event's caller and this trigger's chain ride on every statement
        // it makes — so a write from the body is an event that says who caused it
        // and how deep in a cascade it already is. The engine goes along because a
        // delegated row check is a formula (§7.3), evaluated on the *formula*
        // isolate while this body's own thread waits — which is why the two
        // runtimes are separate.
        let host = TableHost::new(ctx.catalog)
            .caused_by(ctx.event.role, ctx.event.user.clone())
            .chained(ctx.chain.clone())
            .with_evaluator(Some(Arc::clone(evaluator)));
        // The network, on the same terms: borrowed for this run, bounded by the
        // run's own clock, and counted on a budget of its own.
        let net = CodeFetchHost::new(self.client.clone(), ctx.trigger);
        // The file stores, on the same terms again. It carries the event's role
        // rather than its user, because a file rule is a role floor: `asUser()`
        // is checked against it, and the admin default clears every one.
        let files = FileStoreHost::new(ctx.catalog).caused_by(ctx.event.role);
        // The other triggers, when this context has a dispatcher — and nothing
        // at all when it does not, so a body that names `trigger` outside a
        // server says so by name. It carries the caller *and* the chain: the
        // caller because the trigger that runs must see who caused it, and the
        // chain because that is what stops a body running the trigger it is
        // itself the action of, at the same depth every other cascade stops at.
        let runner = ctx.triggers().map(|d| {
            TriggerRunHost::new(d, ctx.catalog)
                .caused_by(ctx.event.role, ctx.event.user.clone())
                .chained(ctx.chain.clone())
        });
        // The module functions, when this server has modules installed and
        // loaded — and nothing at all when it does not, so a body that names
        // `modfn` on a server with no modules says so by name rather than
        // finding an empty handle. The catalog is where they are because a
        // formula's hoisted call needs them too, from three other crates.
        let module_fns = ctx.catalog.module_functions();
        let call = CodeCall {
            code,
            bindings: bindings(ctx.event, ctx.run_context()),
            host: Some(&host),
            fetch: Some(&net),
            files: Some(&files),
            triggers: runner.as_ref().map(|r| r as &dyn TriggerHost),
            module_fns: module_fns.as_deref(),
            timeout,
            ..CodeCall::default()
        };
        // The result is the action's result: a directly-run trigger returns it to
        // its caller, and a workflow step will put it in the run context.
        evaluator
            .run_code(call)
            .await
            .map_err(|e| Error::invalid(format!("trigger `{}`: `{CFG_CODE}`: {e}", ctx.trigger)))
    }
}

/// The configured wall clock for one run, **bounded** ([`MAX_TIMEOUT_MS`]), or
/// `None` when the trigger names none.
///
/// `None` rather than [`DEFAULT_TIMEOUT_MS`] resolved here, because the engine
/// has that default already and a process may have been started with another one:
/// an admin who left the field alone said "whatever this server's default is",
/// and turning that into a number would overrule it.
///
/// Out of range is refused rather than clamped, exactly as `fetch`'s is: an admin
/// who typed five minutes should be told it is not allowed, not quietly given one.
fn timeout(config: &Attrs) -> Result<Option<Duration>> {
    let ms = match config.get(CFG_TIMEOUT) {
        None | Some(Json::Null) => return Ok(None),
        Some(Json::Number(n)) => n.as_i64().ok_or_else(|| {
            Error::invalid(format!(
                "`{CFG_TIMEOUT}` must be a whole number of milliseconds"
            ))
        })?,
        Some(other) => {
            return Err(Error::invalid(format!(
                "`{CFG_TIMEOUT}` must be a whole number of milliseconds, got {other}"
            )));
        }
    };
    if !(1..=MAX_TIMEOUT_MS as i64).contains(&ms) {
        return Err(Error::invalid(format!(
            "`{CFG_TIMEOUT}` must be between 1 and {MAX_TIMEOUT_MS} milliseconds, got {ms}"
        )));
    }
    Ok(Some(Duration::from_millis(ms.unsigned_abs())))
}

/// What the event binds in the code's scope (`row`, `old`, `user`, `payload`) —
/// plus `context` when this body is a **workflow step** (§10.3, decision 8): the
/// run so far, as an object, which is the same thing a step's formulas read.
///
/// Presence is scope, exactly as it is for a formula: an absent binding is a
/// `ReferenceError` naming it, a binding present as `null` is a value. `old` on an
/// insert is the case that distinguishes the two — in scope, null. `context` is
/// the other: outside a run it is not bound at all, so a body that names it says
/// so rather than reading an empty object as "nothing has happened yet".
fn bindings(event: &Event, run_context: Option<&Attrs>) -> BTreeMap<String, Json> {
    let mut bindings = BTreeMap::new();
    if let Some(context) = run_context {
        bindings.insert(
            "context".to_owned(),
            Json::Object(
                context
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            ),
        );
    }
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
    use serde_json::json;

    #[test]
    fn the_code_is_required_and_the_timeout_is_the_other_setting() {
        let spec = RunJsCode::new().expect("client builds").config_spec();
        let names: Vec<&str> = spec.iter().map(|f| f.name()).collect();
        assert_eq!(names, vec![CFG_CODE, CFG_TIMEOUT]);
        assert!(spec[0].required);
        assert!(!spec[1].required, "a body may take the server's default");
        assert_eq!(spec[1].default, Some(json!(DEFAULT_TIMEOUT_MS)));
        // A configuration with nothing in it is a named error rather than an
        // empty body that silently returns null.
        let msg = config_str(&Attrs::new(), CFG_CODE).unwrap_err().to_string();
        assert!(msg.contains(CFG_CODE) && msg.contains("required"), "{msg}");
    }

    #[test]
    fn the_timeout_defers_to_the_engine_when_unset_and_is_bounded_when_set() {
        let config =
            |value: Json| -> Attrs { [(CFG_TIMEOUT.to_owned(), value)].into_iter().collect() };
        // Unset is not "5000" but "whatever this server's default is": resolving
        // it here would overrule a process started with another one.
        assert_eq!(timeout(&Attrs::new()).unwrap(), None);
        assert_eq!(timeout(&config(Json::Null)).unwrap(), None);
        assert_eq!(
            timeout(&config(json!(250))).unwrap(),
            Some(Duration::from_millis(250))
        );
        assert_eq!(
            timeout(&config(json!(MAX_TIMEOUT_MS))).unwrap(),
            Some(MAX_CODE_TIMEOUT),
            "the ceiling itself is allowed"
        );
        // Refused rather than clamped, and each refusal names the setting.
        for bad in [
            json!(MAX_TIMEOUT_MS + 1),
            json!(0),
            json!(-5),
            json!("soon"),
        ] {
            let msg = timeout(&config(bad.clone())).unwrap_err().to_string();
            assert!(msg.contains(CFG_TIMEOUT), "{bad}: {msg}");
        }
    }

    #[test]
    fn presence_is_scope_for_the_events_bindings() {
        // An update binds both rows; every name in the scope is there.
        let update = Event::new(EventKind::Update)
            .on("books")
            .row(json!({ "id": 1, "title": "now" }))
            .old_row(json!({ "id": 1, "title": "was" }))
            .caller(1, Some(json!({ "email": "a@b.c" })));
        let bound = bindings(&update, None);
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
        let bound = bindings(&insert, None);
        assert_eq!(bound["old"], Json::Null);
        assert_eq!(bound["user"], Json::Null, "anonymous binds null");

        // An event with no row binds neither, so code naming `row` there fails
        // in the engine instead of reading undefined.
        let called = Event::new(EventKind::None).payload(json!({ "n": 2 }));
        let bound = bindings(&called, None);
        assert!(!bound.contains_key("row") && !bound.contains_key("old"));
        assert_eq!(bound["payload"], json!({ "n": 2 }));

        // Outside a run there is no `context` at all; as a workflow step it is
        // bound — including on the first step, where it is empty. Presence is
        // scope, so a body may ask what has happened before anything has.
        assert!(!bound.contains_key("context"));
        let step = bindings(&called, Some(&Attrs::new()));
        assert_eq!(step["context"], json!({}));
        let later: Attrs = [("total".to_owned(), json!(120))].into_iter().collect();
        assert_eq!(
            bindings(&called, Some(&later))["context"],
            json!({"total": 120})
        );
    }
}
