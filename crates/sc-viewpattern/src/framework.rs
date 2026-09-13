//! Saltcorn UI as a [`Framework`] (TODO "Saltcorn UI" Phase 5): the fourth
//! framework, and the first with nothing to build.
//!
//! An application whose framework is `saltcorn-ui` owns views and pages rather
//! than a source tree, and this is what serves them on its subdomain:
//!
//! - `/` — the viewer's home page, else a document naming what there is;
//! - `/view/:name` and `/view/:name/*slug` — one view, through the
//!   [`ViewRuntime`];
//! - `/page/:name` — one page, with the views it embeds rendered in place;
//! - `/static_assets/:tag/*` — the browser half of v1 (Bootstrap, jQuery,
//!   `saltcorn.js`), out of the bundle's `public/`;
//! - `/files/serve/*` — the application's file stores, under their access rules.
//!
//! **No build**: [`SaltcornUiFactory`] constructs the framework when the
//! application is mounted, and a mount re-reads the application's view set
//! under a new generation (§4). What a request renders is always the view set
//! as of the last write, because every write goes through [`view_sets`].
//!
//! **Whose authority**: every read a view makes is under the viewer's — the
//! surfaces a call carries are built from the request's user, exactly as a code
//! body's are built from its event's caller (§11).

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use async_trait::async_trait;
use bytes::Bytes;
use sc_action::TriggerDispatcher;
use sc_api::code_host::{FileStoreHost, TableHost, TriggerRunHost, schema_snapshot};
use sc_app::{
    AppRequest, AppResponse, Application, BuildSpec, CspPolicy, Framework, FrameworkFactory,
    FrameworkInfo, Method, MountContext,
};
use sc_auth::User;
use sc_catalog::Catalog;
use sc_error::{Error, ErrorKind, Repr, Result};
use sc_expr::{CodeHosts, JsEvaluator, ModuleFnHost, TriggerHost};
use sc_types::{Attrs, BasicType, FormField};
use serde_json::{Map, Value as Json};

use crate::bundle::{SALTCORN_UI_FRAMEWORK, require_view_runtime};
use crate::runtime::{
    ViewContext, ViewOutput, ViewRequest, ViewRuntime, ViewUser, Wrap, view_runtime,
};
use crate::snapshot::{MENU_CONFIG_KEY, ViewSnapshot};
use crate::view::{Page, View};
use crate::view_set::{ViewSet, ViewSets};

/// The `site_name` setting: the brand in the navbar, and `getConfig("site_name")`.
pub const CFG_SITE_NAME: &str = "site_name";

/// The `root_pages` setting: which page `/` is for each role, as
/// `{ "<role id>": "<page name>" }`. A role it does not name falls back to the
/// pages whose `root_page_for_roles` attribute names it, which is where a
/// restored v1 backup keeps the answer.
pub const CFG_ROOT_PAGES: &str = "root_pages";

/// The `getConfig` keys the view runtime answers that are **not** settings: the
/// application knows them (`base_url`), or the server decides them
/// (`enable_dynamic_updates`, always off). Every other declared key is offered
/// by [`saltcorn_ui_config_spec`] — §7's rule, held by a test in `sc-module`
/// against the runtime's own list.
pub const DERIVED_CONFIG_KEYS: [&str; 2] = ["base_url", "enable_dynamic_updates"];

/// The version tag in `/static_assets/<tag>/…`. The server's version, so an
/// upgrade is a new URL and a year-long cache is safe.
pub const ASSET_VERSION_TAG: &str = env!("CARGO_PKG_VERSION");

/// The public role, which an anonymous viewer has.
const ROLE_PUBLIC: u8 = 100;

/// The view sets every Saltcorn UI application renders from, one per
/// application, shared by the mount, the renders and every writer.
///
/// One per process for the reason [`view_runtime`] is: an application's views
/// are the application's wherever they are read from, and a write that reloaded
/// one cache while a render read another would be the stale read §4's
/// generation exists to rule out.
pub fn view_sets() -> &'static ViewSets {
    static SETS: OnceLock<ViewSets> = OnceLock::new();
    SETS.get_or_init(ViewSets::new)
}

/// Install Saltcorn UI into `sc-app`'s framework registry. Idempotent.
pub fn install_saltcorn_ui() -> Result<()> {
    sc_app::install_framework_factory(Arc::new(SaltcornUiFactory))
}

