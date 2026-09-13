//! Saltcorn UI's **view runtime** on the module worker, as `sc-viewpattern`'s
//! [`ViewRuntime`] (TODO "Saltcorn UI" §3).
//!
//! v1's view patterns are v1's own source, vendored into the `ui/saltcorn-ui`
//! bundle, and they run where every other piece of v1 JavaScript on this server
//! runs: a module worker, with its call table, its wall clock, its JS slice, its
//! heap cap and its permission set. So the runtime is **a module** — a built-in
//! one, `@feldspar/saltcorn-ui`, loaded from the bundle rather than from the
//! modules root, granted nothing — and this is the few lines that make it the
//! seam the rest of the server renders through.
//!
//! Not a module like the installed ones in two respects, both deliberate:
//!
//! - **It is not stored and not replayed.** The bundle is part of the server's
//!   artifact, so there is no row, and the worker imports it the first time a
//!   view is asked for (and again after a restart) rather than being told to.
//! - **Its name is reserved but not in the installed modules' namespace.** It
//!   is pinned beside them, so a package that happens to be called
//!   `@feldspar/saltcorn-ui` loads as the ordinary module it is — its card says
//!   so — and cannot take the runtime's place.
//!
//! What crosses is a view's **name** and data; the view itself is read out of
//! the [`ViewSnapshot`](sc_viewpattern::ViewSnapshot) the call carries, which the
//! worker holds per generation. A pattern's throw comes back as an
//! **Application** error naming the view, never as a stack.

use std::sync::Arc;

use async_trait::async_trait;
use sc_error::{Error, ErrorKind, Repr, Result};
use sc_viewpattern::{
    ConfigStep, Flash, Page, PatternManifest, View, ViewContext, ViewOutput, ViewRequest,
    ViewRuntime,
};
use serde_json::{Value as Json, json};
use tokio::sync::OnceCell;

use crate::host::{CallHosts, ModuleHost};

/// The name the built-in view runtime is known by — in the log, and on the card
/// of any installed package that claims it.
pub const BUILTIN_VIEW_RUNTIME: &str = "@feldspar/saltcorn-ui";

/// The view runtime, over the module pool.
pub struct ModuleViewRuntime {
    host: Arc<ModuleHost>,
    /// The pattern manifest, asked once and kept: it is the bundle's registry,
    /// and the bundle does not change while the server runs.
    patterns: OnceCell<Vec<PatternManifest>>,
}

impl ModuleViewRuntime {
    /// The runtime over `host`, whose workers must have been given the bundle
    /// ([`ModuleHost::with_view_runtime`]). Nothing starts until the first call.
    pub fn new(host: &Arc<ModuleHost>) -> ModuleViewRuntime {
        ModuleViewRuntime {
            host: Arc::clone(host),
            patterns: OnceCell::new(),
        }
    }
}

/// A context's surfaces and snapshot as the call a worker is handed, and the
/// request beside them.
fn call_of(ctx: ViewContext<'_>) -> (CallHosts<'_>, &ViewRequest) {
    let ViewContext {
        snapshot,
        request,
        hosts,
        schema,
    } = ctx;
    (CallHosts::new(hosts, schema).with_views(snapshot), request)
}

#[async_trait]
impl ViewRuntime for ModuleViewRuntime {
    async fn patterns(&self) -> Result<Vec<PatternManifest>> {
        self.patterns
            .get_or_try_init(|| async {
                let answer = self
                    .host
                    .view_call("view_patterns", json!({}), CallHosts::default())
                    .await
                    .map_err(|e| failed("the view patterns", "listed", e))?;
                serde_json::from_value::<Vec<PatternManifest>>(answer).map_err(|e| {
                    Error::msg(format!(
                        "the view runtime answered its pattern list with something this server \
                         cannot read: {e}"
                    ))
                })
            })
            .await
            .cloned()
    }

    async fn render(&self, view: &View, state: &Json, ctx: ViewContext<'_>) -> Result<ViewOutput> {
        let (call, request) = call_of(ctx);
        let answer = self
            .host
            .view_call(
                "view_render",
                json!({ "view": view.name, "state": state, "request": request }),
                call,
            )
            .await
            .map_err(|e| failed(&view_named(view), "rendered", e))?;
        output(answer)
    }

