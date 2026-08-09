//! `saltcorn auth token`: a signed-in browser session, written to a file.
//!
//! An application's screens are behind a sign-in (design §13.3 — a scaffolded
//! project's routes require a user unless they say otherwise), so a screenshot
//! taken by a script is a screenshot of the sign-in page. What the script needs
//! is the cookie a browser would have got by signing in, and what it has is a
//! shell on the machine.
//!
//! **It asks no password, because it is not a caller who should have one.** This
//! command runs where the server runs, from a shell holding the primary
//! database's credentials — the authority that can already read every password
//! hash, rewrite any of them, and grant itself any role. Demanding a user's
//! password on top of that protected nothing and cost the operator a secret to
//! keep, so what it does instead is name a *user* — `--email`, `--admin`, or
//! `--role NAME` — and mint a [one-time grant](sc_auth::create_session_grant) in
//! the database for them.
//!
//! **It still does not forge.** The session cookie the server accepts is the one
//! *it* minted, in a store that lives in its own memory (§7.2), so nothing
//! outside that process can make one and nothing here tries: the grant is
//! presented to the running server, which checks it against its own database and
//! starts an ordinary session — the same session, by the same code, that a
//! sign-in would have started. The session can do exactly what that account can
//! do and no more, so giving an agent a low-privilege account of its own is a
//! real limit and not a gesture.
//!
//! Two files come out, and which one depends on what will read it:
//!
//! - **Playwright's `storageState`** (the default) — a JSON document
//!   `browser.newContext({ storageState })` restores a logged-in browser from.
//! - **A Netscape `cookies.txt`** — what `curl --cookie` and `wget` read.
//!
//! Both are **credentials**, so both are written `0600` and both default names
//! are in the `.gitignore` a scaffolded project ships with.
//!
//! The CSRF dance is why this is not one request: mutating requests are refused
//! unless the `x-csrf-token` header echoes the `sc_csrf` cookie (§7.2's
//! double-submit check), and a first-contact client has neither. So it does what
//! a browser does — one GET to be given the cookie, then the redemption carrying
//! it both ways.

use std::path::{Path, PathBuf};

use sc_auth::{ROLE_ADMIN, Role, User, create_session_grant, list_roles};
use sc_catalog::Catalog;
use sc_error::{Error, Result};
use serde_json::{Value as Json, json};

/// The default file name for [`SessionFormat::Playwright`].
pub const DEFAULT_PLAYWRIGHT_FILE: &str = ".saltcorn-session.json";
/// The default file name for [`SessionFormat::Netscape`].
pub const DEFAULT_NETSCAPE_FILE: &str = ".saltcorn-cookies.txt";

/// The name of the session cookie, of the CSRF cookie and header, and the route
/// a grant is redeemed at with the field it travels in.
///
/// Spelled here rather than imported from `sc-server` because this crate is a
/// *client* of the running server, which may be a different build: what matters
/// is the wire contract, and the wire contract is these five names. The server's
/// own constants are asserted equal to these in a test, so a rename that broke
/// this cannot land quietly.
const SESSION_COOKIE: &str = "sc_session";
const CSRF_COOKIE: &str = "sc_csrf";
const CSRF_HEADER: &str = "x-csrf-token";
const TOKEN_ROUTE: &str = "/auth/token";
const GRANT_FIELD: &str = "grant";

/// Which file to write.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SessionFormat {
    /// Playwright's `storageState` JSON — a browser context restored from a
    /// file. The default: the command exists for screenshots, and this is what
    /// takes them.
    #[default]
    Playwright,
    /// A Netscape cookie file, for `curl --cookie` and friends.
    Netscape,
}

impl SessionFormat {
    /// The format named on the command line.
    pub fn parse(name: &str) -> Result<SessionFormat> {
        match name {
            "playwright" | "json" => Ok(SessionFormat::Playwright),
            "netscape" | "curl" | "txt" => Ok(SessionFormat::Netscape),
            other => Err(Error::config(format!(
                "unknown --format `{other}`; there are `playwright` (a browser's \
                 storageState) and `netscape` (a cookies.txt for curl)"
            ))),
        }
    }

    /// The file name used when `--out` says nothing.
    pub fn default_file(&self) -> &'static str {
        match self {
            SessionFormat::Playwright => DEFAULT_PLAYWRIGHT_FILE,
            SessionFormat::Netscape => DEFAULT_NETSCAPE_FILE,
        }
    }
}