/// The settings a Saltcorn UI application has (5.4): the brand, the menu, the
/// home page per role, and every `getConfig` key the view runtime declares that
/// the application does not already know.
pub fn saltcorn_ui_config_spec() -> Vec<FormField> {
    vec![
        FormField::new(CFG_SITE_NAME, BasicType::Text)
            .label("Site name")
            .default_value(""),
        FormField::new(MENU_CONFIG_KEY, BasicType::Json)
            .label("Menu")
            .default_value(Json::Array(Vec::new())),
        FormField::new(CFG_ROOT_PAGES, BasicType::Json)
            .label("Home page per role")
            .default_value(Json::Object(Map::new())),
        FormField::new("default_locale", BasicType::Text)
            .label("Default locale")
            .default_value("en"),
        FormField::new("login_form", BasicType::Text)
            .label("Login form view")
            .default_value(""),
        FormField::new("exttables_min_role_read", BasicType::Json)
            .label("External tables: read role per table")
            .default_value(Json::Object(Map::new())),
        FormField::new("layout_by_role", BasicType::Json)
            .label("Layout per role")
            .default_value(Json::Object(Map::new())),
        FormField::new("push_policy_by_role", BasicType::Json)
            .label("Push policy per role")
            .default_value(Json::Object(Map::new())),
        FormField::new("localizer_languages", BasicType::Json)
            .label("Languages")
            .default_value(Json::Object(Map::new())),
        FormField::new("search_disable_fts", BasicType::Bool)
            .label("Search: no full-text search")
            .default_value(false),
        FormField::new("search_use_websearch", BasicType::Bool)
            .label("Search: web-style queries")
            .default_value(false),
    ]
}

/// What the spec's vocabulary cannot say about the settings: the menu is a list
/// of entries and the home pages a map of role to page name.
pub fn check_saltcorn_ui_config(config: &Attrs) -> Result<()> {
    fn check_menu(items: &Json, at: &str) -> Result<()> {
        let Json::Array(items) = items else {
            return Err(Error::invalid(format!(
                "framework `{SALTCORN_UI_FRAMEWORK}`: `{at}` must be a list of menu entries"
            )));
        };
        for (i, item) in items.iter().enumerate() {
            let entry = format!("{at}[{i}]");
            if !item.get("type").is_some_and(Json::is_string) {
                return Err(Error::invalid(format!(
                    "framework `{SALTCORN_UI_FRAMEWORK}`: menu entry `{entry}` has no `type` \
                     (View, Page, Link or Header)"
                )));
            }
            if let Some(sub) = item.get("subitems").filter(|s| !s.is_null()) {
                check_menu(sub, &format!("{entry}.subitems"))?;
            }
        }
        Ok(())
    }
    if let Some(menu) = config.get(MENU_CONFIG_KEY).filter(|m| !m.is_null()) {
        check_menu(menu, MENU_CONFIG_KEY)?;
    }
    if let Some(roots) = config.get(CFG_ROOT_PAGES).filter(|r| !r.is_null()) {
        let ok = roots.as_object().is_some_and(|m| {
            m.iter()
                .all(|(role, page)| role.parse::<u8>().is_ok() && page.is_string())
        });
        if !ok {
            return Err(Error::invalid(format!(
                "framework `{SALTCORN_UI_FRAMEWORK}`: `{CFG_ROOT_PAGES}` must map a role id to a \
                 page name, like {{\"1\": \"Dashboard\", \"100\": \"Welcome\"}}"
            )));
        }
    }
    Ok(())
}

/// The policy a Saltcorn UI application gets unless the admin states one (§10):
/// the strict baseline with `script-src` widened by `'unsafe-inline'`, because
/// v1's markup puts JavaScript in `onclick` attributes, which cannot be nonced.
/// Nothing else moves — no `eval`, no `blob:`, no other origin.
pub fn saltcorn_ui_csp() -> CspPolicy {
    CspPolicy::strict().directive(
        "script-src",
        vec!["'self'".to_owned(), "'unsafe-inline'".to_owned()],
    )
}

/// Saltcorn UI in the framework registry: its picker entry, its settings, and
/// the mount that constructs it.
pub struct SaltcornUiFactory;

#[async_trait]
impl FrameworkFactory for SaltcornUiFactory {
    fn info(&self) -> FrameworkInfo {
        FrameworkInfo {
            name: SALTCORN_UI_FRAMEWORK.to_owned(),
            label: "Saltcorn UI".to_owned(),
            description: "Views and pages you configure here, rendered by the server with \
                          Saltcorn 1's own List, Show, Edit, Feed and Filter. Nothing to write \
                          and nothing to build: pick the tables it shows."
                .to_owned(),
            serves_ui: true,
        }
    }

