//! `view_app` — look at the application the run built, in a headless browser
//! (TODO §7b, 6b.9).
//!
//! A green `check` mounts the build as the run's **preview** (see
//! [`super::check`]). This tool opens that preview in the server's headless
//! Chromium, in a browser context of the run's own, **as the run's caller**: the
//! person chatting, or for a run nobody is present for, the account the
//! [`CFG_VIEW_APP_USER`] setting names. One action per call, over one page:
//!
//! - `goto(path)`, `click(ref)`, `fill(ref, text)`, `press(key)`,
//!   `wait_for(text | ref, timeout)` and `snapshot()`, each answered with a
//!   compact **accessibility snapshot** whose interactive elements carry refs
//!   (`@e12`) for the next call. It is text, a few hundred tokens a page, and
//!   works for any model.
//! - `screenshot(full_page?)`, a JPEG attached to the result, offered only when
//!   the model has `vision`.
//!
//! Every result also carries the URL, the last document's HTTP status, and the
//! console errors and failed requests since the previous call.
//!
//! **Its data is the live data.** The preview talks to the application's real
//! tables as the caller, so `click` and `fill` on a form write real rows. The
//! description says so.

use std::time::Duration;

use sc_agent::{BrowserAction, BrowserRequest, Elidable, TraitCheck, TraitContext};
use sc_auth::User;
use sc_error::{Error, Result};
use sc_llm::{ImagePart, ToolSpec};
use sc_types::{Attrs, BasicType, FormField};
use serde_json::{Value as Json, json};

use super::check::configured_application;
use super::{CFG_MAY_VIEW_APP, may};
use crate::files::{FileScope, config_count};
use crate::table::config_str;

/// The user a run with no caller (one a trigger started) looks at the
/// application as, by email.
pub const CFG_VIEW_APP_USER: &str = "view_app_user";

/// How long one `view_app` call may take, in seconds.
pub const CFG_VIEW_APP_TIMEOUT: &str = "view_app_timeout";

/// [`CFG_VIEW_APP_TIMEOUT`] when the admin sets none.
pub const DEFAULT_VIEW_APP_TIMEOUT: u64 = 30;

/// The tool one configured scope offers.
pub fn tool_name(scope: &FileScope) -> String {
    format!("view_app_{}", scope.slug())
}

/// The settings, after `check`'s.
pub fn config_fields() -> Vec<FormField> {
    vec![
        FormField::new(CFG_MAY_VIEW_APP, BasicType::Bool)
            .label(
                "May look at the application's preview in a headless browser, as the person \
                 chatting (needs the application setting and a browser on the server)",
            )
            .default_value(false),
        FormField::new(CFG_VIEW_APP_USER, BasicType::Text).label(
            "User a triggered run looks at the application as (email; a low-privilege account)",
        ),
        FormField::new(CFG_VIEW_APP_TIMEOUT, BasicType::Int)
            .label("view_app timeout per action (seconds)")
            .default_value(DEFAULT_VIEW_APP_TIMEOUT as i64),
    ]
}

/// The grant needs an application and a browser; the user must exist.
pub async fn validate(check: &TraitCheck<'_>) -> Result<()> {
    config_count(check.config, CFG_VIEW_APP_TIMEOUT, DEFAULT_VIEW_APP_TIMEOUT)?;
    let email = config_str(check.config, CFG_VIEW_APP_USER);
    if !email.is_empty()
        && sc_auth::load_user_by_email(check.catalog, &email)
            .await?
            .is_none()
    {
        return Err(Error::invalid(format!(
            "`{CFG_VIEW_APP_USER}` names `{email}`, and no user has that email"
        )));
    }
    if !may(check.config, CFG_MAY_VIEW_APP) {
        return Ok(());
    }
    if configured_application(check.config).is_none() {
        return Err(Error::invalid(format!(
            "`{CFG_MAY_VIEW_APP}` needs the `{}` setting: view_app looks at that \
             application's preview",
            crate::CFG_APPLICATION
        )));
    }
    if let Err(reason) = &check.host.browser {
        return Err(Error::invalid(format!(
            "`{CFG_MAY_VIEW_APP}` needs a headless browser on the server, and there is none: \
             {reason}"
        )));
    }
    Ok(())
}

/// The tool, with `screenshot` only for a model that takes images.
pub fn spec(scope: &FileScope, config: &Attrs, vision: bool) -> ToolSpec {
    let application = configured_application(config).unwrap_or_default();
    let mut actions = vec!["goto", "click", "fill", "press", "wait_for", "snapshot"];
    if vision {
        actions.push("screenshot");
    }
    ToolSpec::new(
        tool_name(scope),
        format!(
            "Look at this run's preview of the `{application}` application (mounted by a green \
             check) in a browser, as the user. One action per call; each returns the page as an \
             accessibility snapshot with @e refs for click and fill, plus console errors and \
             failed requests. The preview uses the live data: click and fill on a form write \
             real rows.{}",
            if vision {
                " screenshot returns an image."
            } else {
                ""
            }
        ),
        parameters(actions, vision),
    )
}

