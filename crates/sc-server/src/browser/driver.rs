//! The headless browser `view_app` drives (TODO 6b.6, 6b.7).
//!
//! **One Chromium for the whole server**, started on the first call and again
//! if it has died, with a **fresh browser context per run**: its own cookies and
//! storage, closed when the run's drive stops. At most `--browser-contexts` runs
//! hold one at a time; a call beyond that waits, within its timeout.
//!
//! **How the browser reaches a preview.** Chromium starts with
//! `--host-resolver-rules` mapping the base domain and every name under it to
//! the loopback address, and **every other name to "not found"**, so a page can
//! reach this server and nothing else: `view_app` is a view of this application,
//! not a way to browse the internet from the server. The port is a listener of
//! its own that `serve` binds on the loopback address only, serving the same
//! router in plain HTTP with non-`Secure` cookies (see
//! [`crate::serve`](crate::serve())). So no certificate has to be trusted,
//! however the public listener is set up. Every navigation is checked as well:
//! `goto` takes only a path, and a page that leaves the preview host is taken
//! back, with the attempt reported.
//!
//! **The session is the caller's.** The first call of a run creates a session
//! for the user the request names ([`SessionStore::login`], the
//! `sc_auth::create_session` row plus this node's cache), lets it reach the
//! run's previews, and puts its cookie straight into the run's context. Nothing
//! is written to disk. Closing the context logs the session out.
//!
//! **What changed since the last call** — console errors, uncaught exceptions,
//! failed requests and 4xx/5xx responses — is collected by listeners on the
//! page and handed back with the next report, so a white screen caused by a
//! thrown error is a line of text.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chromiumoxide::cdp::browser_protocol::accessibility::GetFullAxTreeParams;
use chromiumoxide::cdp::browser_protocol::browser::BrowserContextId;
use chromiumoxide::cdp::browser_protocol::dom::{
    BackendNodeId, GetBoxModelParams, ResolveNodeParams, ScrollIntoViewIfNeededParams,
};
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, InsertTextParams,
};
use chromiumoxide::cdp::browser_protocol::network::{
    CookieParam, CookieSameSite, EventLoadingFailed, EventRequestWillBeSent, EventResponseReceived,
    ResourceType,
};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::cdp::browser_protocol::target::{
    CreateBrowserContextParams, CreateTargetParams,
};
use chromiumoxide::cdp::js_protocol::runtime::{
    CallFunctionOnParams, ConsoleApiCalledType, EventConsoleApiCalled, EventExceptionThrown,
    RemoteObject,
};
use chromiumoxide::handler::viewport::Viewport;
use chromiumoxide::layout::Point;
use chromiumoxide::page::ScreenshotParams;
use chromiumoxide::{Browser, BrowserConfig, Page};
use futures::StreamExt;
use sc_agent::{BrowserAction, BrowserDriver, BrowserReport, BrowserRequest, RunId};
use sc_auth::SessionStore;
use sc_error::{Error, Result};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::snapshot::{AxNode, render};
use crate::apps::AppMounts;
use crate::security::SESSION_COOKIE;

/// The viewport every page gets.
const VIEWPORT: (u32, u32) = (1280, 800);

/// A screenshot larger than this is taken again at a lower quality.
pub(crate) const MAX_SCREENSHOT_BYTES: usize = 1_500_000;

/// The most console errors, and the most failed requests, one report carries.
const MAX_EVENTS: usize = 10;

/// How a [`ChromiumDriver`] starts its browser.
#[derive(Debug, Clone)]
pub struct DriverConfig {
    /// The Chromium binary.
    pub executable: PathBuf,
    /// Whether it keeps its sandbox.
    pub sandbox: bool,
    /// How many runs may hold a context at once.
    pub contexts: usize,
    /// The domain applications are served under.
    pub base_domain: String,
    /// The loopback port the browser's listener is bound to.
    pub port: u16,
}

/// The browser process, the task pumping its protocol connection, and the
/// watchdog that cleans up after it.
struct Launched {
    browser: Browser,
    handler: tokio::task::JoinHandle<()>,
    /// Kills the browser if this process dies without dropping it, and removes
    /// its profile once it has gone either way.
    watchdog: Option<std::process::Child>,
}