    fn config_spec(&self) -> Vec<FormField> {
        saltcorn_ui_config_spec()
    }

    fn default_csp(&self) -> CspPolicy {
        saltcorn_ui_csp()
    }

    fn check_config(&self, config: &Attrs) -> Result<()> {
        check_saltcorn_ui_config(config)
    }

    /// Construct the framework — after checking, **once, here**, that this
    /// server can render anything at all (2.6): the bundle it was built with,
    /// and the view runtime started from it. The view set is re-read under a new
    /// generation, which is what makes a mount the reload of an application
    /// (5.7).
    async fn mount(&self, app: &Application, ctx: MountContext<'_>) -> Result<Arc<dyn Framework>> {
        let runtime_file = require_view_runtime(ctx.bundle_dir)?;
        let bundle = runtime_file
            .parent()
            .map_or_else(PathBuf::new, Path::to_path_buf);
        let runtime = view_runtime()?;
        view_sets().reload(ctx.catalog, app.id).await?;
        Ok(Arc::new(SaltcornUiFramework {
            app: app.clone(),
            evaluator: ctx.evaluator,
            triggers: ctx.triggers,
            public_dir: bundle.join("public"),
            runtime,
            snapshot: RwLock::new(None),
        }))
    }
}

/// One mounted Saltcorn UI application.
pub struct SaltcornUiFramework {
    app: Application,
    evaluator: Option<Arc<dyn JsEvaluator>>,
    triggers: Option<Arc<TriggerDispatcher>>,
    /// The bundle's `public/`: what `/static_assets/` serves.
    public_dir: PathBuf,
    runtime: Arc<dyn ViewRuntime>,
    /// The snapshot of the view set's current generation, built once for it.
    snapshot: RwLock<Option<Arc<ViewSnapshot>>>,
}

#[async_trait]
impl Framework for SaltcornUiFramework {
    fn name(&self) -> &str {
        SALTCORN_UI_FRAMEWORK
    }

    fn config_spec(&self) -> Vec<FormField> {
        saltcorn_ui_config_spec()
    }

    async fn handle(&self, req: AppRequest, cat: &Catalog) -> Result<AppResponse> {
        // Posting is Phase 6.
        if !matches!(req.method, Method::Get) {
            return Ok(AppResponse::method_not_allowed());
        }
        let path = req.path.clone();
        let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        match segments.as_slice() {
            [""] => self.index(&req, cat).await,
            ["view", name, slug @ ..] if !name.is_empty() => {
                self.view(&req, cat, &percent_decode(name), slug).await
            }
            ["page", name] if !name.is_empty() => self.page(&req, cat, &percent_decode(name)).await,
            ["static_assets", _tag, rest @ ..] if !rest.is_empty() => self.static_asset(rest),
            ["files", "serve", rest @ ..] if !rest.is_empty() => self.file(&req, cat, rest).await,
            _ => Ok(self.message(404, "Not found", "There is nothing at this address.")),
        }
    }

    fn build(&self) -> Option<BuildSpec> {
        None
    }
}

impl SaltcornUiFramework {
    /// `/`: the viewer's home page, or a document naming the pages and views
    /// there are (DoD step 5).
    async fn index(&self, req: &AppRequest, cat: &Catalog) -> Result<AppResponse> {
        let set = view_sets().get(cat, self.app.id).await?;
        let role = role_of(req);
        if let Some(page) = self.home_page(cat, &set, role).await? {
            return self.render_page(req, cat, &set, page).await;
        }
        let site = escape(&self.site_name());
        let links = |kind: &str, names: Vec<&str>| -> String {
            names
                .iter()
                .map(|name| {
                    format!(
                        "<li><a href=\"/{kind}/{}\">{}</a></li>",
                        escape(&percent_encode(name)),
                        escape(name)
                    )
                })
                .collect::<String>()
        };
        let body = if set.pages.is_empty() && set.views.is_empty() {
            "<h1>Nothing here yet</h1><p>This application has no pages and no views.</p>".to_owned()
        } else {
            let mut body =
                format!("<h1>{site}</h1><p>There is no home page here for your role.</p>");
            if !set.pages.is_empty() {
                body.push_str("<h2>Pages</h2><ul>");
                body.push_str(&links(
                    "page",
                    set.pages.iter().map(|p| p.name.as_str()).collect(),
                ));
                body.push_str("</ul>");
            }
            if !set.views.is_empty() {
                body.push_str("<h2>Views</h2><ul>");
                body.push_str(&links(
                    "view",
                    set.views.iter().map(|v| v.name.as_str()).collect(),
                ));
                body.push_str("</ul>");
            }
            body
        };
        Ok(AppResponse::html(
            200,
            self.document(
                &self.site_name(),
                &format!("<main class=\"container py-4\">{body}</main>"),
            ),
        ))
    }