/// Where to ask for the session: the origin to connect to and the host the
/// application answers on.
///
/// The connection target and the host are separate because they routinely
/// differ. A server routes an application by the request's `Host` header
/// (§13.2), and a development machine that has no DNS for `blog.example.com`
/// can still reach it by connecting to the loopback and *saying* that name —
/// which is exactly what `--url` is for.
///
/// The request goes to the application's own host even though the route that
/// answers it is the server's rather than the app's ([`TOKEN_ROUTE`] is a fixed
/// route, ahead of the host-routed fallback). That is not incidental: the
/// cookies come back scoped to the host that will use them, and the one thing
/// this command must not do is write a session file for the wrong origin.
#[derive(Debug, Clone)]
pub struct Target {
    /// The origin to connect to, e.g. `http://127.0.0.1:3000`.
    pub url: String,
    /// The `Host` header to send, e.g. `blog.example.com`.
    pub host: String,
    /// Whether the cookies should be marked `secure` — i.e. whether the browser
    /// that will use them is talking to an `https` origin.
    pub secure: bool,
}

impl Target {
    /// The URL a browser should open, which is the app's own host rather than
    /// whatever `--url` connected to.
    pub fn browser_url(&self) -> String {
        let scheme = if self.secure { "https" } else { "http" };
        match port_of(&self.url) {
            Some(port) => format!("{scheme}://{}:{port}", self.host),
            None => format!("{scheme}://{}", self.host),
        }
    }
}

/// The port in an origin like `http://127.0.0.1:3000`, when it carries one.
fn port_of(url: &str) -> Option<u16> {
    url.rsplit(':').next()?.trim_end_matches('/').parse().ok()
}

/// One cookie the server set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    /// The cookie's name.
    pub name: String,
    /// Its value — the secret, when the name is `sc_session`.
    pub value: String,
    /// Its path, defaulting to `/`.
    pub path: String,
    /// Whether it carries `HttpOnly`.
    pub http_only: bool,
    /// Whether it carries `Secure`.
    pub secure: bool,
    /// Its `SameSite`, as the server spelled it.
    pub same_site: String,
}

/// Parse one `Set-Cookie` header value.
///
/// Attributes not listed here (`Max-Age`, `Expires`, `Domain`) are ignored
/// deliberately: the server sets none of them — its cookies are host-only
/// session cookies whose lifetime is the *server-side* session's — so parsing
/// them would be modelling a case that cannot arise, and writing them out would
/// claim a lifetime this file does not have.
fn parse_set_cookie(header: &str) -> Option<Cookie> {
    let mut parts = header.split(';');
    let (name, value) = parts.next()?.trim().split_once('=')?;
    let mut cookie = Cookie {
        name: name.to_owned(),
        value: value.to_owned(),
        path: "/".to_owned(),
        http_only: false,
        secure: false,
        same_site: "Lax".to_owned(),
    };
    for attr in parts {
        let attr = attr.trim();
        let (key, val) = match attr.split_once('=') {
            Some((k, v)) => (k.trim().to_ascii_lowercase(), v.trim().to_owned()),
            None => (attr.to_ascii_lowercase(), String::new()),
        };
        match key.as_str() {
            "path" => cookie.path = val,
            "httponly" => cookie.http_only = true,
            "secure" => cookie.secure = true,
            "samesite" => cookie.same_site = val,
            _ => {}
        }
    }
    Some(cookie)
}

