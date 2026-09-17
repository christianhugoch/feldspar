//! Feedback after a model turn's edits (TODO 5.10, §6).
//!
//! After the **last** edit of a turn, not after each one, the harness does what
//! a careful person does after saving:
//!
//! 1. **Formats** the edited files with the project's own `prettier`
//!    (`node_modules/.bin/prettier`), if the project has it installed. Only the
//!    edited files, so the diff stays the model's. A file prettier changed is
//!    recorded as seen in its new form, so the next edit is not refused as stale.
//! 2. **Diagnoses**: runs the configured `diagnose` script (`typecheck` by
//!    default) when `package.json` declares it, and attaches its diagnostics to
//!    the turn's last edit result. Files the turn edited come first, and each
//!    diagnostic is marked **new** or **pre-existing**.
//!
//! New or pre-existing is decided against a **baseline**: the diagnostics that
//! were there before the run's first edit, recorded just before that edit
//! ([`record_baseline`]). A diagnostic is keyed by its file and message, not its
//! line, since the model's edits move lines.
//!
//! Both steps run project code, so both need the `may_check` grant. Without it
//! nothing runs, and the edit results stand as they are.

use std::collections::BTreeMap;

use sc_catalog::Catalog;
use sc_error::Result;
use sc_types::Attrs;
use serde_json::Value as Json;

use super::script::{Ran, npm_run, project, run_bounded, tail};
use super::state::CodingState;
use super::{CFG_DIAGNOSE, CFG_MAY_CHECK, DEFAULT_DIAGNOSE, may};
use crate::files::{FileScope, config_count};
use crate::table::config_str;

/// The most diagnostics attached to an edit result.
pub const MAX_DIAGNOSTICS: usize = 20;

/// How long prettier may take.
const PRETTIER_TIMEOUT_SECONDS: u64 = 60;

/// The most characters of output shown when a failed check's output names no
/// diagnostic.
const MAX_TAIL_CHARS: usize = 2000;

/// The diagnose script's name.
fn diagnose_script(config: &Attrs) -> String {
    match config_str(config, CFG_DIAGNOSE) {
        name if name.trim().is_empty() => DEFAULT_DIAGNOSE.to_owned(),
        name => name.trim().to_owned(),
    }
}

/// One diagnostic, with its path relative to the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// The file.
    pub file: String,
    /// The line.
    pub line: u64,
    /// The column.
    pub column: u64,
    /// The message.
    pub message: String,
}

impl Diagnostic {
    /// What makes two diagnostics the same one, across edits that move lines.
    fn key(&self) -> String {
        format!("{}\u{1f}{}", self.file, self.message)
    }
}

/// What one diagnose run found.
#[derive(Debug, Clone)]
enum Diagnosis {
    /// The project declares no such script.
    Undeclared,
    /// It did not finish in time.
    TimedOut(u64),
    /// It ran.
    Ran {
        passed: bool,
        diagnostics: Vec<Diagnostic>,
        output: String,
    },
}

/// Run the diagnose script.
async fn diagnose(
    scope: &FileScope,
    config: &Attrs,
    catalog: &Catalog,
    role: u8,
) -> Result<Diagnosis> {
    let script = diagnose_script(config);
    let project = project(scope, catalog, role).await?;
    if !project.scripts.contains(&script) {
        return Ok(Diagnosis::Undeclared);
    }
    let timeout = config_count(config, super::CFG_TIMEOUT, super::DEFAULT_TIMEOUT_SECONDS)?;
    Ok(match npm_run(&project.dir, &script, timeout).await? {
        Ran::TimedOut => Diagnosis::TimedOut(timeout),
        Ran::Finished(output) => {
            let log = format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            Diagnosis::Ran {
                passed: output.status.success(),
                diagnostics: parse(&log),
                output: log,
            }
        }
    })
}

/// The diagnostics a log names.
fn parse(log: &str) -> Vec<Diagnostic> {
    sc_app::build_diagnostics(log)
        .iter()
        .map(|d| Diagnostic {
            file: d["file"].as_str().unwrap_or_default().to_owned(),
            line: d["line"].as_u64().unwrap_or(1),
            column: d["column"].as_u64().unwrap_or(1),
            message: d["message"].as_str().unwrap_or_default().to_owned(),
        })
        .collect()
}