/// The tool's arguments: `full_page` only beside `screenshot`.
fn parameters(actions: Vec<&str>, vision: bool) -> Json {
    let mut properties = json!({
        "action": {"type": "string", "enum": actions},
        "path": {"type": "string", "description": "goto: a path, such as /tasks"},
        "ref": {"type": "string", "description": "click, fill, wait_for: an @e ref"},
        "text": {"type": "string", "description": "fill: the value; wait_for: text to wait for"},
        "key": {"type": "string", "description": "press: Enter, Tab, Escape, ArrowDown, …"},
        "timeout": {"type": "integer", "description": "wait_for: seconds"},
    });
    if vision && let Some(map) = properties.as_object_mut() {
        map.insert(
            "full_page".to_owned(),
            json!({"type": "boolean", "description": "screenshot: the whole page"}),
        );
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": ["action"],
        "additionalProperties": false,
    })
}

/// The action the arguments ask for.
fn action(args: &Json, limit: Duration) -> Result<BrowserAction> {
    let text = |key: &str| -> Result<String> {
        args.get(key)
            .and_then(Json::as_str)
            .map(str::to_owned)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Error::invalid(format!(
                    "`{}` needs `{key}`",
                    args.get("action").and_then(Json::as_str).unwrap_or("?")
                ))
            })
    };
    let optional = |key: &str| args.get(key).and_then(Json::as_str).map(str::to_owned);
    Ok(
        match args
            .get("action")
            .and_then(Json::as_str)
            .unwrap_or("snapshot")
        {
            "goto" => BrowserAction::Goto {
                path: text("path")?,
            },
            "click" => BrowserAction::Click {
                reference: text("ref")?,
            },
            "fill" => BrowserAction::Fill {
                reference: text("ref")?,
                text: args
                    .get("text")
                    .and_then(Json::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            },
            "press" => BrowserAction::Press { key: text("key")? },
            "wait_for" => {
                let (text, reference) = (optional("text"), optional("ref"));
                if text.is_none() && reference.is_none() {
                    return Err(Error::invalid("`wait_for` needs `text` or `ref`"));
                }
                let seconds = args.get("timeout").and_then(Json::as_u64).unwrap_or(10);
                BrowserAction::WaitFor {
                    text,
                    reference,
                    timeout: Duration::from_secs(seconds).min(limit),
                }
            }
            "snapshot" => BrowserAction::Snapshot,
            "screenshot" => BrowserAction::Screenshot {
                full_page: args
                    .get("full_page")
                    .and_then(Json::as_bool)
                    .unwrap_or(false),
            },
            other => {
                return Err(Error::invalid(format!(
                    "`{other}` is not a view_app action; use goto, click, fill, press, wait_for \
                     or snapshot"
                )));
            }
        },
    )
}

/// Whom the run looks at the application as.
async fn viewer(config: &Attrs, ctx: &TraitContext<'_>) -> Result<User> {
    if let Some(user) = &ctx.caller.user {
        return Ok(user.clone());
    }
    let email = config_str(config, CFG_VIEW_APP_USER);
    if email.is_empty() {
        return Err(Error::invalid(format!(
            "agent `{}` may not use view_app in this run: nobody is chatting, so there is no \
             user to look at the application as, and its `coding` trait has no \
             `{CFG_VIEW_APP_USER}`. Report that an administrator can set it to a \
             low-privilege account.",
            ctx.agent
        )));
    }
    sc_auth::load_user_by_email(ctx.catalog, &email)
        .await?
        .ok_or_else(|| {
            Error::invalid(format!(
                "`{CFG_VIEW_APP_USER}` names `{email}`, and no user has that email"
            ))
        })
}

/// Perform one action and report the page.
pub async fn call(config: &Attrs, args: &Json, ctx: &mut TraitContext<'_>) -> Result<Json> {
    let application = configured_application(config).ok_or_else(|| {
        Error::invalid(format!(
            "view_app needs the `coding` trait's `{}` setting",
            crate::CFG_APPLICATION
        ))
    })?;
    let limit = Duration::from_secs(config_count(
        config,
        CFG_VIEW_APP_TIMEOUT,
        DEFAULT_VIEW_APP_TIMEOUT,
    )?);
    let action = action(args, limit)?;
    let preview = ctx
        .require_previews()?
        .preview(ctx.run, &application)
        .ok_or_else(|| {
            Error::invalid(format!(
                "no preview of `{application}` is mounted for this run yet: run the check tool, \
                 and a green build is mounted as the preview"
            ))
        })?;
    let user = viewer(config, ctx).await?;
    let name = action.name();
    let target = match &action {
        BrowserAction::Goto { path } => format!(" {path}"),
        BrowserAction::Click { reference } | BrowserAction::Fill { reference, .. } => {
            format!(" {reference}")
        }
        BrowserAction::Press { key } => format!(" {key}"),
        _ => String::new(),
    };
    let report = ctx
        .require_browser()?
        .act(BrowserRequest {
            run: ctx.run,
            preview: &preview,
            user: &user,
            action,
            timeout: limit,
        })
        .await?;

    let mut out = format!("view_app {name}{target}\nurl: {}", report.url);
    if let Some(status) = report.status {
        out.push_str(&format!(" (status {status})"));
    }
    if let Some(note) = &report.note {
        out.push_str(&format!("\nnote: {note}"));
    }
    for (heading, lines) in [
        ("console errors", &report.console_errors),
        ("failed requests", &report.failed_requests),
    ] {
        if !lines.is_empty() {
            out.push_str(&format!("\n{heading}:"));
            for line in lines {
                out.push_str(&format!("\n- {line}"));
            }
        }
    }
    if let Some(shot) = report.screenshot {
        out.push_str(&format!(
            "\nscreenshot: attached ({} KB JPEG)",
            shot.len().div_ceil(1024)
        ));
        ctx.attach_image(ImagePart::new("image/jpeg", shot));
    }
    if let Some(snapshot) = &report.snapshot {
        out.push_str(&format!("\nsnapshot:\n{snapshot}"));
    }
    Ok(Json::String(out))
}