/// Present `grant` at `target`, returning the cookies the server set and the
/// user it says you are.
///
/// A refusal is the **server's** error, verbatim: a grant that expired while the
/// command was running, one already redeemed, a user deleted in between.
/// Guessing at which would be worse than quoting it (§16).
pub async fn mint_session(target: &Target, grant: &str) -> Result<(Vec<Cookie>, Json)> {
    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| Error::config(format!("could not build the HTTP client: {e}")))?;

    // One GET to be handed a CSRF cookie, exactly as a browser opening the app
    // would be. Any answer will do — even a 404 carries the cookie — so the
    // status is not checked; only the absence of a cookie is a problem.
    let primer = client
        .get(&target.url)
        .header(reqwest::header::HOST, &target.host)
        .send()
        .await
        .map_err(|e| unreachable_server(target, e))?;
    let csrf = cookies_of(primer.headers())
        .into_iter()
        .find(|c| c.name == CSRF_COOKIE)
        .ok_or_else(|| {
            Error::msg(format!(
                "{} answered without a `{CSRF_COOKIE}` cookie, so it is not a \
                 Saltcorn server (or something in front of it is stripping cookies)",
                target.url
            ))
        })?;

    let redeem = format!("{}{TOKEN_ROUTE}", target.url.trim_end_matches('/'));
    let response = client
        .post(&redeem)
        .header(reqwest::header::HOST, &target.host)
        .header(
            reqwest::header::COOKIE,
            format!("{CSRF_COOKIE}={}", csrf.value),
        )
        .header(CSRF_HEADER, &csrf.value)
        .json(&json!({ GRANT_FIELD: grant }))
        .send()
        .await
        .map_err(|e| unreachable_server(target, e))?;

    let status = response.status();
    let mut cookies = cookies_of(response.headers());
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(Error::invalid(format!(
            "{redeem} refused the session grant with {status}: {}",
            server_message(&body)
        )));
    }
    // The redemption response carries the session; it does **not** re-set the CSRF
    // cookie, because the request already had one — the server only mints that
    // on first contact. Carrying the primer's forward is what makes the written
    // jar complete: a restored browser would be handed a fresh one on its first
    // page load, but `curl` would not, and its first POST would 403.
    if !cookies.iter().any(|c| c.name == CSRF_COOKIE) {
        cookies.push(csrf);
    }
    if !cookies.iter().any(|c| c.name == SESSION_COOKIE) {
        return Err(Error::msg(format!(
            "{redeem} accepted the grant but set no `{SESSION_COOKIE}` cookie, \
             so there is no session to write"
        )));
    }
    let user = serde_json::from_str(&body).unwrap_or(Json::Null);
    Ok((cookies, user))
}

/// Every cookie in a response's `Set-Cookie` headers.
fn cookies_of(headers: &reqwest::header::HeaderMap) -> Vec<Cookie> {
    headers
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(parse_set_cookie)
        .collect()
}

/// The error for a server that could not be reached, saying both where we tried
/// and what to do about it — the two things missing from a bare connect error.
fn unreachable_server(target: &Target, e: reqwest::Error) -> Error {
    Error::config(format!(
        "could not reach the server at {} (as host `{}`): {e}. Is `saltcorn serve` \
         running, and is --url pointing at it?",
        target.url, target.host
    ))
}

/// The server's own message out of an error body, falling back to the body.
fn server_message(body: &str) -> String {
    serde_json::from_str::<Json>(body)
        .ok()
        .and_then(|j| {
            j.get("error")
                .or_else(|| j.get("message"))
                .and_then(|m| m.as_str().map(str::to_owned))
        })
        .unwrap_or_else(|| body.trim().to_owned())
}

/// Render the cookies in `format`, for `host`.
pub fn render(cookies: &[Cookie], host: &str, format: SessionFormat) -> String {
    match format {
        SessionFormat::Playwright => playwright(cookies, host),
        SessionFormat::Netscape => netscape(cookies, host),
    }
}

/// Playwright's `storageState`: the cookies, and no origin-scoped storage.
///
/// `expires: -1` is Playwright's spelling of a **session cookie**, which is what
/// these are: the server sets no `Expires`, and the lifetime that actually
/// governs them is the server-side session's.
fn playwright(cookies: &[Cookie], host: &str) -> String {
    let cookies: Vec<Json> = cookies
        .iter()
        .map(|c| {
            json!({
                "name": c.name,
                "value": c.value,
                // Host-only, as the server set it: no leading dot, so it is not
                // sent to any other subdomain of the base domain.
                "domain": host,
                "path": c.path,
                "expires": -1,
                "httpOnly": c.http_only,
                "secure": c.secure,
                "sameSite": same_site(&c.same_site),
            })
        })
        .collect();
    let state = json!({ "cookies": cookies, "origins": [] });
    format!(
        "{}\n",
        serde_json::to_string_pretty(&state).unwrap_or_else(|_| state.to_string())
    )
}

/// Playwright accepts exactly `Strict`, `Lax` and `None`.
fn same_site(raw: &str) -> &'static str {
    match raw.to_ascii_lowercase().as_str() {
        "strict" => "Strict",
        "none" => "None",
        _ => "Lax",
    }
}

/// A Netscape cookie file: `curl --cookie`, `wget --load-cookies`.
///
/// The `#HttpOnly_` prefix is curl's convention for an `HttpOnly` cookie, and
/// the session cookie is one — without it curl reads the line as a comment and
/// silently sends no session, which is the failure mode this whole file exists
/// to avoid.
fn netscape(cookies: &[Cookie], host: &str) -> String {
    let mut out = String::from(
        "# Netscape HTTP Cookie File\n\
         # Written by `saltcorn auth token`. This is a live session — treat it as a password.\n",
    );
    for c in cookies {
        out.push_str(&format!(
            "{}{host}\tFALSE\t{}\t{}\t0\t{}\t{}\n",
            if c.http_only { "#HttpOnly_" } else { "" },
            c.path,
            if c.secure { "TRUE" } else { "FALSE" },
            c.name,
            c.value,
        ));
    }
    out
}

