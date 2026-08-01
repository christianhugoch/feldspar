//! `read_file` — read one text file out of a configured scope (§11.3).
//!
//! The first of the coding traits and the simplest, but two of its decisions are
//! the ones the rest inherit:
//!
//! - **It returns text, not bytes.** A model cannot act on a PNG, and base64 of
//!   one is context it pays for and cannot use. A file that is not UTF-8 comes
//!   back as an error saying so, with its size — which is a fact the model can
//!   use ("that is an image, I will not try again").
//! - **It is bounded and says when it truncated.** A tool result becomes part of
//!   every subsequent turn, so an unbounded read is a conversation that gets more
//!   expensive with each file opened. The configured ceiling is characters, the
//!   model may ask for less, and a truncated read reports the fact rather than
//!   letting the model reason about a file it has only the first half of.

use sc_agent::{AgentTrait, TraitCheck, TraitContext};
use sc_catalog::Catalog;
use sc_error::{Error, Result};
use sc_llm::ToolSpec;
use sc_types::{Attrs, BasicType, FormField};
use serde_json::{Value as Json, json};

use crate::files::{
    ARG_PATH, FileScope, check_scope, check_tool_name, config_count, configured_scope, open_at,
    optional_count_arg, scope_as_written, scope_fields, string_arg,
};
use crate::table::arguments;

/// The ceiling on characters returned, when the admin sets none.
pub const CFG_MAX_CHARS: &str = "max_chars";

/// Bigger than most source files and small enough that a stray minified bundle
/// does not become the conversation.
pub const DEFAULT_MAX_CHARS: u64 = 60_000;

/// How many characters at most, this call.
const ARG_MAX_CHARS: &str = "max_chars";

/// Read one file from a configured file store.
pub struct ReadFile;

/// The tool one `read_file` instance offers, derived from its scope.
pub fn tool_name(scope: &FileScope) -> String {
    format!("read_file_{}", scope.slug())
}

#[async_trait::async_trait]
impl AgentTrait for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read a text file from one file store"
    }

    fn config_spec(&self) -> Vec<FormField> {
        let mut spec = scope_fields();
        spec.push(
            FormField::new(CFG_MAX_CHARS, BasicType::Int)
                .label("Maximum characters")
                .default_value(DEFAULT_MAX_CHARS as i64),
        );
        spec
    }

    async fn validate_config(&self, check: &TraitCheck<'_>) -> Result<()> {
        let scope = check_scope(check).await?;
        config_count(check.config, CFG_MAX_CHARS, DEFAULT_MAX_CHARS)?;
        check_tool_name(&tool_name(&scope))
    }

    fn tools(&self, _catalog: &Catalog, config: &Attrs) -> Vec<ToolSpec> {
        let scope = scope_as_written(config);
        let ceiling = config_count(config, CFG_MAX_CHARS, DEFAULT_MAX_CHARS).unwrap_or(u64::MAX);
        vec![ToolSpec::new(
            tool_name(&scope),
            format!(
                "Read a text file from {}. `{ARG_PATH}` is relative to that \
                 directory (`src/App.tsx`), never absolute and never containing \
                 `..`. Returns the file's text, at most {ceiling} characters, and \
                 says whether it was truncated.",
                scope.label()
            ),
            json!({
                "type": "object",
                "properties": {
                    ARG_PATH: {
                        "type": "string",
                        "description": "The file to read, relative to the root of this store.",
                    },
                    ARG_MAX_CHARS: {
                        "type": "integer",
                        "description": format!(
                            "At most this many characters. The ceiling — and the default \
                             — is {ceiling}."
                        ),
                        "minimum": 1,
                    },
                },
                "required": [ARG_PATH],
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
        let ceiling = config_count(config, CFG_MAX_CHARS, DEFAULT_MAX_CHARS)?;
        let args = arguments(args, &[ARG_PATH, ARG_MAX_CHARS])?;
        let rel = string_arg(&args, ARG_PATH)?;
        let limit = optional_count_arg(&args, ARG_MAX_CHARS, ceiling)? as usize;

        let (store, path) = open_at(&scope, ctx, &rel).await?;
        let bytes = store.read(&path).await?;
        let text = std::str::from_utf8(&bytes).map_err(|_| {
            Error::invalid(format!(
                "`{rel}` is not a text file ({} bytes of binary data)",
                bytes.len()
            ))
        })?;

        let truncated = text.chars().count() > limit;
        let text: String = text.chars().take(limit).collect();
        Ok(json!({
            "path": rel,
            "text": text,
            "bytes": bytes.len(),
            "truncated": truncated,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tools_name_is_derived_from_its_scope() {
        let scope = FileScope {
            store: "app-src".to_owned(),
            root: "web".to_owned(),
        };
        assert_eq!(tool_name(&scope), "read_file_app_src_web");
    }

    #[test]
    fn a_ceiling_the_admin_did_not_set_is_the_default() {
        assert_eq!(
            config_count(&Attrs::new(), CFG_MAX_CHARS, DEFAULT_MAX_CHARS).unwrap(),
            DEFAULT_MAX_CHARS
        );
        let config: Attrs = [(CFG_MAX_CHARS.to_owned(), json!(120))]
            .into_iter()
            .collect();
        assert_eq!(
            config_count(&config, CFG_MAX_CHARS, DEFAULT_MAX_CHARS).unwrap(),
            120
        );
        // Zero characters is not a small read, it is a malformed setting.
        let config: Attrs = [(CFG_MAX_CHARS.to_owned(), json!(0))].into_iter().collect();
        assert!(config_count(&config, CFG_MAX_CHARS, DEFAULT_MAX_CHARS).is_err());
    }
}
