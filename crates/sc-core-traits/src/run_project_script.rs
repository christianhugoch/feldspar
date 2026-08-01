//! `run_project_script` — `npm run <script>`, for a script the project already
//! has (§11.3, decision 6).
//!
//! **There is no shell trait, and this is why there does not need to be.** The
//! IDE milestone declined to give an admin a terminal, and handing a model
//! `run_command` would be that decision arriving through the back door. What a
//! project's `package.json` declares, though, is a set of commands the people who
//! wrote it intended to be run — `test`, `lint`, `typecheck` — and running one of
//! *those* is bounded in the way a shell is not: the model **chooses from a list**
//! rather than composing a command line, and the list is the project's own.
//!
//! So: the scripts are read from `package.json` through the [`FileStore`], the
//! requested one must be among them (the refusal names the ones that are), and
//! `npm run <script>` is spawned directly — no shell, no arguments of the model's
//! own, no `--`. A script the model wants but the project does not have is a file
//! it can write and then run, which keeps the whole thing inside the project's
//! source rather than inside a command line.
//!
//! Bounded in time and in output: a timeout the admin configures, and both
//! streams captured and truncated, because a test suite that prints a megabyte is
//! a conversation nobody can afford.
//!
//! [`FileStore`]: sc_files::FileStore

use std::process::Stdio;
use std::time::Duration;

use sc_agent::{AgentTrait, TraitCheck, TraitContext};
use sc_catalog::Catalog;
use sc_error::{Error, Result};
use sc_llm::ToolSpec;
use sc_types::{Attrs, BasicType, FormField};
use serde_json::{Value as Json, json};
use tokio::process::Command;

use crate::files::{
    FileScope, check_scope, check_tool_name, config_count, configured_scope, scope_as_written,
    scope_fields, string_arg,
};
use crate::table::arguments;

/// How long one script may run for.
pub const CFG_TIMEOUT: &str = "timeout_seconds";

/// Long enough for a test suite or a type check, short enough that a watch-mode
/// script started by mistake does not hold the conversation open for ever.
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 300;

/// How much of each stream is reported. Past this the interesting part is the
/// tail — a failure's last words — so that is what is kept.
pub const MAX_OUTPUT_CHARS: usize = 20_000;

/// The script to run.
const ARG_SCRIPT: &str = "script";

/// Run one of a project's own `package.json` scripts.
pub struct RunProjectScript;

/// The tool one `run_project_script` instance offers, derived from its scope.
pub fn tool_name(scope: &FileScope) -> String {
    format!("run_script_{}", scope.slug())
}

#[async_trait::async_trait]
impl AgentTrait for RunProjectScript {
    fn name(&self) -> &str {
        "run_project_script"
    }

    fn description(&self) -> &str {
        "Run one of a project's own package.json scripts (npm run …)"
    }

    fn config_spec(&self) -> Vec<FormField> {
        let mut spec = scope_fields();
        spec.push(
            FormField::new(CFG_TIMEOUT, BasicType::Int)
                .label("Timeout (seconds)")
                .default_value(DEFAULT_TIMEOUT_SECONDS as i64),
        );
        spec
    }

    async fn validate_config(&self, check: &TraitCheck<'_>) -> Result<()> {
        let scope = check_scope(check).await?;
        config_count(check.config, CFG_TIMEOUT, DEFAULT_TIMEOUT_SECONDS)?;
        check_tool_name(&tool_name(&scope))
    }