/// Write `contents` to `path`, readable by nobody else.
///
/// The mode is set **before** the bytes go in, so the file is never briefly
/// world-readable with a session in it.
pub fn write_private(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|e| Error::file(format!("could not write {}: {e}", path.display())))?;
    file.write_all(contents.as_bytes())
        .map_err(|e| Error::file(format!("could not write {}: {e}", path.display())))?;
    Ok(())
}

/// Where to write, given `--out` and the format's default.
pub fn out_path(out: Option<&str>, format: SessionFormat) -> PathBuf {
    PathBuf::from(out.unwrap_or_else(|| format.default_file()))
}

/// Which user the session is for — the three ways of saying it.
///
/// Three rather than one because the caller is usually a script that does not
/// know the installation's users, and the two questions it can actually answer
/// are "the admin" and "somebody who can see this screen". `--email` remains for
/// the case where the answer is a particular person, which is the one a shared
/// deployment wants: give the agent its own account and name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserSelector {
    /// `--email EMAIL`: exactly this user.
    Email(String),
    /// `--admin`: the first user holding the admin role.
    Admin,
    /// `--role NAME`: the first user holding the role with this name.
    Role(String),
}

impl UserSelector {
    /// How the selection reads in a message about it.
    fn describe(&self) -> String {
        match self {
            UserSelector::Email(email) => format!("--email {email}"),
            UserSelector::Admin => "--admin".to_owned(),
            UserSelector::Role(name) => format!("--role {name}"),
        }
    }
}

/// Resolve a selector against the database, or say why it names nobody.
///
/// Every refusal names what was asked for and what exists instead — an unknown
/// role lists the roles, a role nobody holds says so and gives its number — because
/// the caller is at a shell with no other way to find out, and "not found" on its
/// own would send them to the admin UI to answer a question this command could
/// have answered.
pub async fn resolve_user(catalog: &Catalog, selector: &UserSelector) -> Result<User> {
    match selector {
        UserSelector::Email(email) => sc_auth::load_user_by_email(catalog, email)
            .await?
            .ok_or_else(|| Error::not_found(format!("no user with the email `{email}`"))),
        UserSelector::Admin => {
            let role = role_by_number(catalog, ROLE_ADMIN).await?;
            first_holder(catalog, &role).await
        }
        UserSelector::Role(name) => {
            let role = role_by_name(catalog, name).await?;
            first_holder(catalog, &role).await
        }
    }
}

/// The role with this name, or an error listing the roles there are.
async fn role_by_name(catalog: &Catalog, name: &str) -> Result<Role> {
    let roles = list_roles(catalog).await?;
    roles
        .iter()
        .find(|r| r.name.eq_ignore_ascii_case(name.trim()))
        .cloned()
        .ok_or_else(|| {
            Error::not_found(format!(
                "no role named `{name}`. The roles on this server are: {}",
                role_list(&roles)
            ))
        })
}

/// The role with this number — used for `--admin`, so its name is the
/// installation's own even when an admin has renamed it.
async fn role_by_number(catalog: &Catalog, number: u8) -> Result<Role> {
    list_roles(catalog)
        .await?
        .into_iter()
        .find(|r| r.role == number)
        .ok_or_else(|| Error::not_found(format!("this database has no role {number}")))
}

/// The first user holding `role`, or an error saying that nobody does.
async fn first_holder(catalog: &Catalog, role: &Role) -> Result<User> {
    sc_auth::first_user_with_role(catalog, role.role)
        .await?
        .ok_or_else(|| {
            Error::not_found(format!(
                "no user has the role `{}` ({}), so there is no session to mint for it",
                role.name, role.role
            ))
        })
}