/// Record the baseline before the run's first edit, once. Does nothing without
/// the `may_check` grant, or when the baseline is already recorded.
pub async fn record_baseline(
    scope: &FileScope,
    config: &Attrs,
    catalog: &Catalog,
    role: u8,
    state_json: &mut Json,
) -> Result<()> {
    let mut state = CodingState::load(state_json);
    if !may(config, CFG_MAY_CHECK) || state.baseline.is_some() {
        return Ok(());
    }
    let baseline = match diagnose(scope, config, catalog, role).await {
        Ok(Diagnosis::Ran { diagnostics, .. }) => diagnostics.iter().map(Diagnostic::key).collect(),
        // Nothing to compare against: every later diagnostic is new.
        _ => Vec::new(),
    };
    state.baseline = Some(baseline);
    state.store(state_json);
    Ok(())
}

/// Format and diagnose the files this turn edited, and say what happened. `None`
/// when the turn edited nothing or the grant is off.
pub async fn after_edits(
    scope: &FileScope,
    config: &Attrs,
    catalog: &Catalog,
    role: u8,
    state_json: &mut Json,
) -> Result<Option<String>> {
    let mut state = CodingState::load(state_json);
    let edited = std::mem::take(&mut state.turn_edits);
    state.store(state_json);
    if edited.is_empty() || !may(config, CFG_MAY_CHECK) {
        return Ok(None);
    }

    let mut report = Vec::new();
    if let Some(formatted) = format(scope, catalog, &edited, &mut state).await? {
        report.push(formatted);
    }
    state.store(state_json);

    let script = diagnose_script(config);
    let edited_rel: Vec<String> = edited.iter().map(|p| scope.relative(p)).collect();
    match diagnose(scope, config, catalog, role).await? {
        Diagnosis::Undeclared => {}
        Diagnosis::TimedOut(seconds) => {
            report.push(format!(
                "{script}: still running after {seconds} seconds; stopped."
            ));
        }
        Diagnosis::Ran {
            passed,
            diagnostics,
            output,
        } => report.push(describe(
            &script,
            passed,
            &diagnostics,
            &output,
            state.baseline.as_deref().unwrap_or_default(),
            &edited_rel,
        )),
    }
    Ok((!report.is_empty()).then(|| report.join("\n")))
}

/// Run the project's prettier over the edited files that still exist, and
/// record the ones it changed as seen.
async fn format(
    scope: &FileScope,
    catalog: &Catalog,
    edited: &[String],
    state: &mut CodingState,
) -> Result<Option<String>> {
    let (store, _) = scope.connect(catalog).await?;
    let Some(root) = store.local_path(&scope.resolve("")?)? else {
        return Ok(None);
    };
    let prettier = root.join("node_modules").join(".bin").join("prettier");
    if !prettier.exists() {
        return Ok(None);
    }
    let mut files: Vec<(String, String, Vec<u8>)> = Vec::new();
    for path in edited {
        if let Some(stat) = store.stat(path).await?
            && !stat.is_dir
        {
            files.push((
                path.clone(),
                scope.relative(path),
                store.read(path).await?.to_vec(),
            ));
        }
    }
    if files.is_empty() {
        return Ok(None);
    }
    let program = prettier.to_string_lossy().into_owned();
    let mut args = vec!["--write", "--ignore-unknown", "--log-level", "warn"];
    args.extend(files.iter().map(|(_, rel, _)| rel.as_str()));
    if let Ran::TimedOut = run_bounded(&root, &program, &args, PRETTIER_TIMEOUT_SECONDS).await? {
        return Ok(Some(
            "prettier: still running after 60 seconds; stopped.".to_owned(),
        ));
    }
    let mut changed = Vec::new();
    for (path, rel, before) in files {
        let after = store.read(&path).await?;
        if after.as_ref() != before.as_slice() {
            state.saw(&path, &after);
            changed.push(rel);
        }
    }
    Ok((!changed.is_empty()).then(|| format!("Formatted by prettier: {}.", changed.join(", "))))
}