    async fn render_page(&self, page: &Page, ctx: ViewContext<'_>) -> Result<ViewOutput> {
        let (call, request) = call_of(ctx);
        let answer = self
            .host
            .view_call(
                "view_render_page",
                json!({ "page": page.name, "request": request }),
                call,
            )
            .await
            .map_err(|e| failed(&format!("the page `{}`", page.name), "rendered", e))?;
        output(answer)
    }

    async fn post(&self, view: &View, body: &Json, ctx: ViewContext<'_>) -> Result<ViewOutput> {
        let (call, request) = call_of(ctx);
        let answer = self
            .host
            .view_call(
                "view_post",
                json!({ "view": view.name, "body": body, "request": request }),
                call,
            )
            .await
            .map_err(|e| failed(&view_named(view), "posted to", e))?;
        output(answer)
    }

    async fn route(
        &self,
        view: &View,
        route: &str,
        body: &Json,
        ctx: ViewContext<'_>,
    ) -> Result<ViewOutput> {
        let (call, request) = call_of(ctx);
        let answer = self
            .host
            .view_call(
                "view_route",
                json!({ "view": view.name, "route": route, "body": body, "request": request }),
                call,
            )
            .await
            .map_err(|e| {
                failed(
                    &format!("the route `{route}` of {}", view_named(view)),
                    "run",
                    e,
                )
            })?;
        output(answer)
    }

    async fn config_step(
        &self,
        pattern: &str,
        table: Option<&str>,
        step: usize,
        context: &Json,
        ctx: ViewContext<'_>,
    ) -> Result<ConfigStep> {
        let (call, request) = call_of(ctx);
        let answer = self
            .host
            .view_call(
                "view_config_step",
                json!({
                    "pattern": pattern,
                    "table": table,
                    "step": step,
                    "context": context,
                    "request": request,
                }),
                call,
            )
            .await
            .map_err(|e| {
                failed(
                    &format!("step {step} of the {pattern} pattern's configuration"),
                    "built",
                    e,
                )
            })?;
        Ok(ConfigStep {
            name: answer
                .get("name")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_owned(),
            count: answer
                .get("count")
                .and_then(Json::as_u64)
                .and_then(|c| usize::try_from(c).ok())
                .unwrap_or(0),
            builder: answer
                .get("builder")
                .and_then(Json::as_bool)
                .unwrap_or(false),
            form: answer.get("form").cloned().unwrap_or(Json::Null),
        })
    }
}

/// How a view is named in a failure: its name, and the pattern that failed.
fn view_named(view: &View) -> String {
    format!("the view `{}` ({})", view.name, view.viewpattern)
}

/// A failure through the seam, with what failed put in front of it.
///
/// The **kind is kept**: a pattern's throw is the application's to fix and stays
/// an Application error, and a worker that could not be reached stays a System
/// one. The reason is the worker's own sentence, without the "configuration
/// error:" its kind would print in front of it a second time.
fn failed(what: &str, doing: &str, error: Error) -> Error {
    let reason = match error.repr() {
        Repr::Config(m) | Repr::Invalid(m) | Repr::NotFound(m) => m.clone(),
        _ => error.to_string(),
    };
    let message = format!("{what} could not be {doing}: {reason}");
    match error.kind() {
        ErrorKind::Application => Error::config(message),
        ErrorKind::System => Error::msg(message),
    }
}