    /// The page `/` is for `role`: the `root_pages` setting, then a page whose
    /// `root_page_for_roles` names the role by id or by name.
    async fn home_page<'s>(
        &self,
        cat: &Catalog,
        set: &'s ViewSet,
        role: u8,
    ) -> Result<Option<&'s Page>> {
        let configured = self
            .app
            .framework
            .config
            .get(CFG_ROOT_PAGES)
            .and_then(|m| m.get(role.to_string()))
            .and_then(Json::as_str);
        if let Some(name) = configured
            && let Some(page) = set.page(name)
        {
            return Ok(Some(page));
        }
        let claims = |p: &&Page| {
            p.attributes
                .get("root_page_for_roles")
                .and_then(Json::as_array)
                .is_some_and(|roles| !roles.is_empty())
        };
        if !set.pages.iter().any(|p| claims(&p)) {
            return Ok(None);
        }
        let role_name = sc_auth::list_roles(cat)
            .await?
            .into_iter()
            .find(|r| r.role == role)
            .map(|r| r.name);
        Ok(set.pages.iter().filter(claims).find(|p| {
            p.attributes["root_page_for_roles"]
                .as_array()
                .is_some_and(|roles| {
                    roles.iter().any(|r| {
                        r.as_u64() == Some(u64::from(role))
                            || r.as_str().is_some_and(|s| {
                                s == role.to_string() || Some(s) == role_name.as_deref()
                            })
                    })
                })
        }))
    }

    /// `/view/:name` and `/view/:name/*slug`: v1's view GET. An ajax request —
    /// the Filter's reload of the list under it — gets the view's HTML alone;
    /// a navigation gets it in the layout.
    async fn view(
        &self,
        req: &AppRequest,
        cat: &Catalog,
        name: &str,
        slug: &[&str],
    ) -> Result<AppResponse> {
        let set = view_sets().get(cat, self.app.id).await?;
        let Some(view) = set.view(name) else {
            return Ok(self.message(
                404,
                "Not found",
                &format!("This application has no view named {name}."),
            ));
        };
        if let Some(refused) = self.refused(req, &format!("the view {name}"), view.min_role) {
            return Ok(refused);
        }
        let state = state_of(req, view, slug);
        let title = view
            .attributes
            .get("page_title")
            .and_then(Json::as_str)
            .filter(|t| !t.trim().is_empty())
            .unwrap_or(&view.name)
            .to_owned();
        let request = self.view_request(
            req,
            (!is_xhr(req)).then(|| Wrap {
                title,
                current_url: req.path.clone(),
            }),
        );
        let snapshot = self.snapshot(cat, &set, &req.base_url).await?;
        let schema = schema_snapshot(cat)?;
        let hosts = ViewerHosts::new(
            cat,
            req.user.as_ref(),
            self.evaluator.clone(),
            self.triggers.as_deref(),
        );
        let ctx = ViewContext {
            snapshot: &snapshot,
            request: &request,
            hosts: hosts.surfaces(),
            schema: Some(&schema),
        };
        let out = self.runtime.render(view, &state, ctx).await;
        self.respond(out, &request)
    }

    /// `/page/:name`.
    async fn page(&self, req: &AppRequest, cat: &Catalog, name: &str) -> Result<AppResponse> {
        let set = view_sets().get(cat, self.app.id).await?;
        let Some(page) = set.page(name) else {
            return Ok(self.message(
                404,
                "Not found",
                &format!("This application has no page named {name}."),
            ));
        };
        self.render_page(req, cat, &set, page).await
    }

    async fn render_page(
        &self,
        req: &AppRequest,
        cat: &Catalog,
        set: &ViewSet,
        page: &Page,
    ) -> Result<AppResponse> {
        if let Some(refused) = self.refused(req, &format!("the page {}", page.name), page.min_role)
        {
            return Ok(refused);
        }
        let title = if page.title.trim().is_empty() {
            page.name.clone()
        } else {
            page.title.clone()
        };
        let request = self.view_request(
            req,
            (!is_xhr(req)).then(|| Wrap {
                title,
                current_url: req.path.clone(),
            }),
        );
        let snapshot = self.snapshot(cat, set, &req.base_url).await?;
        let schema = schema_snapshot(cat)?;
        let hosts = ViewerHosts::new(
            cat,
            req.user.as_ref(),
            self.evaluator.clone(),
            self.triggers.as_deref(),
        );
        let ctx = ViewContext {
            snapshot: &snapshot,
            request: &request,
            hosts: hosts.surfaces(),
            schema: Some(&schema),
        };
        let out = self.runtime.render_page(page, ctx).await;
        self.respond(out, &request)
    }

    /// `/static_assets/:tag/*`: a file of the bundle's `public/`, and nothing
    /// outside it.
    fn static_asset(&self, rest: &[&str]) -> Result<AppResponse> {
        let parts: Vec<String> = rest.iter().map(|p| percent_decode(p)).collect();
        if !confined(&parts) {
            return Ok(AppResponse::not_found());
        }
        let file = parts
            .iter()
            .fold(self.public_dir.clone(), |dir, p| dir.join(p));
        match std::fs::read(&file) {
            Ok(bytes) => Ok(AppResponse::ok(
                sc_app::asset_content_type(&parts.join("/")),
                Bytes::from(bytes),
            )
            .header("Cache-Control", "public, max-age=31536000, immutable")),
            Err(_) => Ok(AppResponse::not_found()),
        }
    }

    /// `/files/serve/*`: a file from one of the application's stores, under the
    /// store's and the path's access rules. `/files/serve/<store>/<path>` names
    /// the store; a path whose first segment is not one of the application's
    /// stores is in its first store, which is where v1's one file area lands.
    ///
    /// A file the viewer may not read is not found, as it is to the REST
    /// provider: "forbidden" would say it exists.
    async fn file(&self, req: &AppRequest, cat: &Catalog, rest: &[&str]) -> Result<AppResponse> {
        let parts: Vec<String> = rest.iter().map(|p| percent_decode(p)).collect();
        if !confined(&parts) {
            return Ok(AppResponse::not_found());
        }
        let stores = &self.app.file_stores;
        let (store_name, path) = match stores.iter().find(|s| parts.len() > 1 && s.0 == parts[0]) {
            Some(store) => (store.0.clone(), parts[1..].join("/")),
            None => match stores.first() {
                Some(store) => (store.0.clone(), parts.join("/")),
                None => return Ok(AppResponse::not_found()),
            },
        };
        let Ok(store) = cat.require_file_store(&store_name) else {
            return Ok(AppResponse::not_found());
        };
        let floor = sc_catalog::load_file_store_by_name(cat, &store_name)
            .await?
            .and_then(|def| def.min_role);
        if sc_files::check_access(store.as_ref(), floor, &path, role_of(req))
            .await
            .is_err()
        {
            return Ok(AppResponse::not_found());
        }
        match store.read(&path).await {
            Ok(bytes) => Ok(AppResponse::ok(
                sc_files::mime_for_path(&path)
                    .unwrap_or_else(|| "application/octet-stream".to_owned()),
                bytes,
            )),
            Err(e) if matches!(e.repr(), Repr::NotFound(_)) => Ok(AppResponse::not_found()),
            Err(e) => Err(e),
        }
    }

    /// v1's `req`, for this request.
    fn view_request(&self, req: &AppRequest, wrap: Option<Wrap>) -> ViewRequest {
        ViewRequest {
            method: req.method.as_str().to_owned(),
            path: req.path.clone(),
            query: req.query.clone(),
            body: Json::Null,
            headers: req.headers.clone(),
            user: req.user.as_ref().map(view_user),
            base_url: req.base_url.clone(),
            csrf_token: String::new(),
            wrap,
        }
    }

    /// The snapshot of `set`, built once per generation.
    ///
    /// Keyed on the generation alone, as the worker's copy is: the base URL in
    /// it is the one the generation was first rendered at, and a request's own
    /// base URL reaches `req.get_base_url()` directly.
    async fn snapshot(
        &self,
        cat: &Catalog,
        set: &ViewSet,
        base_url: &str,
    ) -> Result<Arc<ViewSnapshot>> {
        if let Some(held) = self.snapshot.read().map_err(|_| poisoned())?.as_ref()
            && held.generation() == set.generation
        {
            return Ok(held.clone());
        }
        let roles = sc_auth::list_roles(cat).await?;
        let built = Arc::new(ViewSnapshot::build(&self.app, set, &roles, base_url)?);
        *self.snapshot.write().map_err(|_| poisoned())? = Some(built.clone());
        Ok(built)
    }

    /// What a render produced, as a response.
    fn respond(&self, out: Result<ViewOutput>, request: &ViewRequest) -> Result<AppResponse> {
        let out = match out {
            Ok(out) => out,
            // A pattern that threw is the application's to fix: a document
            // saying which view and why, not a stack. A worker that could not be
            // reached is the server's, and goes up.
            Err(e) if e.kind() == ErrorKind::Application => {
                eprintln!("feldspar: application `{}`: {e}", self.app.subdomain);
                let reason = match e.repr() {
                    Repr::Config(m) | Repr::Invalid(m) | Repr::NotFound(m) => m.clone(),
                    _ => e.to_string(),
                };
                return Ok(self.message(500, "This could not be shown", &reason));
            }
            Err(e) => return Err(e),
        };
        let mut response = if let Some(location) = out.redirect {
            let status = out.status.filter(|s| (300..400).contains(s)).unwrap_or(302);
            AppResponse::with_status(status, "text/plain; charset=utf-8", Bytes::new())
                .header("Location", location)
        } else if let Some(json) = out.json {
            AppResponse::with_status(
                out.status.unwrap_or(200),
                "application/json",
                serde_json::to_vec(&json).map_err(|e| Error::serde(e.to_string()))?,
            )
        } else {
            let html = match out.sent.unwrap_or(out.body) {
                Json::String(html) => html,
                Json::Null => String::new(),
                other => other.to_string(),
            };
            let status = out.status.unwrap_or(200);
            match &request.wrap {
                Some(wrap) => AppResponse::html(status, self.document(&wrap.title, &html)),
                None => AppResponse::html(status, html),
            }
        };
        for (name, value) in out.headers {
            response = response.header(name, value);
        }
        Ok(response)
    }

    /// `Some(refusal)` when the viewer's role is below `min_role` (§11's first
    /// check). Phase 7 turns the anonymous case into a login.
    fn refused(&self, req: &AppRequest, what: &str, min_role: u8) -> Option<AppResponse> {
        if role_of(req) <= min_role {
            return None;
        }
        Some(match req.user {
            None => self.message(
                401,
                "Please sign in",
                &format!("You need to sign in to see {what}."),
            ),
            Some(_) => self.message(
                403,
                "Not permitted",
                &format!("Your role may not see {what}."),
            ),
        })
    }

    /// A short document: a heading and a sentence.
    fn message(&self, status: u16, heading: &str, sentence: &str) -> AppResponse {
        AppResponse::html(
            status,
            self.document(
                heading,
                &format!(
                    "<main class=\"container py-4\"><h1>{}</h1><p>{}</p></main>",
                    escape(heading),
                    escape(sentence)
                ),
            ),
        )
    }

    fn site_name(&self) -> String {
        self.app
            .framework
            .config
            .get(CFG_SITE_NAME)
            .and_then(Json::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&self.app.name)
            .to_owned()
    }

    /// The document around a body (§9): Saltcorn UI's own head — v1's
    /// `wrapper.js`, ported — with the assets a v1 view needs to *work*.
    fn document(&self, title: &str, body: &str) -> String {
        let asset = |file: &str| format!("/static_assets/{ASSET_VERSION_TAG}/{file}");
        format!(
            "<!doctype html>\n\
             <html lang=\"en\" data-bs-theme=\"light\">\n\
             <head>\n\
             <meta charset=\"utf-8\">\n\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1, shrink-to-fit=no\">\n\
             <link rel=\"stylesheet\" href=\"{bootstrap_css}\">\n\
             <link rel=\"stylesheet\" href=\"{fontawesome}\">\n\
             <link rel=\"stylesheet\" href=\"{saltcorn_css}\">\n\
             <script src=\"{jquery}\"></script>\n\
             <script src=\"{bootstrap_js}\"></script>\n\
             <script src=\"{common}\"></script>\n\
             <script src=\"{saltcorn}\"></script>\n\
             <script>var _sc_globalCsrf = \"\"; var _sc_version_tag = \"{ASSET_VERSION_TAG}\"; \
             var _sc_pageloadtag = \"\"; var _sc_loglevel = 1; var _sc_lightmode = \"light\";</script>\n\
             <title>{title}</title>\n\
             </head>\n\
             <body id=\"page-top\">\n\
             <div id=\"wrapper\">{body}</div>\n\
             </body>\n\
             </html>\n",
            bootstrap_css = asset("bootstrap.min.css"),
            fontawesome = asset("fontawesome-free/css/all.min.css"),
            saltcorn_css = asset("saltcorn.css"),
            jquery = asset("jquery-3.6.0.min.js"),
            bootstrap_js = asset("bootstrap.bundle.min.js"),
            common = asset("saltcorn-common.js"),
            saltcorn = asset("saltcorn.js"),
            title = escape(title),
        )
    }
}

