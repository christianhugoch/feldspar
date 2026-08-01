//! `build_application` — build the app whose source the agent is editing (§11.3).
//!
//! The trait that closes the loop. An agent that can read, search and edit a
//! project can produce a change that does not compile, and a model that is not
//! told cannot fix it. So the build's **diagnostics are the tool result**: a
//! failed build is not an exception here, it is the most useful answer this tool
//! ever returns.
//!
//! Three decisions:
//!
//! - **A failed build comes back as a result, not an error.** `built: false` with
//!   the tools' own output and the file/line/message triples parsed out of it.
//!   The loop would have turned an `Err` into a tool result anyway (§11.2), but
//!   flattened to a sentence; the model can act on a list of diagnostics, and the
//!   admin watching the transcript can read one.
//! - **It builds the *stored* application**, resolved by subdomain from
//!   `_sc_applications` and built through the same `sc_app::build_application`
//!   the admin's Build button runs — the client is regenerated, a React app's
//!   runtime is regenerated, the same bundler runs over the same tree. A second
//!   build path would be a second set of results to explain.
//! - **It does not mount what it built.** Mounting is the server's (§13.2 — "the
//!   mount registry is live"), and an agent's build is a *check*: the question is
//!   "does this compile?", not "serve this to the world". An admin publishes what
//!   the agent produced by pressing Build, which is also where they get to look
//!   at the diff first.

use sc_agent::{AgentTrait, TraitCheck, TraitContext};
use sc_app::{BuildReport, app_source_from_config, load_application_by_subdomain};
use sc_catalog::Catalog;
use sc_error::{Error, Result};
use sc_llm::ToolSpec;
use sc_types::{Attrs, BasicType, FormField};
use serde_json::{Value as Json, json};

use crate::files::slugify;
use crate::table::{arguments, config_str};

/// The application this trait builds, by subdomain — its unique key (§13.2).
pub const CFG_APPLICATION: &str = "application";

/// Build one configured application.
pub struct BuildApplication;

/// The tool one `build_application` instance offers, derived from its app.
pub fn tool_name(application: &str) -> String {
    format!("build_{}", slugify(application))
}

#[async_trait::async_trait]
impl AgentTrait for BuildApplication {
    fn name(&self) -> &str {
        "build_application"
    }

    fn description(&self) -> &str {
        "Build one application from its source, returning the build's diagnostics"
    }

    fn config_spec(&self) -> Vec<FormField> {
        vec![
            FormField::new(CFG_APPLICATION, BasicType::Text)
                .label("Application (subdomain)")
                .required(),
        ]
    }

    /// The application must exist, and it must be one that builds.
    ///
    /// Checked on save and again on load, so an agent pointed at a deleted app —
    /// or at one whose framework has no build step, which is the same mistake
    /// found later — leaves the live set with a reason rather than failing when
    /// the model calls the tool.
    async fn validate_config(&self, check: &TraitCheck<'_>) -> Result<()> {
        let subdomain = configured_application(check.config)?;
        let app = load_application_by_subdomain(check.catalog, &subdomain)
            .await?
            .ok_or_else(|| Error::invalid(format!("no application is served at `{subdomain}`")))?;
        // Resolving the source is what says "this framework builds from a file
        // store"; a static app has nothing to build and the admin should hear it
        // here, not from a tool call.
        app_source_from_config(&app.framework)?;
        Ok(())
    }

    fn tools(&self, _catalog: &Catalog, config: &Attrs) -> Vec<ToolSpec> {
        let configured = config_str(config, CFG_APPLICATION);
        vec![ToolSpec::new(
            tool_name(&configured),
            format!(
                "Build the `{configured}` application from its source. Returns \
                 whether the build succeeded, the build tools' output, and — when \
                 it failed — the diagnostics with their file, line and message. \
                 Run this after changing the source to find out whether the change \
                 compiles; a failed build's diagnostics are what to fix."
            ),
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }),
        )]
    }

    async fn call(
        &self,
        config: &Attrs,
        _tool: &str,
        args: &Json,
        ctx: &mut TraitContext<'_>,
    ) -> Result<Json> {
        arguments(args, &[])?;
        let subdomain = configured_application(config)?;
        let app = load_application_by_subdomain(ctx.catalog, &subdomain)
            .await?
            .ok_or_else(|| Error::invalid(format!("no application is served at `{subdomain}`")))?;
        let source = app_source_from_config(&app.framework)?;

        // Everything from here is *news about the build*, including its failure,
        // so it is reported rather than raised.
        match sc_app::build_application(ctx.catalog, &app, &source, ctx.triggers).await {
            Ok(report) => Ok(json!({
                "application": subdomain,
                "built": true,
                "output": success_log(&report),
                "diagnostics": diagnostics(&format!("{}\n{}", report.stdout, report.stderr)),
            })),
            Err(e) => {
                let output = e.to_string();
                Ok(json!({
                    "application": subdomain,
                    "built": false,
                    "output": output,
                    "diagnostics": diagnostics(&output),
                }))
            }
        }
    }
}

/// The configured application's subdomain.
fn configured_application(config: &Attrs) -> Result<String> {
    let name = config_str(config, CFG_APPLICATION);
    if name.is_empty() {
        return Err(Error::invalid(format!("`{CFG_APPLICATION}` is required")));
    }
    Ok(name)
}

/// What a successful build said, in the order it said it.
///
/// Kept even on success: bundlers report warnings here, and a model told only
/// "built" would never see them.
fn success_log(report: &BuildReport) -> String {
    let mut parts = Vec::new();
    if let Some(install) = &report.install_log {
        parts.push(install.trim().to_owned());
    }
    parts.push(report.stdout.trim().to_owned());
    parts.push(report.stderr.trim().to_owned());
    parts.retain(|p| !p.is_empty());
    parts.join("\n")
}

