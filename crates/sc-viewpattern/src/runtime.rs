//! [`ViewRuntime`]: the seam a view is rendered through (TODO "Saltcorn UI" §3,
//! Phase 3).
//!
//! A view pattern is v1's own JavaScript, and it runs on the module worker this
//! process already has (`sc-module`), so it is declared here and implemented one
//! layer up — the arrangement `sc-catalog` has with `TableProviderHost` and
//! `sc-app` with `FrameworkHost`. Nothing at this layer knows a worker exists.
//!
//! What crosses the seam is **the name of a view and data**. The view itself —
//! its pattern, its configuration, the views it embeds — is read out of the
//! [`ViewSnapshot`] the call carries, because an embedded view recurses inside
//! the worker and has to find its neighbours the way the outer one was found.
//!
//! Every call carries the viewer's surfaces ([`CodeHosts`]) and the schema
//! snapshot, so every read a pattern makes is a read under the viewer's
//! authority (§11). The seam does not decide that authority; its caller does.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use sc_error::{Error, Result};
use sc_expr::{CodeHosts, SchemaSnapshot};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::snapshot::ViewSnapshot;
use crate::view::{Page, View};

/// One view pattern as the runtime describes it — the registry manifest that
/// crosses **once**, when the runtime is first asked (§3.3).
///
/// Data only, because the admin UI asks these on the path that renders a form.
/// A configuration step's **fields** are not here: a step's form does not exist
/// without a table and the context gathered before it (§6), so it is a call —
/// [`ViewRuntime::config_step`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternManifest {
    /// v1's name for it — `List`, `Show` — and what a view's `viewpattern` holds.
    pub name: String,
    /// What the pattern picker shows. v1 patterns have no label of their own, so
    /// this is the name unless the pattern declares one.
    #[serde(default)]
    pub label: String,
    /// v1's one-line description.
    #[serde(default)]
    pub description: String,
    /// Whether a view of it must name a table (v1's `tableless`, inverted).
    #[serde(default)]
    pub table_required: bool,
    /// v1's `view_quantity`: `One`, `Many`, `ZeroOrOne`, or none declared.
    #[serde(default)]
    pub view_quantity: Option<String>,
    /// The routes it answers at `POST /view/:name/:route`.
    #[serde(default)]
    pub routes: Vec<String>,
    /// The names of its `configuration_workflow` steps, in order.
    #[serde(default)]
    pub steps: Vec<String>,
}

/// v1's `req.user`: who is looking, in the shape a pattern reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewUser {
    /// The user's id. This server's ids are UUIDs; a pattern compares them and
    /// never does arithmetic on them.
    pub id: String,
    /// The user's email.
    pub email: String,
    /// The user's role on the `1..=100` scale (v1's `role_id`).
    pub role_id: u8,
}

/// The request a view is rendered for: v1's `req`, as data.
///
/// The fields v1's patterns read and nothing else. Built by the framework from
/// the application request (§8), turned back into a `req`/`res` pair on the
/// worker, and read back out of it after the call ([`ViewOutput`]).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ViewRequest {
    /// `GET`, `POST`.
    pub method: String,
    /// The path the request arrived at, under the application.
    pub path: String,
    /// v1's `req.query`.
    pub query: BTreeMap<String, String>,
    /// v1's `req.body`: the form's fields, or the JSON posted. `null` for none.
    pub body: Json,
    /// The few headers v1 reads — `referer`, `x-requested-with` — lower-cased.
    pub headers: BTreeMap<String, String>,
    /// v1's `req.user`; `None` for an anonymous viewer.
    pub user: Option<ViewUser>,
    /// v1's `req.get_base_url()`: the application's own origin.
    pub base_url: String,
    /// What `req.csrfToken()` answers.
    pub csrf_token: String,
}

/// Everything one call through the seam may reach.
pub struct ViewContext<'a> {
    /// The application's views and pages, as the worker sees them (§4).
    pub snapshot: &'a ViewSnapshot,
    /// The request.
    pub request: &'a ViewRequest,
    /// The viewer's surfaces: every read and write the pattern makes goes
    /// through these, under the viewer's authority (§11).
    pub hosts: CodeHosts<'a>,
    /// The tables, as v1's synchronous `Table.findOne` answers them.
    pub schema: Option<&'a SchemaSnapshot>,
}

impl<'a> ViewContext<'a> {
    /// A context with no surfaces and no schema — which can render nothing that
    /// reads a table, and says so by name. What a test of the seam itself uses.
    pub fn bare(snapshot: &'a ViewSnapshot, request: &'a ViewRequest) -> ViewContext<'a> {
        ViewContext {
            snapshot,
            request,
            hosts: CodeHosts::default(),
            schema: None,
        }
    }
}