/// The surfaces one render reaches, on the viewer's terms — built the way
/// `sc_core_actions::code_body::Hosts` builds a code body's from its event, with
/// the request's user as the caller. No `fetch`: the view runtime is granted no
/// network.
struct ViewerHosts<'a> {
    table: TableHost<'a>,
    files: FileStoreHost<'a>,
    triggers: Option<TriggerRunHost<'a>>,
    module_fns: Option<Arc<dyn ModuleFnHost>>,
}

impl<'a> ViewerHosts<'a> {
    fn new(
        cat: &'a Catalog,
        user: Option<&User>,
        evaluator: Option<Arc<dyn JsEvaluator>>,
        triggers: Option<&'a TriggerDispatcher>,
    ) -> ViewerHosts<'a> {
        let caller = sc_api::caller_context(user);
        ViewerHosts {
            table: TableHost::new(cat)
                .caused_by(caller.role, caller.user.clone())
                .with_evaluator(evaluator),
            files: FileStoreHost::new(cat).caused_by(caller.role),
            triggers: triggers
                .map(|d| TriggerRunHost::new(d, cat).caused_by(caller.role, caller.user.clone())),
            module_fns: cat.module_functions(),
        }
    }

    fn surfaces(&'a self) -> CodeHosts<'a> {
        CodeHosts {
            host: Some(&self.table),
            fetch: None,
            files: Some(&self.files),
            triggers: self.triggers.as_ref().map(|t| t as &dyn TriggerHost),
            module_fns: self.module_fns.as_deref(),
        }
    }
}

