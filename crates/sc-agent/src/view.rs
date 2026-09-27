//! Looking at an application: the preview mount and the browser (TODO §7b).
//!
//! An agent that edits an application can build it (`check`), but a build
//! changes nothing that is served, and nothing inside the server gives a run a
//! browser. Both belong to the server layer: a preview is a second mount in
//! `sc-server`'s registry, and the browser is a headless Chromium it drives. A
//! trait in `sc-core-traits` can name neither, so this module is the seam, in
//! the shape the evaluator and the dispatcher already have on
//! [`TraitContext`](crate::TraitContext): a capability the deployment has or has
//! not got, reached through `require_*`, whose absence is a configuration error
//! naming what is missing.
//!
//! - [`AppPreviewer`] mounts, re-mounts and unmounts a run's preview of one
//!   application.
//! - [`BrowserDriver`] performs one [`BrowserAction`] in the run's own browser
//!   context, as a session for the run's caller, and reports what the page is
//!   now.
//! - [`AppRequester`] sends one HTTP request to an application through the
//!   server's own router, as a session for a user or as nobody, and hands back
//!   the response — `call_api`'s way of seeing what an endpoint answers.
//! - [`HostCapabilities`] is what the server found on its host at boot, so a
//!   trait's configuration check can refuse a grant the host cannot honour.
//! - [`ViewServices`] holds the three capabilities once the server has built them.
//!   The registry carries it, because the server builds its mounts after the
//!   agents and every runner — a chat turn, a trigger, a delegated child — is
//!   made from the registry.
//!
//! When a drive of a run stops, the driver unmounts the run's previews and
//! closes its browser context (and with it the session), whatever the reason.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use sc_auth::User;
use sc_error::Result;

use crate::run::RunId;

/// What the server found on its host at boot that a trait's configuration may
/// depend on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCapabilities {
    /// The headless browser `view_app` drives, or why there is none.
    pub browser: std::result::Result<PathBuf, String>,
}

impl Default for HostCapabilities {
    /// Nothing detected: a registry built outside a server (a test, a tool) has
    /// not looked.
    fn default() -> Self {
        HostCapabilities {
            browser: Err("this process did not look for a browser".to_owned()),
        }
    }
}

impl HostCapabilities {
    /// A host with the browser at `path`.
    pub fn with_browser(path: impl Into<PathBuf>) -> HostCapabilities {
        HostCapabilities {
            browser: Ok(path.into()),
        }
    }
}

/// One run's preview of one application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewInfo {
    /// The application's subdomain.
    pub subdomain: String,
    /// The random label that owns the preview host.
    pub label: String,
    /// The preview's host name, `<label>--<subdomain>.<base-domain>`.
    pub host: String,
}

/// Mounts a run's preview of an application (TODO 6b.3).
#[async_trait::async_trait]
pub trait AppPreviewer: Send + Sync {
    /// Mount `run`'s preview of `subdomain`, serving the bundle a green build
    /// just wrote to `output_dir`, or re-mount it under the label it already
    /// has. The live mount is not touched. An application with nothing to build
    /// is previewed as it is served, and `output_dir` is not read.
    async fn mount_preview(
        &self,
        run: RunId,
        subdomain: &str,
        output_dir: &Path,
    ) -> Result<PreviewInfo>;

    /// `run`'s preview of `subdomain`, if one is mounted. Counts as use, for the
    /// idle sweep.
    fn preview(&self, run: RunId, subdomain: &str) -> Option<PreviewInfo>;

    /// Unmount every preview `run` owns.
    fn unmount_previews(&self, run: RunId);
}

/// One thing to do in the page.
#[derive(Debug, Clone, PartialEq)]
pub enum BrowserAction {
    /// Open a path on the preview host.
    Goto { path: String },
    /// Click the element a snapshot named `@eN`.
    Click { reference: String },
    /// Type into the element a snapshot named, replacing its value.
    Fill { reference: String, text: String },
    /// Press a key (`Enter`, `Tab`, `Escape`, …) in the focused element.
    Press { key: String },
    /// Wait until the text appears in the page, or the element exists.
    WaitFor {
        text: Option<String>,
        reference: Option<String>,
        timeout: Duration,
    },
    /// Only look.
    Snapshot,
    /// A JPEG of the viewport, or of the whole page.
    Screenshot { full_page: bool },
}

impl BrowserAction {
    /// The action's name, as the tool spells it.
    pub fn name(&self) -> &'static str {
        match self {
            BrowserAction::Goto { .. } => "goto",
            BrowserAction::Click { .. } => "click",
            BrowserAction::Fill { .. } => "fill",
            BrowserAction::Press { .. } => "press",
            BrowserAction::WaitFor { .. } => "wait_for",
            BrowserAction::Snapshot => "snapshot",
            BrowserAction::Screenshot { .. } => "screenshot",
        }
    }
}