impl Drop for Launched {
    fn drop(&mut self) {
        self.handler.abort();
        // The browser is `kill_on_drop` and goes with this value; the watchdog
        // sees it go, removes the profile and exits. Reaped off this thread.
        if let Some(mut watchdog) = self.watchdog.take() {
            std::thread::spawn(move || watchdog.wait());
        }
    }
}

/// What the page's listeners saw since the last report.
#[derive(Default)]
struct Seen {
    console_errors: Vec<String>,
    failed_requests: Vec<String>,
    /// The last document response: its URL and status.
    document: Option<(String, u16)>,
    /// Request URLs by id, for a failure that names only the id.
    requests: HashMap<String, String>,
}

impl Seen {
    fn push(list: &mut Vec<String>, line: String) {
        if list.len() < MAX_EVENTS {
            list.push(line);
        } else if list.len() == MAX_EVENTS {
            list.push("[more not shown]".to_owned());
        }
    }
}

/// One run's context: its page, its session, and what its listeners saw.
struct RunPage {
    context: BrowserContextId,
    page: Page,
    session: String,
    /// The origins the session cookie has been set for.
    origins: Vec<String>,
    /// The DOM node each ref of the last snapshot names.
    refs: HashMap<String, i64>,
    seen: Arc<Mutex<Seen>>,
    listeners: Vec<tokio::task::JoinHandle<()>>,
    _permit: OwnedSemaphorePermit,
}

impl Drop for RunPage {
    fn drop(&mut self) {
        for listener in &self.listeners {
            listener.abort();
        }
    }
}

/// The server's headless Chromium.
pub struct ChromiumDriver {
    config: DriverConfig,
    sessions: Arc<SessionStore>,
    apps: Arc<AppMounts>,
    browser: Arc<tokio::sync::Mutex<Option<Launched>>>,
    runs: Mutex<HashMap<RunId, Arc<tokio::sync::Mutex<RunPage>>>>,
    permits: Arc<Semaphore>,
}

impl ChromiumDriver {
    /// A driver that starts `config`'s browser on first use, creating sessions
    /// in `sessions` and letting them reach `apps`' previews.
    pub fn new(
        config: DriverConfig,
        sessions: Arc<SessionStore>,
        apps: Arc<AppMounts>,
    ) -> ChromiumDriver {
        let permits = Arc::new(Semaphore::new(config.contexts.max(1)));
        ChromiumDriver {
            config,
            sessions,
            apps,
            browser: Arc::new(tokio::sync::Mutex::new(None)),
            runs: Mutex::new(HashMap::new()),
            permits,
        }
    }

    /// The origin a preview host is reached at through the loopback listener.
    pub fn origin(&self, host: &str) -> String {
        format!("http://{host}:{}", self.config.port)
    }

    /// How many runs hold a context now.
    pub fn open_contexts(&self) -> usize {
        self.runs_lock().len()
    }

    /// Stop the browser, if it is running. For the server's shutdown.
    pub fn shutdown(&self) {
        if let Ok(mut browser) = self.browser.try_lock() {
            // `kill_on_drop` ends the process.
            browser.take();
        }
    }

    /// The arguments the browser is started with.
    fn launch_config(&self, profile: &std::path::Path) -> Result<BrowserConfig> {
        let base = &self.config.base_domain;
        let rules = format!("MAP *.{base} 127.0.0.1, MAP {base} 127.0.0.1, MAP * ~NOTFOUND");
        let mut builder = BrowserConfig::builder()
            .chrome_executable(&self.config.executable)
            .respect_https_errors()
            .window_size(VIEWPORT.0, VIEWPORT.1)
            .viewport(Viewport {
                width: VIEWPORT.0,
                height: VIEWPORT.1,
                ..Viewport::default()
            })
            .request_timeout(Duration::from_secs(30))
            .launch_timeout(Duration::from_secs(30))
            .user_data_dir(profile)
            .arg(("host-resolver-rules", rules.as_str()))
            .arg("no-proxy-server")
            .arg("disable-gpu");
        if !self.config.sandbox {
            builder = builder.no_sandbox();
        }
        builder
            .build()
            .map_err(|e| Error::config(format!("the browser could not be configured: {e}")))
    }