/// The report of one diagnose run: pass or fail, the counts, and the capped
/// diagnostics, edited files first, each marked new or pre-existing.
fn describe(
    script: &str,
    passed: bool,
    diagnostics: &[Diagnostic],
    output: &str,
    baseline: &[String],
    edited: &[String],
) -> String {
    if passed && diagnostics.is_empty() {
        return format!("{script}: passed.");
    }
    // A diagnostic is new when there are more with its key than the baseline had.
    let mut budget: BTreeMap<&str, usize> = BTreeMap::new();
    for key in baseline {
        *budget.entry(key.as_str()).or_default() += 1;
    }
    let keys: Vec<String> = diagnostics.iter().map(Diagnostic::key).collect();
    let marked: Vec<(bool, &Diagnostic)> = diagnostics
        .iter()
        .zip(&keys)
        .map(|(d, key)| match budget.get_mut(key.as_str()) {
            Some(left) if *left > 0 => {
                *left -= 1;
                (false, d)
            }
            _ => (true, d),
        })
        .collect();
    let new = marked.iter().filter(|(n, _)| *n).count();
    let mut out = format!(
        "{script}: {}, {new} new, {} pre-existing.",
        if passed {
            "passed with diagnostics"
        } else {
            "failed"
        },
        marked.len() - new
    );
    if marked.is_empty() {
        let tail_text = tail(output);
        let short: String = tail_text
            .chars()
            .skip(tail_text.chars().count().saturating_sub(MAX_TAIL_CHARS))
            .collect();
        out.push_str(&format!(" Its output ends:\n{}", short.trim()));
        return out;
    }
    let mut ordered = marked;
    // Stable: edited files first, then new before pre-existing.
    ordered.sort_by_key(|(new, d)| (!edited.contains(&d.file), !*new));
    for (new, d) in ordered.iter().take(MAX_DIAGNOSTICS) {
        out.push_str(&format!(
            "\n{}:{}:{}: {} ({})",
            d.file,
            d.line,
            d.column,
            d.message,
            if *new { "new" } else { "pre-existing" }
        ));
    }
    if ordered.len() > MAX_DIAGNOSTICS {
        out.push_str(&format!(
            "\n[{} more not shown]",
            ordered.len() - MAX_DIAGNOSTICS
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(file: &str, line: u64, message: &str) -> Diagnostic {
        Diagnostic {
            file: file.to_owned(),
            line,
            column: 1,
            message: message.to_owned(),
        }
    }

    #[test]
    fn diagnostics_are_marked_new_against_the_baseline_and_edited_files_come_first() {
        let old = diag("src/lib.ts", 3, "TS1: old");
        let baseline = vec![old.key()];
        let found = vec![
            // The pre-existing one, moved by an edit.
            diag("src/lib.ts", 9, "TS1: old"),
            diag("src/lib.ts", 10, "TS2: also in lib"),
            diag("src/App.tsx", 4, "TS3: caused by the edit"),
        ];
        let out = describe(
            "typecheck",
            false,
            &found,
            "",
            &baseline,
            &["src/App.tsx".to_owned()],
        );
        assert_eq!(
            out,
            "typecheck: failed, 2 new, 1 pre-existing.\n\
             src/App.tsx:4:1: TS3: caused by the edit (new)\n\
             src/lib.ts:10:1: TS2: also in lib (new)\n\
             src/lib.ts:9:1: TS1: old (pre-existing)"
        );
    }

    #[test]
    fn a_pass_is_one_line_and_a_failure_without_diagnostics_shows_the_output() {
        assert_eq!(
            describe("typecheck", true, &[], "ok", &[], &[]),
            "typecheck: passed."
        );
        let out = describe("typecheck", false, &[], "boom\n", &[], &[]);
        assert_eq!(
            out,
            "typecheck: failed, 0 new, 0 pre-existing. Its output ends:\nboom"
        );
    }

    #[test]
    fn a_tsc_log_parses_to_diagnostics() {
        let found = parse("src/App.tsx(12,5): error TS2322: Type 'x' is not assignable.\n");
        assert_eq!(
            found,
            vec![Diagnostic {
                file: "src/App.tsx".to_owned(),
                line: 12,
                column: 5,
                message: "TS2322: Type 'x' is not assignable.".to_owned(),
            }]
        );
    }
}