/// One flash message a pattern set with `req.flash(type, message)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flash {
    /// v1's type: `success`, `warning`, `danger`.
    pub kind: String,
    /// The message.
    pub message: String,
}

/// What a call through the seam produced: what the pattern returned, and what it
/// did to `res` on the way.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ViewOutput {
    /// What the pattern returned — the HTML of a run, whatever a POST or a
    /// route answered. `null` for nothing.
    pub body: Json,
    /// `res.status(code)`, if it was called.
    pub status: Option<u16>,
    /// `res.redirect(url)`, if it was called.
    pub redirect: Option<String>,
    /// `res.json(value)`, if it was called.
    pub json: Option<Json>,
    /// `res.send(value)` / `res.sendWrap(title, body)`, if either was called.
    pub sent: Option<Json>,
    /// Every `res.set(name, value)`, in order — v1's route sets `Page-Title`.
    pub headers: Vec<(String, String)>,
    /// Every `req.flash`, in order.
    pub flashes: Vec<Flash>,
}

/// One step of a pattern's `configuration_workflow`, over the context gathered
/// so far.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigStep {
    /// The step's name.
    pub name: String,
    /// How many steps the workflow has, so a wizard knows where it is.
    pub count: usize,
    /// Whether the step is a drag-and-drop **builder** (a layout) rather than a
    /// form — which this version shows read-only.
    pub builder: bool,
    /// The step's v1 `Form`, as JSON; `null` for a builder step.
    pub form: Json,
}

/// The seam a view is rendered through.
///
/// Object-safe, so the server holds one `Arc<dyn ViewRuntime>` and a test can
/// hold a fake.
#[async_trait]
pub trait ViewRuntime: Send + Sync {
    /// Every pattern the runtime has. Asked of the runtime once and kept; a
    /// runtime that has not started yet starts on this call.
    async fn patterns(&self) -> Result<Vec<PatternManifest>>;

    /// v1's `View.run`: the view's HTML for `state`.
    async fn render(&self, view: &View, state: &Json, ctx: ViewContext<'_>) -> Result<ViewOutput>;

    /// v1's `Page.run` and `renderLayout`: the page's HTML, with every view it
    /// embeds rendered in place.
    async fn render_page(&self, page: &Page, ctx: ViewContext<'_>) -> Result<ViewOutput>;

    /// v1's `View.runPost`: a form posted to the view.
    async fn post(&self, view: &View, body: &Json, ctx: ViewContext<'_>) -> Result<ViewOutput>;

    /// v1's `View.runRoute`: one of the pattern's named routes.
    async fn route(
        &self,
        view: &View,
        route: &str,
        body: &Json,
        ctx: ViewContext<'_>,
    ) -> Result<ViewOutput>;

    /// One step of `pattern`'s configuration workflow over `table`, with the
    /// `context` the earlier steps gathered — a call per step, because a step's
    /// form does not exist without that context (§6).
    async fn config_step(
        &self,
        pattern: &str,
        table: Option<&str>,
        step: usize,
        context: &Json,
        ctx: ViewContext<'_>,
    ) -> Result<ConfigStep>;
}

/// The installed runtime.
///
/// A global for the reason `sc-app`'s framework registry is one: there is one
/// view runtime per server, and threading it through every mount would put it
/// in the signature of most of the application layer for the benefit of a
/// lookup.
static INSTALLED: RwLock<Option<Arc<dyn ViewRuntime>>> = RwLock::new(None);

/// Install the view runtime, replacing whatever was installed.
pub fn install_view_runtime(runtime: Arc<dyn ViewRuntime>) -> Result<()> {
    let mut guard = INSTALLED
        .write()
        .map_err(|_| Error::msg("the view runtime registry lock is poisoned"))?;
    *guard = Some(runtime);
    Ok(())
}

/// The installed view runtime, or the sentence saying there is none.
///
/// A server built without the Saltcorn UI bundle installs none, and an
/// application that needs one is refused on mount (`require_view_runtime`); this
/// is the same fact reached from the other side.
pub fn view_runtime() -> Result<Arc<dyn ViewRuntime>> {
    let guard = match INSTALLED.read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.clone().ok_or_else(|| {
        Error::config(
            "no view runtime is running on this server, so no Saltcorn UI view can be \
             rendered; it is started with the modules, from the Saltcorn UI bundle",
        )
    })
}
