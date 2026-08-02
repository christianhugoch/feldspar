//! `write_file` — create or replace one file in the configured scope (§11.3).
//!
//! The blunt write, offered only under the [`Coding`](super::Coding) trait's edit
//! grant for the reason `insert_row` is separate from `query_table`: a read-only
//! agent is the default shape, and the ability to change something is a
//! deliberate act with a checkbox attached.
//!
//! It **replaces the whole file**, and says so in as many words, because that is
//! the difference between it and [`edit`](super::edit). A model that reaches for
//! `write_file` to change one line will write the file it remembers rather than
//! the file that is there, and the paragraph in the description is what steers it
//! to the edit that cannot do that.

use sc_agent::TraitContext;
use sc_error::Result;
use sc_llm::ToolSpec;
use serde_json::{Value as Json, json};

use crate::files::{ARG_PATH, FileScope, open_at, string_arg};
use crate::table::arguments;

/// The file's new contents.
const ARG_CONTENT: &str = "content";

/// The tool one configured scope offers, derived from it.
pub fn tool_name(scope: &FileScope) -> String {
    format!("write_file_{}", scope.slug())
}

/// The tool this scope's whole-file write contributes.
pub fn spec(scope: &FileScope) -> ToolSpec {
    ToolSpec::new(
        tool_name(scope),
        format!(
            "Create a file in {}, or **replace one that exists in its \
             entirety**. Parent directories are created as needed. \
             `{ARG_PATH}` is relative to that directory. To change part of an \
             existing file, read it and edit it rather than rewriting it from \
             memory.",
            scope.label()
        ),
        json!({
            "type": "object",
            "properties": {
                ARG_PATH: {
                    "type": "string",
                    "description": "The file to write, relative to the root of this store.",
                },
                ARG_CONTENT: {
                    "type": "string",
                    "description": "The file's complete new contents.",
                },
            },
            "required": [ARG_PATH, ARG_CONTENT],
            "additionalProperties": false,
        }),
    )
}

/// Write one file whole, as the run's caller.
pub async fn call(scope: &FileScope, args: &Json, ctx: &mut TraitContext<'_>) -> Result<Json> {
    let args = arguments(args, &[ARG_PATH, ARG_CONTENT])?;
    let rel = string_arg(&args, ARG_PATH)?;
    let content = string_arg(&args, ARG_CONTENT)?;

    let (store, path) = open_at(scope, ctx, &rel).await?;
    let bytes = content.len();
    store
        .write(&path, bytes::Bytes::from(content.into_bytes()))
        .await?;
    Ok(json!({ "path": rel, "bytes": bytes, "written": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tools_name_is_derived_from_its_scope() {
        let scope = FileScope {
            store: "src".to_owned(),
            root: String::new(),
        };
        assert_eq!(tool_name(&scope), "write_file_src");
    }
}