/// One call on a run's browser context.
pub struct BrowserRequest<'a> {
    /// The run whose context this is. A first request creates it.
    pub run: RunId,
    /// The preview the page belongs to. Navigation anywhere else is refused.
    pub preview: &'a PreviewInfo,
    /// Whom the context's session is for: the run's caller, or the configured
    /// user of a run nobody is present for.
    pub user: &'a User,
    /// What to do.
    pub action: BrowserAction,
    /// How long the whole call may take, including waiting for a context.
    pub timeout: Duration,
}

/// What the page is after an action.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BrowserReport {
    /// The page's URL.
    pub url: String,
    /// The HTTP status of the last document loaded, when known.
    pub status: Option<u16>,
    /// The accessibility snapshot, with refs on interactive elements. `None`
    /// for a screenshot.
    pub snapshot: Option<String>,
    /// The JPEG, for a screenshot.
    pub screenshot: Option<Vec<u8>>,
    /// Console errors and uncaught exceptions since the previous call.
    pub console_errors: Vec<String>,
    /// Requests that failed or answered 4xx/5xx since the previous call.
    pub failed_requests: Vec<String>,
    /// Something about the action the model should know: a `wait_for` that
    /// timed out, a navigation that was refused.
    pub note: Option<String>,
}

/// Drives the headless browser (TODO 6b.6).
#[async_trait::async_trait]
pub trait BrowserDriver: Send + Sync {
    /// Perform one action in the request's run context, creating the context
    /// and its session on first use, and report the page.
    async fn act(&self, request: BrowserRequest<'_>) -> Result<BrowserReport>;

    /// Close `run`'s context and delete its session. Synchronous, because it is
    /// called from the driver's drop guard: start the release and return.
    fn close(&self, run: RunId);
}

/// One HTTP request to an application (`call_api`).
#[derive(Debug, Clone)]
pub struct AppHttpRequest<'a> {
    /// The application's subdomain. The request goes to its live mount.
    pub subdomain: &'a str,
    /// The method, upper case.
    pub method: String,
    /// The path, with any query string.
    pub path: String,
    /// Headers besides the ones the requester sets itself (`Host`, `Cookie`
    /// and the CSRF header).
    pub headers: Vec<(String, String)>,
    /// The body; empty for none.
    pub body: Vec<u8>,
    /// Whom a session is made for, or `None` for a request with no session —
    /// what an anonymous visitor sends.
    pub user: Option<&'a User>,
    /// How long the whole request may take, reading the body included.
    pub timeout: Duration,
}

/// What an application answered.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppHttpResponse {
    /// The status code.
    pub status: u16,
    /// The response headers, in order, names lower case.
    pub headers: Vec<(String, String)>,
    /// The body, up to the requester's cap.
    pub body: Vec<u8>,
    /// Why the body is incomplete — over the cap, or still streaming when the
    /// time ran out — if it is.
    pub truncated: Option<String>,
}

/// Sends a request to an application as its own client would.
#[async_trait::async_trait]
pub trait AppRequester: Send + Sync {
    /// Send `request` to the application's live mount, with a session for
    /// `request.user` made for this one request and ended after it, and a
    /// valid CSRF token, so the answer is the one the application's own page
    /// would get.
    async fn request(&self, request: AppHttpRequest<'_>) -> Result<AppHttpResponse>;
}

/// The server's preview, browser and request capabilities, once it has built
/// them.
///
/// Late-bound, because the server builds the mount registry after the agents.
/// Each slot is set once; a second set is ignored.
#[derive(Default)]
pub struct ViewServices {
    previews: OnceLock<Arc<dyn AppPreviewer>>,
    browser: OnceLock<Arc<dyn BrowserDriver>>,
    requests: OnceLock<Arc<dyn AppRequester>>,
}

impl ViewServices {
    /// Install the previewer.
    pub fn set_previews(&self, previews: Arc<dyn AppPreviewer>) {
        let _ = self.previews.set(previews);
    }

    /// Install the browser driver.
    pub fn set_browser(&self, browser: Arc<dyn BrowserDriver>) {
        let _ = self.browser.set(browser);
    }

    /// Install the application requester.
    pub fn set_requests(&self, requests: Arc<dyn AppRequester>) {
        let _ = self.requests.set(requests);
    }

    /// The previewer, if installed.
    pub fn previews(&self) -> Option<&Arc<dyn AppPreviewer>> {
        self.previews.get()
    }

    /// The browser driver, if installed.
    pub fn browser(&self) -> Option<&Arc<dyn BrowserDriver>> {
        self.browser.get()
    }

    /// The application requester, if installed.
    pub fn requests(&self) -> Option<&Arc<dyn AppRequester>> {
        self.requests.get()
    }
}

impl std::fmt::Debug for ViewServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewServices")
            .field("previews", &self.previews.get().is_some())
            .field("browser", &self.browser.get().is_some())
            .field("requests", &self.requests.get().is_some())
            .finish()
    }
}
