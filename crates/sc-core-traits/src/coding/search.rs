//! `search_files` — grep the configured scope, server-side (§11.3).
//!
//! The tool that makes the others usable. A model asked to change how something
//! works does not know which file it is in, and the alternative to a search is
//! reading directories until it finds one — which costs a turn each and fills the
//! conversation with listings.
//!
//! The search itself is [`sc_files::search_store`], which is also what the
//! admin API's `searchFiles` endpoint (and therefore the IDE's find-in-files)
//! runs. One implementation, so what a person finds in the editor is what the
//! model finds in the same store: a search that disagreed with the editor about
//! what is in a file would be worse than no search at all.

use sc_agent::TraitContext;
use sc_error::Result;
use sc_files::{DEFAULT_EXCLUDED_DIRS, DEFAULT_MAX_RESULTS, SearchQuery, search_store};
use sc_llm::ToolSpec;
use sc_types::Attrs;
use serde_json::{Value as Json, json};

use crate::files::{
    FileScope, config_count, optional_bool_arg, optional_count_arg, optional_string_arg, string_arg,
};
use crate::table::arguments;

/// The ceiling on matches one call may return.
pub const CFG_MAX_RESULTS: &str = "max_results";

/// What to look for.
const ARG_PATTERN: &str = "pattern";
/// Whether that is a regular expression.
const ARG_REGEX: &str = "regex";
/// Whether case matters.
const ARG_CASE: &str = "case_sensitive";
/// Which files to look in.
const ARG_GLOB: &str = "glob";
/// Where to start.
const ARG_DIR: &str = "dir";
/// How many matches at most.
const ARG_MAX: &str = "max_results";

/// The tool one configured scope offers, derived from it.
pub fn tool_name(scope: &FileScope) -> String {
    format!("search_files_{}", scope.slug())
}

/// The tool this scope's search contributes.
pub fn spec(scope: &FileScope, config: &Attrs) -> ToolSpec {
    let ceiling =
        config_count(config, CFG_MAX_RESULTS, DEFAULT_MAX_RESULTS as u64).unwrap_or(u64::MAX);
    let skipped = DEFAULT_EXCLUDED_DIRS.join("`, `");
    ToolSpec::new(
        tool_name(scope),
        format!(
            "Search the text files of {} and return every matching line with \
             its path, line number and the line itself — at most {ceiling} \
             matches, and it says when there were more. This is how to find \
             where something is defined or used without knowing the file. \
             `{skipped}` are not descended into, and binary files are \
             skipped.",
            scope.label()
        ),
        json!({
            "type": "object",
            "properties": {
                ARG_PATTERN: {
                    "type": "string",
                    "description":
                        "The text to find. Literal unless `regex` is set.",
                },
                ARG_REGEX: {
                    "type": "boolean",
                    "description":
                        "Treat the pattern as a regular expression (Rust regex syntax).",
                },
                ARG_CASE: {
                    "type": "boolean",
                    "description": "Match case exactly. Off by default.",
                },
                ARG_GLOB: {
                    "type": "string",
                    "description":
                        "Only search files matching this glob: `*.ts` matches by name \
                         anywhere in the tree, `src/**/*.tsx` matches by path.",
                },
                ARG_DIR: {
                    "type": "string",
                    "description":
                        "Search only inside this directory, relative to the root of \
                         this store.",
                },
                ARG_MAX: {
                    "type": "integer",
                    "description": format!(
                        "At most this many matches. The ceiling — and the default — \
                         is {ceiling}."
                    ),
                    "minimum": 1,
                },
            },
            "required": [ARG_PATTERN],
            "additionalProperties": false,
        }),
    )
}

/// Run one search, at the run's caller's role.
pub async fn call(
    scope: &FileScope,
    config: &Attrs,
    args: &Json,
    ctx: &mut TraitContext<'_>,
) -> Result<Json> {
    let ceiling = config_count(config, CFG_MAX_RESULTS, DEFAULT_MAX_RESULTS as u64)?;
    let args = arguments(
        args,
        &[ARG_PATTERN, ARG_REGEX, ARG_CASE, ARG_GLOB, ARG_DIR, ARG_MAX],
    )?;

    let dir = optional_string_arg(&args, ARG_DIR)?;
    let glob = optional_string_arg(&args, ARG_GLOB)?;
    let query = SearchQuery {
        pattern: string_arg(&args, ARG_PATTERN)?,
        regex: optional_bool_arg(&args, ARG_REGEX, false)?,
        case_sensitive: optional_bool_arg(&args, ARG_CASE, false)?,
        whole_word: false,
        glob: (!glob.trim().is_empty()).then_some(glob),
        dir: scope.resolve(&dir)?,
        max_results: optional_count_arg(&args, ARG_MAX, ceiling)? as usize,
        ..SearchQuery::literal("")
    };

    let (store, floor) = scope.connect(ctx.catalog).await?;
    // The caller's own role, so a search cannot report a line out of a file
    // the caller could not have opened.
    let found = search_store(store.as_ref(), floor, ctx.caller.role, &query).await?;
    let matches: Vec<Json> = found
        .hits
        .iter()
        .map(|hit| {
            json!({
                "path": scope.relative(&hit.path),
                "line": hit.line,
                "column": hit.column,
                "text": hit.text,
            })
        })
        .collect();
    Ok(json!({
        "pattern": query.pattern,
        "matches": matches,
        "count": matches.len(),
        "files_searched": found.files_scanned,
        "more_matches_available": found.truncated,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tools_name_is_derived_from_its_scope() {
        let scope = FileScope {
            store: "src".to_owned(),
            root: "web".to_owned(),
        };
        assert_eq!(tool_name(&scope), "search_files_src_web");
    }
}