    /// The browser, started if it is not running.
    async fn launched(&self) -> Result<tokio::sync::MutexGuard<'_, Option<Launched>>> {
        let mut guard = self.browser.lock().await;
        let dead = match guard.as_mut() {
            Some(launched) => launched.handler.is_finished(),
            None => true,
        };
        if dead {
            guard.take();
            // A profile of its own, gone with the process: nothing the browser
            // stores, cookies included, outlives it.
            let profile = std::env::temp_dir().join(format!(
                "feldspar-browser-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple()
            ));
            let config = self.launch_config(&profile)?;
            let (mut browser, mut handler) = Browser::launch(config).await.map_err(|e| {
                Error::config(format!(
                    "the browser at `{}` did not start: {e}",
                    self.config.executable.display()
                ))
            })?;
            let handler = tokio::spawn(async move {
                while let Some(event) = handler.next().await {
                    if event.is_err() {
                        break;
                    }
                }
            });
            sc_log::log_info!(
                "started the browser at {} for view_app",
                self.config.executable.display()
            );
            let watchdog = browser
                .get_mut_child()
                .and_then(|child| child.inner.id())
                .and_then(|pid| watchdog(pid, &profile));
            *guard = Some(Launched {
                browser,
                handler,
                watchdog,
            });
        }
        Ok(guard)
    }

    fn runs_lock(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<RunId, Arc<tokio::sync::Mutex<RunPage>>>> {
        self.runs.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The run's page, creating its context and session on first use.
    async fn run_page(
        &self,
        request: &BrowserRequest<'_>,
        deadline: Instant,
    ) -> Result<Arc<tokio::sync::Mutex<RunPage>>> {
        if let Some(page) = self.runs_lock().get(&request.run) {
            return Ok(page.clone());
        }
        let left = deadline.saturating_duration_since(Instant::now());
        let permit = tokio::time::timeout(left, self.permits.clone().acquire_owned())
            .await
            .map_err(|_| {
                Error::invalid(format!(
                    "every browser context ({}) is in use by other runs; try again shortly",
                    self.config.contexts
                ))
            })?
            .map_err(|_| Error::msg("the browser is shutting down"))?;

        let (context, page) = {
            let guard = self.launched().await?;
            let browser = &guard
                .as_ref()
                .ok_or_else(|| Error::msg("no browser"))?
                .browser;
            let context = browser
                .create_browser_context(CreateBrowserContextParams::default())
                .await
                .map_err(cdp)?;
            let target = CreateTargetParams::builder()
                .url("about:blank")
                .browser_context_id(context.clone())
                .build()
                .map_err(Error::msg)?;
            let page = browser.new_page(target).await.map_err(cdp)?;
            (context, page)
        };

        let session = self.sessions.login(request.user.clone()).await?;
        self.apps.allow_preview_session(request.run, &session);
        let seen = Arc::new(Mutex::new(Seen::default()));
        let listeners = listen(&page, &seen).await?;
        let run_page = Arc::new(tokio::sync::Mutex::new(RunPage {
            context,
            page,
            session,
            origins: Vec::new(),
            refs: HashMap::new(),
            seen,
            listeners,
            _permit: permit,
        }));
        self.runs_lock().insert(request.run, run_page.clone());
        Ok(run_page)
    }

    async fn act_in(
        &self,
        request: &BrowserRequest<'_>,
        deadline: Instant,
    ) -> Result<BrowserReport> {
        let page = self.run_page(request, deadline).await?;
        let mut page = page.lock().await;
        let origin = self.origin(&request.preview.host);
        if !page.origins.contains(&origin) {
            let cookie = CookieParam::builder()
                .name(SESSION_COOKIE)
                .value(page.session.clone())
                .url(origin.clone())
                .http_only(true)
                .same_site(CookieSameSite::Strict)
                .build()
                .map_err(Error::msg)?;
            page.page.set_cookie(cookie).await.map_err(cdp)?;
            page.origins.push(origin.clone());
        }

        let mut note = None;
        let current = page.page.url().await.map_err(cdp)?.unwrap_or_default();
        let on_preview = host_of(&current) == Some(request.preview.host.as_str());
        match &request.action {
            BrowserAction::Goto { path } => {
                let path = preview_path(path)?;
                navigate(&page.page, &format!("{origin}{path}")).await?;
            }
            action => {
                // Everything but `goto` acts on the page as it is; a page that
                // has not been opened on this preview yet is opened at `/`.
                if !on_preview {
                    navigate(&page.page, &format!("{origin}/")).await?;
                }
                match action {
                    BrowserAction::Click { reference } => {
                        let node = node_for(&page.refs, reference)?;
                        click(&page.page, node).await?;
                        settle(&page.page).await;
                    }
                    BrowserAction::Fill { reference, text } => {
                        let node = node_for(&page.refs, reference)?;
                        fill(&page.page, node, text).await?;
                    }
                    BrowserAction::Press { key } => {
                        press(&page.page, key).await?;
                        settle(&page.page).await;
                    }
                    BrowserAction::WaitFor {
                        text,
                        reference,
                        timeout,
                    } => {
                        let until = (Instant::now() + *timeout).min(deadline);
                        let node = reference
                            .as_deref()
                            .map(|r| node_for(&page.refs, r))
                            .transpose()?;
                        if !wait_for(&page.page, text.as_deref(), node, until).await? {
                            note = Some(format!(
                                "wait_for timed out after {:.0} seconds",
                                timeout.as_secs_f64()
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }

        // A page that left the preview is taken back.
        let now = page.page.url().await.map_err(cdp)?.unwrap_or_default();
        if host_of(&now) != Some(request.preview.host.as_str()) {
            navigate(&page.page, &format!("{origin}/")).await?;
            note = Some(format!(
                "the page tried to leave the preview for {now}; view_app only shows this \
                 application, so it was taken back to /"
            ));
        }

        let mut report = BrowserReport {
            url: page
                .page
                .url()
                .await
                .map_err(cdp)?
                .unwrap_or_default()
                .replacen(&format!(":{}", self.config.port), "", 1),
            note,
            ..BrowserReport::default()
        };
        match &request.action {
            BrowserAction::Screenshot { full_page } => {
                report.screenshot = Some(screenshot(&page.page, *full_page).await?);
            }
            _ => {
                let snapshot = snapshot(&page.page).await?;
                page.refs = snapshot.refs;
                report.snapshot = Some(snapshot.text);
            }
        }
        let mut seen = page.seen.lock().unwrap_or_else(|e| e.into_inner());
        report.console_errors = std::mem::take(&mut seen.console_errors);
        report.failed_requests = std::mem::take(&mut seen.failed_requests);
        report.status = seen.document.as_ref().map(|(_, status)| *status);
        Ok(report)
    }
}

#[async_trait::async_trait]
impl BrowserDriver for ChromiumDriver {
    async fn act(&self, request: BrowserRequest<'_>) -> Result<BrowserReport> {
        let deadline = Instant::now() + request.timeout;
        match tokio::time::timeout(request.timeout, self.act_in(&request, deadline)).await {
            Ok(result) => result,
            Err(_) => Err(Error::invalid(format!(
                "`{}` did not finish within {} seconds",
                request.action.name(),
                request.timeout.as_secs()
            ))),
        }
    }

    fn close(&self, run: RunId) {
        let Some(page) = self.runs_lock().remove(&run) else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let sessions = self.sessions.clone();
        let browser = self.browser.clone();
        runtime.spawn(async move {
            let page = page.lock().await;
            if let Err(e) = sessions.logout(&page.session).await {
                sc_log::log_warn!("run {run}: its view_app session could not be deleted: {e}");
            }
            if let Some(launched) = browser.lock().await.as_ref() {
                let _ = launched
                    .browser
                    .dispose_browser_context(page.context.clone())
                    .await;
            }
        });
    }
}

/// A process that kills the browser once this process is gone — however it
/// went — and removes the browser's profile once the browser is gone. A browser
/// outliving a killed server would otherwise hold its memory, and its profile,
/// until the next reboot.
#[cfg(unix)]
fn watchdog(browser: u32, profile: &std::path::Path) -> Option<std::process::Child> {
    let script = format!(
        "while kill -0 {server} 2>/dev/null && kill -0 {browser} 2>/dev/null; do sleep 1; done; \
         kill -9 {browser} 2>/dev/null; sleep 1; rm -rf '{profile}'",
        server = std::process::id(),
        profile = profile.display().to_string().replace('\'', ""),
    );
    std::process::Command::new("sh")
        .args(["-c", &script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()
}

#[cfg(not(unix))]
fn watchdog(_browser: u32, _profile: &std::path::Path) -> Option<std::process::Child> {
    None
}

/// A protocol failure, as an error the model can read.
fn cdp(e: chromiumoxide::error::CdpError) -> Error {
    Error::msg(format!("the browser reported: {e}"))
}

/// The host of a URL, without its port.
fn host_of(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    Some(authority.split(':').next().unwrap_or(authority))
}

/// A `goto` target: a path on the preview, and nothing else.
fn preview_path(path: &str) -> Result<String> {
    let path = path.trim();
    if path.contains("://") || path.starts_with("//") || !path.starts_with('/') {
        return Err(Error::invalid(format!(
            "`goto` takes a path on this application, starting with `/`, not `{path}`; \
             view_app shows only this application"
        )));
    }
    Ok(path.to_owned())
}

/// The DOM node a ref of the last snapshot names.
fn node_for(refs: &HashMap<String, i64>, reference: &str) -> Result<i64> {
    let reference = reference.trim();
    let key = if reference.starts_with('@') {
        reference.to_owned()
    } else {
        format!("@{reference}")
    };
    refs.get(&key).copied().ok_or_else(|| {
        Error::invalid(format!(
            "`{reference}` is not a ref in the last snapshot; take a snapshot and use one of \
             the @e refs it shows"
        ))
    })
}

/// Open `url` and wait for it to load.
async fn navigate(page: &Page, url: &str) -> Result<()> {
    page.goto(url).await.map_err(cdp)?;
    settle(page).await;
    Ok(())
}

/// Give an action's consequences a moment: a navigation it started, a render.
async fn settle(page: &Page) {
    tokio::time::sleep(Duration::from_millis(250)).await;
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        let ready = page
            .evaluate("document.readyState")
            .await
            .ok()
            .and_then(|r| r.into_value::<String>().ok());
        if ready.as_deref() == Some("complete") {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Click the middle of a node, scrolled into view.
async fn click(page: &Page, node: i64) -> Result<()> {
    let id = BackendNodeId::new(node);
    page.execute(
        ScrollIntoViewIfNeededParams::builder()
            .backend_node_id(id)
            .build(),
    )
    .await
    .map_err(stale)?;
    let model = page
        .execute(GetBoxModelParams::builder().backend_node_id(id).build())
        .await
        .map_err(stale)?
        .result
        .model;
    let quad = model.content.inner();
    let (x, y) = (
        (quad[0] + quad[2] + quad[4] + quad[6]) / 4.0,
        (quad[1] + quad[3] + quad[5] + quad[7]) / 4.0,
    );
    page.click(Point { x, y }).await.map_err(cdp)?;
    Ok(())
}

/// Replace a field's value with `text`, as typing would.
async fn fill(page: &Page, node: i64, text: &str) -> Result<()> {
    let object = page
        .execute(
            ResolveNodeParams::builder()
                .backend_node_id(BackendNodeId::new(node))
                .build(),
        )
        .await
        .map_err(stale)?
        .result
        .object;
    let object_id = object
        .object_id
        .ok_or_else(|| Error::invalid("that element cannot be filled"))?;
    // Clear through the native setter, so a framework's controlled input sees
    // the change, then type.
    let clear = CallFunctionOnParams::builder()
        .object_id(object_id)
        .function_declaration(
            "function() { this.focus(); \
               const proto = Object.getPrototypeOf(this); \
               const desc = Object.getOwnPropertyDescriptor(proto, 'value'); \
               if (desc && desc.set) { desc.set.call(this, ''); } \
               else if (this.isContentEditable) { this.textContent = ''; } \
               this.dispatchEvent(new Event('input', { bubbles: true })); }",
        )
        .build()
        .map_err(Error::msg)?;
    page.execute(clear).await.map_err(cdp)?;
    page.execute(InsertTextParams::new(text))
        .await
        .map_err(cdp)?;
    Ok(())
}

/// Press one key in the focused element.
async fn press(page: &Page, key: &str) -> Result<()> {
    let definition = chromiumoxide::keys::USKEYBOARD_LAYOUT
        .iter()
        .find(|k| k.key.eq_ignore_ascii_case(key) && k.key.len() == key.len())
        .ok_or_else(|| {
            Error::invalid(format!(
                "`{key}` is not a key name; use one like Enter, Tab, Escape, Backspace or \
                 ArrowDown"
            ))
        })?;
    for kind in [DispatchKeyEventType::KeyDown, DispatchKeyEventType::KeyUp] {
        let mut event = DispatchKeyEventParams::builder()
            .r#type(kind.clone())
            .key(definition.key)
            .code(definition.code)
            .windows_virtual_key_code(definition.key_code)
            .native_virtual_key_code(definition.key_code);
        if kind == DispatchKeyEventType::KeyDown
            && let Some(text) = definition.text
        {
            event = event.text(text);
        }
        page.execute(event.build().map_err(Error::msg)?)
            .await
            .map_err(cdp)?;
    }
    Ok(())
}

/// Wait until `text` is in the page or `node` is laid out. `false` on timeout.
async fn wait_for(
    page: &Page,
    text: Option<&str>,
    node: Option<i64>,
    until: Instant,
) -> Result<bool> {
    let probe = text.map(|t| {
        format!(
            "!!(document.body && document.body.innerText.includes({}))",
            serde_json::to_string(t).unwrap_or_default()
        )
    });
    loop {
        let text_ok = match &probe {
            Some(probe) => page
                .evaluate(probe.as_str())
                .await
                .ok()
                .and_then(|r| r.into_value::<bool>().ok())
                .unwrap_or(false),
            None => true,
        };
        let node_ok = match node {
            Some(node) => page
                .execute(
                    GetBoxModelParams::builder()
                        .backend_node_id(BackendNodeId::new(node))
                        .build(),
                )
                .await
                .is_ok(),
            None => true,
        };
        if text_ok && node_ok {
            return Ok(true);
        }
        if Instant::now() >= until {
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// A ref whose element is gone.
fn stale(e: chromiumoxide::error::CdpError) -> Error {
    Error::invalid(format!(
        "that element is no longer on the page ({e}); take a snapshot for fresh refs"
    ))
}

/// The page's accessibility tree, rendered.
async fn snapshot(page: &Page) -> Result<super::snapshot::Snapshot> {
    let tree = page
        .execute(GetFullAxTreeParams::default())
        .await
        .map_err(cdp)?
        .result
        .nodes;
    let json = serde_json::to_value(tree).map_err(|e| Error::msg(e.to_string()))?;
    let nodes: Vec<AxNode> =
        serde_json::from_value(json).map_err(|e| Error::msg(format!("accessibility tree: {e}")))?;
    Ok(render(&nodes))
}

/// A JPEG of the page, capped in size.
async fn screenshot(page: &Page, full_page: bool) -> Result<Vec<u8>> {
    for (quality, full) in [(70, full_page), (40, full_page), (40, false)] {
        let shot = page
            .screenshot(
                ScreenshotParams::builder()
                    .format(CaptureScreenshotFormat::Jpeg)
                    .quality(quality)
                    .full_page(full)
                    .build(),
            )
            .await
            .map_err(cdp)?;
        if shot.len() <= MAX_SCREENSHOT_BYTES {
            return Ok(shot);
        }
    }
    Err(Error::invalid(
        "the page is too large to screenshot; take a snapshot instead",
    ))
}

/// Start collecting the page's console errors and failed requests.
async fn listen(page: &Page, seen: &Arc<Mutex<Seen>>) -> Result<Vec<tokio::task::JoinHandle<()>>> {
    let lock = |seen: &Arc<Mutex<Seen>>| {
        let seen = seen.clone();
        move |f: &mut dyn FnMut(&mut Seen)| f(&mut seen.lock().unwrap_or_else(|e| e.into_inner()))
    };
    let mut tasks = Vec::new();

    let mut console = page
        .event_listener::<EventConsoleApiCalled>()
        .await
        .map_err(cdp)?;
    let with = lock(seen);
    tasks.push(tokio::spawn(async move {
        while let Some(event) = console.next().await {
            if event.r#type != ConsoleApiCalledType::Error {
                continue;
            }
            let text = event
                .args
                .iter()
                .map(describe)
                .collect::<Vec<_>>()
                .join(" ");
            with(&mut |s| Seen::push(&mut s.console_errors, format!("console.error: {text}")));
        }
    }));

    let mut thrown = page
        .event_listener::<EventExceptionThrown>()
        .await
        .map_err(cdp)?;
    let with = lock(seen);
    tasks.push(tokio::spawn(async move {
        while let Some(event) = thrown.next().await {
            let details = &event.exception_details;
            let what = details
                .exception
                .as_ref()
                .and_then(|e| e.description.clone())
                .unwrap_or_else(|| details.text.clone());
            let what = what.lines().next().unwrap_or_default().to_owned();
            let at = details
                .url
                .as_deref()
                .map(|u| format!(" at {}:{}", strip_origin(u), details.line_number + 1))
                .unwrap_or_default();
            with(&mut |s| Seen::push(&mut s.console_errors, format!("uncaught {what}{at}")));
        }
    }));

    let mut sent = page
        .event_listener::<EventRequestWillBeSent>()
        .await
        .map_err(cdp)?;
    let with = lock(seen);
    tasks.push(tokio::spawn(async move {
        while let Some(event) = sent.next().await {
            let (id, url) = (event.request_id.inner().clone(), event.request.url.clone());
            with(&mut |s| {
                if s.requests.len() > 1000 {
                    s.requests.clear();
                }
                s.requests.insert(id.clone(), url.clone());
            });
        }
    }));

    let mut responses = page
        .event_listener::<EventResponseReceived>()
        .await
        .map_err(cdp)?;
    let with = lock(seen);
    tasks.push(tokio::spawn(async move {
        while let Some(event) = responses.next().await {
            let status = u16::try_from(event.response.status).unwrap_or(0);
            let url = event.response.url.clone();
            let document = event.r#type == ResourceType::Document;
            with(&mut |s| {
                if document {
                    s.document = Some((url.clone(), status));
                }
                if status >= 400 {
                    Seen::push(
                        &mut s.failed_requests,
                        format!("{status} {}", strip_origin(&url)),
                    );
                }
            });
        }
    }));

    let mut failures = page
        .event_listener::<EventLoadingFailed>()
        .await
        .map_err(cdp)?;
    let with = lock(seen);
    tasks.push(tokio::spawn(async move {
        while let Some(event) = failures.next().await {
            if event.canceled == Some(true) {
                continue;
            }
            let (id, error) = (event.request_id.inner().clone(), event.error_text.clone());
            with(&mut |s| {
                let url = s.requests.get(&id).cloned().unwrap_or_default();
                Seen::push(
                    &mut s.failed_requests,
                    format!("failed {} ({error})", strip_origin(&url)),
                );
            });
        }
    }));
    Ok(tasks)
}

/// A console argument as text.
fn describe(object: &RemoteObject) -> String {
    match (&object.value, &object.description) {
        (Some(serde_json::Value::String(s)), _) => s.clone(),
        (Some(other), _) => other.to_string(),
        (None, Some(description)) => description.lines().next().unwrap_or_default().to_owned(),
        (None, None) => String::new(),
    }
}

/// A URL with its scheme and host taken off, when it has them.
fn strip_origin(url: &str) -> String {
    match url.split_once("://") {
        Some((_, rest)) => match rest.find('/') {
            Some(i) => rest[i..].to_owned(),
            None => "/".to_owned(),
        },
        None => url.to_owned(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn goto_takes_a_path_and_nothing_else() {
        assert_eq!(preview_path(" /tasks?x=1 ").unwrap(), "/tasks?x=1");
        for bad in [
            "https://evil.example/",
            "//evil.example/",
            "tasks",
            "javascript:alert(1)",
        ] {
            let err = preview_path(bad).unwrap_err().to_string();
            assert!(err.contains("starting with `/`"), "{bad}: {err}");
        }
    }

    #[test]
    fn a_url_is_reduced_to_its_host_and_path() {
        assert_eq!(
            host_of("http://a--b.example.com:4000/x"),
            Some("a--b.example.com")
        );
        assert_eq!(host_of("about:blank"), None);
        assert_eq!(
            strip_origin("http://a.example.com:4000/api/x?y"),
            "/api/x?y"
        );
        assert_eq!(strip_origin("http://a.example.com"), "/");
    }

    #[test]
    fn a_ref_is_looked_up_with_or_without_its_at() {
        let refs: HashMap<String, i64> = [("@e2".to_owned(), 7)].into_iter().collect();
        assert_eq!(node_for(&refs, "@e2").unwrap(), 7);
        assert_eq!(node_for(&refs, "e2").unwrap(), 7);
        assert!(
            node_for(&refs, "@e9")
                .unwrap_err()
                .to_string()
                .contains("take a snapshot")
        );
    }
}