/// The roles, as an error message lists them: `Admin (1), Public (100)`.
pub fn role_list(roles: &[Role]) -> String {
    roles
        .iter()
        .map(|r| format!("{} ({})", r.name, r.role))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What `auth token` was asked for.
#[derive(Debug, Clone)]
pub struct TokenArgs {
    /// `--app`: which application's session this is.
    pub app: String,
    /// `--email` / `--admin` / `--role`: who the session is for.
    pub user: UserSelector,
    /// `--url`: the origin to connect to, when it is not the application's own
    /// (a loopback address on a machine with no DNS for the base domain).
    pub url: Option<String>,
    /// `--base-domain`: overrides the configuration file's.
    pub base_domain: Option<String>,
    /// `--out`: where to write.
    pub out: Option<String>,
    /// `--format`: which file to write.
    pub format: SessionFormat,
}

/// Parse `auth token`'s flags. Unknown ones are refused by name, like every
/// other command's.
///
/// The three ways of naming a user are mutually exclusive and one is required:
/// two of them together is a caller who has not decided, and defaulting to
/// either would sign them in as somebody they did not ask for.
pub fn parse_token_args(args: &[String]) -> Result<TokenArgs> {
    let mut app = None;
    let mut email = None;
    let mut role = None;
    let mut admin = false;
    let mut url = None;
    let mut base_domain = None;
    let mut out = None;
    let mut format = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        // `--admin` is the one flag that takes no value; everything else reads
        // the next argument, and a flag left dangling at the end is an error
        // rather than an empty string.
        if arg == "--admin" {
            admin = true;
            continue;
        }
        let slot = match arg.as_str() {
            "--app" => &mut app,
            "--email" => &mut email,
            "--role" => &mut role,
            "--url" => &mut url,
            "--base-domain" => &mut base_domain,
            "--out" => &mut out,
            "--format" => &mut format,
            other => {
                return Err(Error::config(format!(
                    "unknown auth token argument `{other}`"
                )));
            }
        };
        *slot = Some(
            it.next()
                .ok_or_else(|| Error::config(format!("{arg} needs a value")))?
                .clone(),
        );
    }

    let app = app.ok_or_else(|| {
        Error::config("auth token needs --app: which application's session to mint")
    })?;
    let user = select_user(email, admin, role)?;
    Ok(TokenArgs {
        app,
        user,
        url,
        base_domain,
        out,
        format: match format {
            Some(name) => SessionFormat::parse(&name)?,
            None => SessionFormat::Playwright,
        },
    })
}

/// Exactly one of the three ways of naming a user.
fn select_user(email: Option<String>, admin: bool, role: Option<String>) -> Result<UserSelector> {
    let mut chosen: Vec<UserSelector> = [
        email.map(UserSelector::Email),
        admin.then_some(UserSelector::Admin),
        role.map(UserSelector::Role),
    ]
    .into_iter()
    .flatten()
    .collect();
    if chosen.len() > 1 {
        return Err(Error::config(format!(
            "auth token takes one of --email, --admin and --role, not {}",
            chosen
                .iter()
                .map(UserSelector::describe)
                .collect::<Vec<_>>()
                .join(" and ")
        )));
    }
    chosen.pop().ok_or_else(|| {
        Error::config(
            "auth token needs to know who the session is for: --email EMAIL, \
             --admin (the first admin user), or --role NAME (the first user \
             holding that role)",
        )
    })
}

