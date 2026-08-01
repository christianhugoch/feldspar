//! `write_file` — create or replace one file in a configured scope (§11.3).
//!
//! The blunt write, and a separate opt-in grant from [`ReadFile`](crate::ReadFile)
//! for the reason `insert_row` is separate from `query_table`: a read-only agent
//! is the default shape, and every way of changing something is a deliberate act
//! with a form field attached.
//!
//! It **replaces the whole file**, and says so in as many words, because that is
//! the difference between it and [`EditFile`](crate::EditFile). A model that
//! reaches for `write_file` to change one line will write the file it remembers
//! rather than the file that is there, and the paragraph in the description is
//! what steers it to the edit that cannot do that.

use sc_agent::{AgentTrait, TraitCheck, TraitContext};
use sc_catalog::Catalog;
use sc_error::Result;
use sc_llm::ToolSpec;
use sc_types::{Attrs, FormField};
use serde_json::{Value as Json, json};

use crate::files::{
    ARG_PATH, FileScope, check_scope, check_tool_name, configured_scope, open_at, scope_as_written,
    scope_fields, string_arg,
};
use crate::table::arguments;

/// The file's new contents.
const ARG_CONTENT: &str = "content";

/// Create or replace one file in a configured file store.
pub struct WriteFile;

/// The tool one `write_file` instance offers, derived from its scope.
pub fn tool_name(scope: &FileScope) -> String {
    format!("write_file_{}", scope.slug())
}

#[async_trait::async_trait]
impl AgentTrait for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Create or replace a whole file in one file store"
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
        let args = arguments(args, &[ARG_PATH, ARG_CONTENT])?;
        let rel = string_arg(&args, ARG_PATH)?;
        let content = string_arg(&args, ARG_CONTENT)?;

        let (store, path) = open_at(&scope, ctx, &rel).await?;
        let bytes = content.len();
        store
            .write(&path, bytes::Bytes::from(content.into_bytes()))
            .await?;
        Ok(json!({ "path": rel, "bytes": bytes, "written": true }))
    }
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