/// A view call's answer — `{ value, response }` — as a [`ViewOutput`].
fn output(answer: Json) -> Result<ViewOutput> {
    let Json::Object(mut answer) = answer else {
        return Err(Error::msg(format!(
            "the view runtime answered a view call with {answer}, which is not the object this \
             server asked for"
        )));
    };
    let response = answer.remove("response").unwrap_or(Json::Null);
    let flashes = match response.get("flashes") {
        Some(flashes) => serde_json::from_value::<Vec<Flash>>(flashes.clone()).map_err(|e| {
            Error::msg(format!(
                "the view runtime answered unreadable flash messages: {e}"
            ))
        })?,
        None => Vec::new(),
    };
    Ok(ViewOutput {
        body: answer.remove("value").unwrap_or(Json::Null),
        status: response
            .get("status")
            .and_then(Json::as_u64)
            .and_then(|s| u16::try_from(s).ok()),
        redirect: response
            .get("redirect")
            .and_then(Json::as_str)
            .map(str::to_owned),
        json: response.get("json").cloned(),
        sent: response.get("sent").cloned(),
        headers: match response.get("headers") {
            Some(headers) => serde_json::from_value::<Vec<(String, String)>>(headers.clone())
                .map_err(|e| {
                    Error::msg(format!(
                        "the view runtime answered unreadable response headers: {e}"
                    ))
                })?,
            None => Vec::new(),
        },
        flashes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_pattern_did_to_res_comes_back_as_data() {
        let out = output(json!({
            "value": "<p>hi</p>",
            "response": {
                "status": 302,
                "redirect": "/view/List%20Books",
                "flashes": [{ "kind": "success", "message": "Saved" }],
                "headers": [["Page-Title", "Books"]],
            },
        }))
        .unwrap();
        assert_eq!(out.headers, [("Page-Title".to_owned(), "Books".to_owned())]);
        assert_eq!(out.body, json!("<p>hi</p>"));
        assert_eq!(out.status, Some(302));
        assert_eq!(out.redirect.as_deref(), Some("/view/List%20Books"));
        assert_eq!(out.json, None);
        assert_eq!(out.flashes[0].message, "Saved");
    }

    /// §7: the `getConfig` keys the runtime declares are one list, and every
    /// one of them is either a setting the framework offers an admin or a key
    /// the application already knows. A key added to `CONFIG_KEYS` without a
    /// setting fails here, rather than silently answering its default forever.
    #[test]
    fn every_get_config_key_the_runtime_declares_is_a_setting_or_derived() {
        let script = crate::host::HOST_SCRIPT;
        let start = script
            .find("const CONFIG_KEYS = {")
            .expect("the runtime declares its getConfig keys");
        let end = start
            + script[start..]
                .find("\n};")
                .expect("the declaration closes");
        let keys: Vec<&str> = script[start..end]
            .lines()
            .filter_map(|line| line.strip_prefix("  "))
            .filter_map(|line| line.split_once(':').map(|(key, _)| key))
            .filter(|key| {
                !key.is_empty() && key.chars().all(|c| c.is_ascii_lowercase() || c == '_')
            })
            .collect();
        assert!(keys.contains(&"site_name") && keys.len() > 5, "{keys:?}");

        let settings: Vec<String> = sc_viewpattern::saltcorn_ui_config_spec()
            .iter()
            .map(|f| f.name().to_owned())
            .collect();
        for key in &keys {
            assert!(
                settings.iter().any(|s| s == key)
                    || sc_viewpattern::DERIVED_CONFIG_KEYS.contains(key),
                "`{key}` is a getConfig key with no setting: add it to saltcorn_ui_config_spec"
            );
        }
        for setting in &settings {
            assert!(
                keys.contains(&setting.as_str()) || setting == sc_viewpattern::CFG_ROOT_PAGES,
                "`{setting}` is a setting the runtime never reads"
            );
        }
    }

    #[test]
    fn an_answer_that_is_not_the_protocol_says_so() {
        let msg = output(json!("<p>hi</p>")).unwrap_err().to_string();
        assert!(msg.contains("not the object"), "{msg}");
    }

    #[test]
    fn a_patterns_throw_stays_the_applications_and_names_the_view() {
        let view = View::new(sc_app::AppId::new(), "List Books", "List", "books");
        let err = failed(
            &view_named(&view),
            "rendered",
            Error::config("Table.findOne is not available here"),
        );
        assert_eq!(err.kind(), ErrorKind::Application);
        let msg = err.to_string();
        assert!(msg.contains("`List Books` (List)"), "{msg}");
        assert_eq!(msg.matches("configuration error").count(), 1, "{msg}");

        let err = failed(&view_named(&view), "rendered", Error::msg("no such worker"));
        assert_eq!(err.kind(), ErrorKind::System);
    }
}
