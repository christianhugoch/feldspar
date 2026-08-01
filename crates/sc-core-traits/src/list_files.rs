//! `list_files` — what is in a directory of a configured scope (§11.3).
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
//!   [`SearchFiles`](crate::SearchFiles) is the tool for finding something
//!   without knowing where it is.

use sc_agent::{AgentTrait, TraitCheck, TraitContext};
use sc_catalog::Catalog;
use sc_error::Result;
use sc_files::{effective_min_role, filter_visible};
use sc_llm::ToolSpec;
use sc_types::{Attrs, FormField};
use serde_json::{Value as Json, json};

use crate::files::{
    FileScope, check_scope, check_tool_name, configured_scope, optional_string_arg,
    scope_as_written, scope_fields,
};
use crate::table::arguments;

/// The directory to list.
const ARG_DIR: &str = "dir";

/// List the files and directories in one directory of a file store.
pub struct ListFiles;

/// The tool one `list_files` instance offers, derived from its scope.
pub fn tool_name(scope: &FileScope) -> String {
    format!("list_files_{}", scope.slug())
}

#[async_trait::async_trait]
impl AgentTrait for ListFiles {
    fn name(&self) -> &str {
        "list_files"
    }

    fn description(&self) -> &str {
        "List one directory of a file store"
    }

    fn config_spec(&self) -> Vec<FormField> {
        scope_fields()
    }

    async fn validate_config(&self, check: &TraitCheck<'_>) -> Result<()> {
        let scope = check_scope(check).await?;
        check_tool_name(&tool_name(&scope))
    }

    fn tools(&self, _catalog: &Catalog, config: &Attrs) -> Vec<ToolSpec> {
        let scope = scope_as_written(config);
        vec![ToolSpec::new(
            tool_name(&scope),
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