// --- diagnostics -------------------------------------------------------------

/// One diagnostic the build tools reported.
#[derive(Debug, PartialEq, Eq)]
struct Diagnostic {
    file: String,
    line: u32,
    column: u32,
    message: String,
}

/// The diagnostics a build log names.
///
/// The Rust sibling of the IDE's `buildDiagnostics.ts`, understanding the same
/// shapes for the same reason: the type errors exist only in the tools' output,
/// and file/line/message is what a reader — a person in the Problems panel, a
/// model deciding what to edit — can act on. Anything unrecognised is *not*
/// lost: the whole output travels beside this list.
fn diagnostics(log: &str) -> Vec<Json> {
    // `src/App.tsx(12,5): error TS2322: Type 'x' is not assignable…` — tsc.
    let tsc = regex_lite::Regex::new(
        r"^(\S[^(]*)\((\d+),(\d+)\):\s*(?:error|warning)\s+([A-Za-z]+\d+):\s*(.+)$",
    );
    // `src/App.tsx:12:5: ERROR: Expected ";"` — esbuild and its imitators.
    let positioned = regex_lite::Regex::new(
        r"^\s*(?:\[[^\]]*\]\s*)?([^\s:]+\.[A-Za-z0-9]+):(\d+):(\d+):\s*(?:(?:ERROR|WARNING|error|warning):\s*)?(\S.*)$",
    );
    // `╭─[ src/main.ts:2:1 ]` — rolldown's boxed report, whose message is the
    // line above the frame.
    let frame = regex_lite::Regex::new(r"[╭┌][─-]*\[\s*([^\s\]]+?):(\d+):(\d+)\s*\]");
    let (Ok(tsc), Ok(positioned), Ok(frame)) = (tsc, positioned, frame) else {
        // A pattern that does not compile is this module's bug, not the build's;
        // the caller still gets the whole output.
        return Vec::new();
    };

    let mut found: Vec<Diagnostic> = Vec::new();
    let mut previous = String::new();
    for raw in log.lines() {
        let line = strip_ansi(raw);
        let line = line.trim_end();
        if let Some(c) = frame.captures(line) {
            push(&mut found, &c, 1, 2, 3, previous.trim());
            continue;
        }
        if let Some(c) = tsc.captures(line) {
            let message = format!("{}: {}", &c[4], &c[5]);
            push(&mut found, &c, 1, 2, 3, &message);
            previous.clear();
            continue;
        }
        if let Some(c) = positioned.captures(line) {
            let message = c[4].to_owned();
            push(&mut found, &c, 1, 2, 3, &message);
            previous.clear();
            continue;
        }
        if !line.trim().is_empty() {
            previous = line.trim().to_owned();
        }
    }
    found
        .iter()
        .map(|d| {
            json!({
                "file": d.file,
                "line": d.line,
                "column": d.column,
                "message": d.message,
            })
        })
        .collect()
}

/// Record one diagnostic, skipping a duplicate and one with nothing to say.
fn push(
    found: &mut Vec<Diagnostic>,
    caps: &regex_lite::Captures<'_>,
    file: usize,
    line: usize,
    column: usize,
    message: &str,
) {
    if message.trim().is_empty() {
        return;
    }
    let diagnostic = Diagnostic {
        file: caps[file].to_owned(),
        line: caps[line].parse().unwrap_or(1),
        column: caps[column].parse().unwrap_or(1),
        message: message.trim().to_owned(),
    };
    if !found.contains(&diagnostic) {
        found.push(diagnostic);
    }
}

/// Terminal colour, which a build that thought it had a TTY leaves behind.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tools_name_is_derived_from_its_application() {
        assert_eq!(tool_name("todo-app"), "build_todo_app");
    }

    #[test]
    fn tscs_diagnostics_are_parsed_with_their_file_and_line() {
        let log = "\
build command `npm run build` failed in /srv/store/web with exit status: 2
src/App.tsx(12,5): error TS2322: Type 'number' is not assignable to type 'string'.
src/App.tsx(19,1): error TS1005: ';' expected.
";
        let found = diagnostics(log);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0]["file"], "src/App.tsx");
        assert_eq!(found[0]["line"], 12);
        assert_eq!(found[0]["column"], 5);
        assert!(
            found[0]["message"]
                .as_str()
                .unwrap()
                .contains("not assignable"),
            "{found:?}"
        );
    }

    #[test]
    fn esbuilds_one_line_form_is_parsed_too_and_colour_is_ignored() {
        let log = "\u{1b}[31msrc/main.ts:2:1: ERROR: Expected \";\" but found \"}\"\u{1b}[0m";
        let found = diagnostics(log);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0]["file"], "src/main.ts");
        assert_eq!(found[0]["line"], 2);
        assert_eq!(found[0]["message"], "Expected \";\" but found \"}\"");
    }

    #[test]
    fn a_boxed_report_takes_its_message_from_the_line_above_it() {
        let log = "\
[builtin:vite-transform] 'export' modifier cannot be used here.
   ╭─[ src/main.ts:2:1 ]
";
        let found = diagnostics(log);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0]["file"], "src/main.ts");
        assert!(
            found[0]["message"]
                .as_str()
                .unwrap()
                .contains("cannot be used here"),
            "{found:?}"
        );
    }

    #[test]
    fn output_with_no_diagnostics_in_it_produces_none() {
        assert!(diagnostics("vite v5.0.0 building for production...\n✓ 34 modules").is_empty());
    }
}