/// Mint a grant for `user` and exchange it at `target` for a session.
///
/// The two halves of the command that need something other than a file: the
/// database, which is what authorises this at all, and the running server, which
/// is the only thing that can turn the grant into a session.
pub async fn session_for(
    catalog: &Catalog,
    target: &Target,
    user: &User,
) -> Result<(Vec<Cookie>, Json)> {
    let grant = create_session_grant(catalog, user.id).await?;
    mint_session(target, &grant).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Cookie {
        parse_set_cookie("sc_session=abc123; Path=/; HttpOnly; SameSite=Strict").expect("parse")
    }

    #[test]
    fn a_set_cookie_header_is_parsed_with_its_attributes() {
        let c = session();
        assert_eq!(c.name, "sc_session");
        assert_eq!(c.value, "abc123");
        assert_eq!(c.path, "/");
        assert!(c.http_only);
        assert!(!c.secure);
        assert_eq!(c.same_site, "Strict");

        // A cookie with no attributes still parses, with the defaults.
        let plain = parse_set_cookie("sc_csrf=xyz").expect("parse");
        assert_eq!(plain.path, "/");
        assert!(!plain.http_only);

        // Not a cookie at all.
        assert!(parse_set_cookie("garbage").is_none());
    }

    #[test]
    fn the_playwright_file_is_a_storage_state_a_browser_can_restore() {
        let text = playwright(&[session()], "blog.example.com");
        let state: Json = serde_json::from_str(&text).expect("valid JSON");
        let cookie = &state["cookies"][0];
        assert_eq!(cookie["name"], "sc_session");
        assert_eq!(cookie["value"], "abc123");
        // Host-only: no leading dot, so the session is not offered to a sibling
        // application on another subdomain.
        assert_eq!(cookie["domain"], "blog.example.com");
        assert_eq!(cookie["httpOnly"], true);
        assert_eq!(cookie["sameSite"], "Strict");
        // Playwright's spelling of "session cookie".
        assert_eq!(cookie["expires"], -1);
        assert!(state["origins"].is_array());
    }

    #[test]
    fn the_netscape_file_marks_an_httponly_cookie_the_way_curl_reads_it() {
        let text = netscape(&[session()], "blog.example.com");
        let line = text
            .lines()
            .find(|l| l.contains("sc_session"))
            .expect("the session line");
        assert!(
            line.starts_with("#HttpOnly_blog.example.com\t"),
            "curl needs the prefix or it reads the line as a comment: {line}"
        );
        let fields: Vec<&str> = line.split('\t').collect();
        assert_eq!(fields.len(), 7, "{line}");
        assert_eq!(fields[1], "FALSE", "host-only, not a domain cookie");
        assert_eq!(fields[2], "/");
        assert_eq!(fields[3], "FALSE", "not secure over http");
        assert_eq!(fields[4], "0", "a session cookie");
        assert_eq!(fields[5], "sc_session");
        assert_eq!(fields[6], "abc123");
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn a_user_is_named_one_of_three_ways() {
        let by_email = parse_token_args(&args(&["--app", "blog", "--email", "a@b.c"]))
            .expect("an email names a user");
        assert_eq!(by_email.user, UserSelector::Email("a@b.c".to_owned()));

        // `--admin` takes no value, and what follows it is still parsed.
        let by_admin =
            parse_token_args(&args(&["--app", "blog", "--admin", "--format", "netscape"]))
                .expect("--admin names a user");
        assert_eq!(by_admin.user, UserSelector::Admin);
        assert_eq!(by_admin.format, SessionFormat::Netscape);

        let by_role =
            parse_token_args(&args(&["--app", "blog", "--role", "Editor"])).expect("a role");
        assert_eq!(by_role.user, UserSelector::Role("Editor".to_owned()));
    }

    #[test]
    fn naming_no_user_or_two_is_refused_by_name() {
        // None of the three: the error lists all three rather than naming the
        // one that used to be mandatory.
        let err = parse_token_args(&args(&["--app", "blog"])).expect_err("no user");
        let message = err.to_string();
        for flag in ["--email", "--admin", "--role"] {
            assert!(message.contains(flag), "{message}");
        }

        // Two of them: a caller who has not decided, and there is no sensible
        // precedence to invent.
        let err = parse_token_args(&args(&["--app", "blog", "--admin", "--email", "a@b.c"]))
            .expect_err("two selectors");
        assert!(err.to_string().contains("--email a@b.c"), "{err}");
        assert!(err.to_string().contains("--admin"), "{err}");
    }

    #[test]
    fn the_roles_are_listed_the_way_an_error_shows_them() {
        let roles = [Role::new(ROLE_ADMIN, "Admin"), Role::new(100, "Public")];
        assert_eq!(role_list(&roles), "Admin (1), Public (100)");
    }

    #[test]
    fn the_output_path_defaults_per_format() {
        assert_eq!(
            out_path(None, SessionFormat::Playwright),
            PathBuf::from(DEFAULT_PLAYWRIGHT_FILE)
        );
        assert_eq!(
            out_path(None, SessionFormat::Netscape),
            PathBuf::from(DEFAULT_NETSCAPE_FILE)
        );
        assert_eq!(
            out_path(Some("/tmp/s.json"), SessionFormat::Playwright),
            PathBuf::from("/tmp/s.json")
        );
    }

    #[test]
    fn an_unknown_format_names_the_two_there_are() {
        let err = SessionFormat::parse("har").expect_err("unknown");
        assert!(err.to_string().contains("playwright"), "{err}");
        assert!(err.to_string().contains("netscape"), "{err}");
    }

    #[test]
    fn the_browser_url_is_the_apps_host_not_the_connection_target() {
        // `--url` points at the loopback; the browser must still be told the
        // name the server routes on.
        let target = Target {
            url: "http://127.0.0.1:3000".to_owned(),
            host: "blog.example.com".to_owned(),
            secure: false,
        };
        assert_eq!(target.browser_url(), "http://blog.example.com:3000");
    }
}