/// v1's `req.user`.
fn view_user(user: &User) -> ViewUser {
    ViewUser {
        id: user.id.to_string(),
        email: match user.extra.get("email") {
            Some(sc_query::Value::Text(email)) => email.clone(),
            _ => String::new(),
        },
        role_id: user.role,
    }
}

fn role_of(req: &AppRequest) -> u8 {
    req.user.as_ref().map_or(ROLE_PUBLIC, |u| u.role)
}

fn is_xhr(req: &AppRequest) -> bool {
    req.header("x-requested-with")
        .is_some_and(|v| v.eq_ignore_ascii_case("XMLHttpRequest"))
}

/// A view's state: the query, and the slug's parts under the fields v1's
/// `slug.steps` name, in order.
fn state_of(req: &AppRequest, view: &View, slug: &[&str]) -> Json {
    let mut state: Map<String, Json> = req
        .query
        .iter()
        .map(|(k, v)| (k.clone(), Json::String(v.clone())))
        .collect();
    let steps = view
        .slug
        .as_ref()
        .and_then(|s| s.get("steps"))
        .and_then(Json::as_array);
    for (step, part) in steps.into_iter().flatten().zip(slug) {
        if let Some(field) = step.get("field").and_then(Json::as_str) {
            state.insert(field.to_owned(), Json::String(percent_decode(part)));
        }
    }
    Json::Object(state)
}