/// The action and its target: a repeated look at the same thing is a repeat.
pub fn fingerprint(args: &Json) -> Json {
    let mut out = serde_json::Map::new();
    for key in ["action", "path", "ref", "key", "text"] {
        if let Some(value) = args.get(key) {
            out.insert(key.to_owned(), value.clone());
        }
    }
    Json::Object(out)
}

/// An old result, as one line: `[elided snapshot of /tasks: 41 lines]`, and a
/// screenshot as a stub.
pub fn elide(old: &Elidable<'_>) -> String {
    let path = old
        .content
        .lines()
        .find_map(|line| line.strip_prefix("url: "))
        .map(|url| {
            let url = url.split(" (status").next().unwrap_or(url);
            match url.split_once("://") {
                Some((_, rest)) => rest.find('/').map_or("/", |i| &rest[i..]).to_owned(),
                None => url.to_owned(),
            }
        })
        .unwrap_or_else(|| "the page".to_owned());
    if old.images > 0 {
        return format!("[elided screenshot of {path}]");
    }
    match old.content.split_once("\nsnapshot:\n") {
        Some((_, snapshot)) => format!(
            "[elided snapshot of {path}: {} lines]",
            snapshot.lines().count()
        ),
        None => old.default_stub(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_arguments_make_one_action() {
        let limit = Duration::from_secs(30);
        assert_eq!(
            action(&json!({"action": "goto", "path": "/tasks"}), limit).unwrap(),
            BrowserAction::Goto {
                path: "/tasks".to_owned()
            }
        );
        assert_eq!(
            action(
                &json!({"action": "wait_for", "text": "Saved", "timeout": 99}),
                limit
            )
            .unwrap(),
            BrowserAction::WaitFor {
                text: Some("Saved".to_owned()),
                reference: None,
                timeout: limit,
            }
        );
        assert!(
            action(&json!({"action": "click"}), limit)
                .unwrap_err()
                .to_string()
                .contains("`click` needs `ref`")
        );
        assert!(action(&json!({"action": "wait_for"}), limit).is_err());
        assert!(action(&json!({"action": "scroll"}), limit).is_err());
    }

    #[test]
    fn the_fingerprint_is_the_action_and_its_target() {
        assert_eq!(
            fingerprint(&json!({"action": "click", "ref": "@e3", "timeout": 5})),
            json!({"action": "click", "ref": "@e3"})
        );
    }

    #[test]
    fn a_screenshot_is_offered_only_with_vision() {
        let scope = FileScope {
            store: "code".to_owned(),
            root: "web".to_owned(),
        };
        let config: Attrs = [("application".to_owned(), json!("todo"))]
            .into_iter()
            .collect();
        let without = spec(&scope, &config, false);
        let with = spec(&scope, &config, true);
        assert_eq!(without.name, "view_app_code_web");
        assert!(!without.parameters.to_string().contains("screenshot"));
        assert!(with.parameters.to_string().contains("screenshot"));
        assert!(without.description.contains("write real rows"));
    }

    #[test]
    fn an_old_result_is_one_line() {
        let call = sc_llm::ToolCall {
            id: "1".to_owned(),
            name: "view_app_code_web".to_owned(),
            arguments: json!({"action": "snapshot"}),
        };
        let content = "view_app snapshot\nurl: http://x--todo.example.com/tasks (status 200)\n\
                       snapshot:\npage \"Tasks\"\n  button \"Add\" @e1";
        let old = Elidable {
            index: 3,
            call: &call,
            content,
            images: 0,
            transcript: &[],
            state: None,
        };
        assert_eq!(elide(&old), "[elided snapshot of /tasks: 2 lines]");
        let shot = Elidable { images: 1, ..old };
        assert_eq!(elide(&shot), "[elided screenshot of /tasks]");
    }
}