    fn tools(&self, _catalog: &Catalog, config: &Attrs) -> Vec<ToolSpec> {
        let scope = scope_as_written(config);
        vec![ToolSpec::new(
            tool_name(&scope),
            format!(
                "Run one of the scripts declared in the `package.json` of {} \
                 (`npm run <script>`) and return its exit status and output. \
                 Only a script that project already declares can be run — there \
                 is no shell and no way to pass arguments — so read its \
                 `package.json` to see what there is. Use this for the checks a \
                 project defines: its tests, its linter, its type check.",
                scope.label()
            ),
            json!({
                "type": "object",
                "properties": {
                    ARG_SCRIPT: {
                        "type": "string",
                        "description":
                            "The script's name, exactly as `package.json` declares it.",
                    },
                },
                "required": [ARG_SCRIPT],
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
        let scope = configured_scope(config)?;
        let timeout = config_count(config, CFG_TIMEOUT, DEFAULT_TIMEOUT_SECONDS)?;
        let args = arguments(args, &[ARG_SCRIPT])?;
        let script = string_arg(&args, ARG_SCRIPT)?;

        let (store, floor) = scope.connect(ctx.catalog).await?;
        let manifest_path = scope.resolve("package.json")?;
        // The manifest is a file in the store like any other, so reading it is
        // the caller's read: an agent whose user cannot see the project cannot
        // learn what scripts it declares either.
        sc_files::check_access(store.as_ref(), floor, &manifest_path, ctx.caller.role).await?;
        let manifest = store.read(&manifest_path).await.map_err(|_| {
            Error::invalid(format!(
                "{} has no `package.json`, so it declares no scripts to run",
                scope.label()
            ))
        })?;
        let declared = declared_scripts(&manifest)?;
        if !declared.iter().any(|s| s == &script) {
            return Err(Error::invalid(match declared.is_empty() {
                true => {
                    format!("that `package.json` declares no scripts, so `{script}` cannot be run")
                }
                false => format!(
                    "`{script}` is not a script this project declares. It declares: {}.",
                    declared.join(", ")
                ),
            }));
        }

        // The bundler's requirement, for the bundler's reason (§13.3): `npm` is
        // an external process handed a working directory, so the store must have
        // one. An object store cannot host a project that runs.
        let dir = store
            .local_path(&manifest_path)?
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .ok_or_else(|| {
                Error::config(format!(
                    "{} has no local path, so no script can be run in it; \
                 a project that runs lives in a local file store",
                    scope.label()
                ))
            })?;

        let child = Command::new("npm")
            .arg("run")
            .arg(&script)
            .current_dir(&dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output();

        let outcome = tokio::time::timeout(Duration::from_secs(timeout), child).await;
        let output = match outcome {
            // Dropping the future killed the process (`kill_on_drop`), so a
            // script that hangs does not outlive the call that started it.
            Err(_) => {
                return Ok(json!({
                    "script": script,
                    "timed_out": true,
                    "seconds": timeout,
                    "message": format!(
                        "`npm run {script}` was still running after {timeout} seconds \
                         and was stopped."
                    ),
                }));
            }
            Ok(result) => result.map_err(|e| {
                Error::config(format!(
                    "could not run `npm run {script}` in {}: {e}",
                    dir.display()
                ))
            })?,
        };

        Ok(json!({
            "script": script,
            "exit_code": output.status.code(),
            "succeeded": output.status.success(),
            "stdout": tail(&String::from_utf8_lossy(&output.stdout)),
            "stderr": tail(&String::from_utf8_lossy(&output.stderr)),
            "timed_out": false,
        }))
    }
}

/// The script names a `package.json` declares.
fn declared_scripts(manifest: &[u8]) -> Result<Vec<String>> {
    let json: Json = serde_json::from_slice(manifest).map_err(|e| {
        Error::invalid(format!(
            "that project's `package.json` is not valid JSON: {e}"
        ))
    })?;
    Ok(match json.get("scripts") {
        Some(Json::Object(scripts)) => scripts.keys().cloned().collect(),
        _ => Vec::new(),
    })
}

/// The last [`MAX_OUTPUT_CHARS`] characters of a stream.
///
/// The tail rather than the head: a script that failed says why at the end, and
/// the first 20,000 characters of a passing test run are the part nobody needs.
fn tail(text: &str) -> String {
    let count = text.chars().count();
    if count <= MAX_OUTPUT_CHARS {
        return text.to_owned();
    }
    let skipped = count - MAX_OUTPUT_CHARS;
    let kept: String = text.chars().skip(skipped).collect();
    format!("[… {skipped} earlier characters omitted …]\n{kept}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tools_name_is_derived_from_its_scope() {
        let scope = FileScope {
            store: "src".to_owned(),
            root: "web/todo".to_owned(),
        };
        assert_eq!(tool_name(&scope), "run_script_src_web_todo");
    }

    #[test]
    fn the_runnable_set_is_what_the_manifest_declares() {
        let manifest = br#"{"name":"todo","scripts":{"build":"vite build","test":"vitest"}}"#;
        let mut scripts = declared_scripts(manifest).unwrap();
        scripts.sort();
        assert_eq!(scripts, ["build", "test"]);

        // A manifest with no scripts declares none, rather than failing: the
        // refusal the caller then gets says exactly that.
        assert!(declared_scripts(br#"{"name":"todo"}"#).unwrap().is_empty());
        assert!(declared_scripts(b"not json").is_err());
    }

    #[test]
    fn a_long_output_keeps_its_tail_and_says_what_it_dropped() {
        let text = "x".repeat(MAX_OUTPUT_CHARS + 10);
        let kept = tail(&text);
        assert!(kept.contains("10 earlier characters omitted"), "{kept}");
        assert!(kept.ends_with(&"x".repeat(20)));
        // Short output is untouched.
        assert_eq!(tail("all fine"), "all fine");
    }
}