/// Whether decoded path parts stay inside the directory they are joined onto.
fn confined(parts: &[String]) -> bool {
    parts
        .iter()
        .all(|p| !p.is_empty() && p != "." && p != ".." && !p.contains(['/', '\\', '\0']))
}

/// Decode a path segment's `%XX` escapes, as UTF-8. A malformed escape stays as
/// it is; `+` is a plus (it is a space only in a query).
pub(crate) fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_owned();
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Some(byte) = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Encode a name as a path segment, as v1's `encodeURIComponent` does.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(char::from(byte)),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Escape text for HTML content and attribute values.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn poisoned() -> Error {
    Error::msg("the Saltcorn UI snapshot lock is poisoned")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use sc_app::FrameworkRef;
    use serde_json::json;

    #[test]
    fn the_csp_widens_script_src_and_nothing_else() {
        let csp = saltcorn_ui_csp().header_value();
        assert!(csp.contains("default-src 'self'"), "{csp}");
        assert!(csp.contains("script-src 'self' 'unsafe-inline'"), "{csp}");
        assert!(!csp.contains("unsafe-eval"), "{csp}");
        assert!(!csp.contains("blob:"), "{csp}");
        assert_eq!(csp.matches("unsafe-inline").count(), 1, "{csp}");
    }

    #[test]
    fn the_settings_validate_as_every_frameworks_do() {
        let spec = saltcorn_ui_config_spec();
        let names: Vec<&str> = spec.iter().map(|f| f.name()).collect();
        assert_eq!(
            &names[..3],
            [CFG_SITE_NAME, MENU_CONFIG_KEY, CFG_ROOT_PAGES]
        );
        assert!(spec.iter().all(|f| !f.base.label.is_empty()));
        for derived in DERIVED_CONFIG_KEYS {
            assert!(!names.contains(&derived), "{derived} is not a setting");
        }

        let fw = FrameworkRef::new(SALTCORN_UI_FRAMEWORK)
            .with(CFG_SITE_NAME, "Books")
            .with(
                MENU_CONFIG_KEY,
                json!([{ "type": "View", "label": "Books", "viewname": "List Books" }]),
            )
            .with(CFG_ROOT_PAGES, json!({ "1": "Dashboard" }));
        sc_types::validate_attrs(&spec, &fw.config).unwrap();
        check_saltcorn_ui_config(&fw.config).unwrap();

        let bad_menu = fw
            .clone()
            .with(MENU_CONFIG_KEY, json!([{ "label": "Books" }]));
        let msg = check_saltcorn_ui_config(&bad_menu.config)
            .unwrap_err()
            .to_string();
        assert!(msg.contains("menu_items[0]"), "{msg}");
        let bad_nesting = fw.clone().with(
            MENU_CONFIG_KEY,
            json!([{ "type": "Header", "subitems": [{}] }]),
        );
        let msg = check_saltcorn_ui_config(&bad_nesting.config)
            .unwrap_err()
            .to_string();
        assert!(msg.contains("menu_items[0].subitems[0]"), "{msg}");
        let bad_roots = fw.with(CFG_ROOT_PAGES, json!({ "admin": "Dashboard" }));
        let msg = check_saltcorn_ui_config(&bad_roots.config)
            .unwrap_err()
            .to_string();
        assert!(msg.contains(CFG_ROOT_PAGES), "{msg}");
    }

    #[test]
    fn the_factory_is_in_the_registry_once_installed() {
        install_saltcorn_ui().unwrap();
        install_saltcorn_ui().unwrap();
        let listed: Vec<String> = sc_app::registered_frameworks();
        assert_eq!(
            listed
                .iter()
                .filter(|n| *n == SALTCORN_UI_FRAMEWORK)
                .count(),
            1,
            "{listed:?}"
        );
        assert_eq!(
            sc_app::framework_default_csp(SALTCORN_UI_FRAMEWORK),
            saltcorn_ui_csp()
        );
        assert_eq!(
            sc_app::framework_config_spec(SALTCORN_UI_FRAMEWORK).unwrap(),
            saltcorn_ui_config_spec()
        );
        assert!(sc_app::framework_serves_ui(SALTCORN_UI_FRAMEWORK));
        // There is no source tree to code in, so there is no builder agent.
        let app = Application::new("Books", "books", FrameworkRef::new(SALTCORN_UI_FRAMEWORK));
        assert!(sc_app::framework_builder_agent(&app.framework, &app).is_none());
        // And nothing to build from a store.
        assert!(sc_app::app_source_from_config(&app.framework).is_err());
        // The structural check runs the framework's own.
        let bad = FrameworkRef::new(SALTCORN_UI_FRAMEWORK).with(MENU_CONFIG_KEY, json!({}));
        assert!(sc_app::validate_framework_config_structure(&bad).is_err());
    }

    #[test]
    fn a_slug_fills_the_fields_its_steps_name() {
        let app = sc_app::AppId::new();
        let mut view = View::new(app, "Show Book", "Show", "books");
        view.slug = Some(json!({ "label": "", "steps": [{ "field": "title", "unique": true }] }));
        let mut req = AppRequest::get("/view/Show%20Book/Dune%20Messiah");
        req.query.insert("id".to_owned(), "3".to_owned());
        assert_eq!(
            state_of(&req, &view, &["Dune%20Messiah", "extra"]),
            json!({ "id": "3", "title": "Dune Messiah" })
        );
    }

    #[test]
    fn paths_are_decoded_and_confined() {
        assert_eq!(percent_decode("List%20Books"), "List Books");
        assert_eq!(percent_decode("caf%C3%A9+%zz"), "café+%zz");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_encode("List Books/é"), "List%20Books%2F%C3%A9");
        assert!(confined(&["css".into(), "all.min.css".into()]));
        for bad in ["..", ".", "", "a\\b", "a/b"] {
            assert!(!confined(&["x".into(), bad.to_owned()]), "{bad:?}");
        }
    }
}
