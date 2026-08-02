//! `edit_file` — exact-string replacement in one file (§11.3).
//!
//! The edit that can be **verified before it is applied**, and the reason this
//! milestone ships it rather than a diff or a line-range replacement: the model
//! sends the text it believes is there and the text it wants instead, and the
//! server checks the belief. Three outcomes, and each of them is deliberate:
//!
//! - **Exactly one occurrence** — replaced, and the file is written.
//! - **None** — an error saying the text was not found, and (when the file is
//!   readable) how many lines it has, so the model knows whether it is looking
//!   at the wrong file or has misremembered its contents. A fuzzy match here
//!   would be a corrupted file nobody noticed until much later.
//! - **More than one** — an error saying how many, because "replace the first
//!   one" is a coin flip and "replace them all" is a change the model did not
//!   ask for. The instruction back is the recoverable one: include more
//!   surrounding text.
//!
//! `replace_all` exists as an explicit argument for the case where changing every
//! occurrence *is* the intent (renaming an identifier through a file). It is
//! opt-in, so ambiguity is never resolved silently.

use sc_agent::TraitContext;
use sc_error::{Error, Result};
use sc_llm::ToolSpec;
use serde_json::{Value as Json, json};

use crate::files::{ARG_PATH, FileScope, open_at, optional_bool_arg, string_arg};
use crate::table::arguments;

/// The text to find, exactly.
const ARG_FIND: &str = "find";
/// What to put in its place.
const ARG_REPLACE: &str = "replace";
/// Whether every occurrence is meant.
const ARG_ALL: &str = "replace_all";

/// The tool one configured scope offers, derived from it.
pub fn tool_name(scope: &FileScope) -> String {
    format!("edit_file_{}", scope.slug())
}

/// The tool this scope's exact-string edit contributes.
pub fn spec(scope: &FileScope) -> ToolSpec {
    ToolSpec::new(
        tool_name(scope),
        format!(
            "Change part of a file in {} by replacing an exact string. \
             `{ARG_FIND}` must appear **exactly once** in the file, \
             character for character including indentation and line breaks; \
             if it appears more than once the call is refused and you should \
             include more of the surrounding lines to make it unique, and if \
             it does not appear at all the file is not what you think it is — \
             read it again. Set `{ARG_ALL}` only when you mean every \
             occurrence.",
            scope.label()
        ),
        json!({
            "type": "object",
            "properties": {
                ARG_PATH: {
                    "type": "string",
                    "description": "The file to edit, relative to the root of this store.",
                },
                ARG_FIND: {
                    "type": "string",
                    "description":
                        "The exact text to replace, as it appears in the file.",
                },
                ARG_REPLACE: {
                    "type": "string",
                    "description":
                        "The text to put in its place. An empty string deletes it.",
                },
                ARG_ALL: {
                    "type": "boolean",
                    "description":
                        "Replace every occurrence instead of requiring exactly one.",
                },
            },
            "required": [ARG_PATH, ARG_FIND, ARG_REPLACE],
            "additionalProperties": false,
        }),
    )
}

/// Apply one exact-string edit, as the run's caller.
pub async fn call(scope: &FileScope, args: &Json, ctx: &mut TraitContext<'_>) -> Result<Json> {
    let args = arguments(args, &[ARG_PATH, ARG_FIND, ARG_REPLACE, ARG_ALL])?;
    let rel = string_arg(&args, ARG_PATH)?;
    let find = string_arg(&args, ARG_FIND)?;
    let replace = string_arg(&args, ARG_REPLACE)?;
    let all = optional_bool_arg(&args, ARG_ALL, false)?;

    let (store, path) = open_at(scope, ctx, &rel).await?;
    let bytes = store.read(&path).await?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| Error::invalid(format!("`{rel}` is not a text file")))?;

    let edited = apply(text, &find, &replace, all, &rel)?;
    let replacements = edited.replacements;
    store
        .write(&path, bytes::Bytes::from(edited.text.into_bytes()))
        .await?;
    Ok(json!({
        "path": rel,
        "replacements": replacements,
        "edited": true,
    }))
}

/// The result of a successful edit.
#[derive(Debug)]
struct Edited {
    text: String,
    replacements: usize,
}

/// Apply the replacement, or say why it cannot be applied.
///
/// Separated from the I/O so the three outcomes are unit-testable without a
/// store — they are the whole of this trait's judgement.
fn apply(text: &str, find: &str, replace: &str, all: bool, path: &str) -> Result<Edited> {
    if find.is_empty() {
        return Err(Error::invalid(
            "`find` is empty; it must be the exact text to replace",
        ));
    }
    let count = text.matches(find).count();
    match (count, all) {
        (0, _) => Err(Error::invalid(format!(
            "that text does not appear in `{path}` (which has {} lines). \
             Read the file and copy the text to replace from it exactly, \
             including indentation.",
            text.lines().count()
        ))),
        (1, _) | (_, true) => Ok(Edited {
            text: text.replace(find, replace),
            replacements: count,
        }),
        (many, false) => Err(Error::invalid(format!(
            "that text appears {many} times in `{path}`, so it is ambiguous. \
             Include more of the surrounding lines so it identifies one place, \
             or set `replace_all` if you mean all {many}."
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "const a = 1;\nconst b = 2;\nconst a = 3;\n";

    #[test]
    fn a_tools_name_is_derived_from_its_scope() {
        let scope = FileScope {
            store: "src".to_owned(),
            root: String::new(),
        };
        assert_eq!(tool_name(&scope), "edit_file_src");
    }

    #[test]
    fn a_unique_match_is_replaced() {
        let edited = apply(FILE, "const b = 2;", "const b = 20;", false, "a.ts").unwrap();
        assert_eq!(edited.replacements, 1);
        assert_eq!(edited.text, "const a = 1;\nconst b = 20;\nconst a = 3;\n");
    }

    #[test]
    fn a_match_that_is_not_there_is_refused_with_the_files_size() {
        let err = apply(FILE, "const c = 9;", "x", false, "a.ts").unwrap_err();
        let err = err.to_string();
        assert!(err.contains("does not appear"), "{err}");
        assert!(err.contains("a.ts"), "{err}");
        assert!(err.contains("3 lines"), "{err}");
    }

    #[test]
    fn an_ambiguous_match_is_refused_with_the_count_and_the_way_out() {
        let err = apply(FILE, "const a", "let a", false, "a.ts")
            .unwrap_err()
            .to_string();
        assert!(err.contains("2 times"), "{err}");
        assert!(err.contains("replace_all"), "{err}");

        // …and `replace_all` is that way out, taken deliberately.
        let edited = apply(FILE, "const a", "let a", true, "a.ts").unwrap();
        assert_eq!(edited.replacements, 2);
        assert_eq!(edited.text, "let a = 1;\nconst b = 2;\nlet a = 3;\n");
    }

    #[test]
    fn an_empty_find_is_refused_rather_than_matching_everywhere() {
        assert!(apply(FILE, "", "x", true, "a.ts").is_err());
    }

    #[test]
    fn an_edit_may_delete_by_replacing_with_nothing() {
        let edited = apply(FILE, "const b = 2;\n", "", false, "a.ts").unwrap();
        assert_eq!(edited.text, "const a = 1;\nconst a = 3;\n");
    }
}
