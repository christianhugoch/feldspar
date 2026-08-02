//! `list_files` — what is in a directory of the configured scope (§11.3).
//!
//! The tool a model reaches for first, because it cannot read a file whose name
//! it does not know. Two properties matter:
//!
//! - **The listing is filtered, not refused.** A directory the caller may open
//!   can hold entries they may not, and §9's rule is applied per entry
//!   ([`filter_visible`](sc_files::filter_visible)) exactly as the file manager
//!   applies it — naming what the caller cannot open would leak precisely what
//!   the rule was set to hide.
//! - **One directory, not the whole tree.** A recursive listing of a project with
//!   a `node_modules` in it is tens of thousands of paths, all of which become
//!   context the conversation carries from then on.
//!   [`search`](super::search) is the tool for finding something without knowing
//!   where it is.

use sc_agent::TraitContext;
use sc_error::Result;
use sc_files::{effective_min_role, filter_visible};
use sc_llm::ToolSpec;
use serde_json::{Value as Json, json};

use crate::files::{FileScope, optional_string_arg};
use crate::table::arguments;

/// The directory to list.
const ARG_DIR: &str = "dir";

/// The tool one configured scope offers, derived from it.
pub fn tool_name(scope: &FileScope) -> String {
    format!("list_files_{}", scope.slug())
}

/// The tool this scope's listing contributes.
pub fn spec(scope: &FileScope) -> ToolSpec {
    ToolSpec::new(
        tool_name(scope),
        format!(
            "List the direct contents of one directory in {}. `{ARG_DIR}` is \
             relative to that directory; omit it for the root. Returns each \
             entry's path, whether it is a directory, and a file's size. It \
             does not recurse — list a sub-directory to see inside it.",
            scope.label()
        ),
        json!({
            "type": "object",
            "properties": {
                ARG_DIR: {
                    "type": "string",
                    "description":
                        "The directory to list, relative to the root of this store. \
                         Omit for the root itself.",
                },
            },
            "additionalProperties": false,
        }),
    )
}

/// List one directory, filtered to what the run's caller may open.
pub async fn call(scope: &FileScope, args: &Json, ctx: &mut TraitContext<'_>) -> Result<Json> {
    let args = arguments(args, &[ARG_DIR])?;
    let rel = optional_string_arg(&args, ARG_DIR)?;

    let (store, floor) = scope.connect(ctx.catalog).await?;
    let dir = scope.resolve(&rel)?;
    sc_files::check_access(store.as_ref(), floor, &dir, ctx.caller.role).await?;

    // The directory's own floor covers every child, so it is computed once
    // rather than re-walked per entry — the same shape `browseFiles` uses.
    let dir_floor = effective_min_role(store.as_ref(), floor, &dir).await?;
    let entries = store.list(&dir).await?;
    let visible = filter_visible(store.as_ref(), dir_floor, entries, ctx.caller.role).await?;
    let entries: Vec<Json> = visible
        .iter()
        .map(|entry| {
            json!({
                "path": scope.relative(&entry.path),
                "name": entry.name,
                "is_directory": entry.is_dir,
                "size": entry.size,
            })
        })
        .collect();
    Ok(json!({
        "dir": rel,
        "entries": entries,
        "count": entries.len(),
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
        assert_eq!(tool_name(&scope), "list_files_src_web");
    }
}
