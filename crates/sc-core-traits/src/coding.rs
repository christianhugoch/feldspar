//! `coding` — one trait for the whole coding loop over one file store (§11.3).
//!
//! Read, list, search, write, edit and run, in **one** grant with **one** form.
//! The six of them were six traits once, each with the same store-and-sub-directory
//! configuration, and that shape was wrong for the reason a form is wrong when it
//! asks the same question six times: an admin setting up a coding agent filled in
//! the same store and the same root six times over, and a change of mind about the
//! root was six edits, five of which could be forgotten. The capability an admin
//! actually grants is "this agent works on the code in `web/todo`", and that is
//! now one row in the traits list.
//!
//! ## What it offers, and what it takes to unlock
//!
//! Three tools are always there, and they are the read-only ones: `read_file`,
//! `list_files` and `search_files`. **Changing the source is a checkbox**
//! ([`CFG_MAY_EDIT`]), which adds `write_file` and `edit_file`, and **running a
//! script is another** ([`CFG_MAY_RUN_SCRIPTS`]), which adds `run_script`. Both
//! are off by default, so a read-only coding agent stays the default shape —
//! the property the six separate grants had and the one worth keeping. Their being
//! configuration rather than separate traits is `manage_table_admin`'s move, made
//! for the same reason: the grants share a scope, and a scope filled in twice is a
//! scope that can disagree with itself.
//!
//! A tool call whose grant is off is refused **by name, naming the checkbox** —
//! the model never sees the tool, but a stale transcript can still carry one, and
//! "you may not do that" is not something a model can act on while "the agent's
//! `may_edit` setting is off" is something its user can.
//!
//! ## And what is still its own trait
//!
//! `build_application` ([`BuildApplication`](crate::BuildApplication)) is not part
//! of this one, deliberately: it is configured on a different axis — an
//! application's subdomain, not a store — and folding it in would mean an agent
//! that builds two applications out of one source tree needed two `coding`
//! instances, which would then collide on the file tools' names.
//!
//! ## The scope
//!
//! Everything about the store, the root, path resolution and §9's access rule is
//! [`crate::files`], unchanged: the tools' names are derived from the scope
//! (`edit_file_apps_web`), so one agent may have this trait twice over two
//! directories and the same directory twice is a collision refused on save
//! (§11.2).

mod edit;
mod list;
mod read;
mod script;
mod search;
mod write;

use sc_agent::{AgentTrait, TraitCheck, TraitContext};
use sc_catalog::Catalog;
use sc_error::{Error, Result};
use sc_files::DEFAULT_MAX_RESULTS;
use sc_llm::ToolSpec;
use sc_types::{Attrs, BasicType, FormField};
use serde_json::Value as Json;

use crate::files::{
    FileScope, check_scope, check_tool_name, config_count, configured_scope, scope_as_written,
    scope_fields,
};

pub use edit::tool_name as edit_file_tool_name;
pub use list::tool_name as list_files_tool_name;
pub use read::{CFG_MAX_CHARS, DEFAULT_MAX_CHARS, tool_name as read_file_tool_name};
pub use script::{
    CFG_TIMEOUT, DEFAULT_TIMEOUT_SECONDS, MAX_OUTPUT_CHARS, tool_name as run_script_tool_name,
};
pub use search::{CFG_MAX_RESULTS, tool_name as search_files_tool_name};
pub use write::tool_name as write_file_tool_name;

/// May create and change files: adds `write_file` and `edit_file`. Off by
/// default, because a read-only agent is the shape that cannot damage anything.
pub const CFG_MAY_EDIT: &str = "may_edit";

/// May run one of the project's own `package.json` scripts. Off by default, and
/// separate from [`CFG_MAY_EDIT`] because running a script executes code the
/// agent did not write.
pub const CFG_MAY_RUN_SCRIPTS: &str = "may_run_scripts";

/// Work on the code in one file store: read it, search it, and — under its
/// grants — change it and run its scripts.
pub struct Coding;

/// Every tool this trait can offer for a scope, in the order it offers them.
///
/// The whole set regardless of the grants, because this is what the admin UI
/// wants to *show* and what a collision check compares: a tool a grant currently
/// withholds still names the same thing.
pub fn tool_names(scope: &FileScope) -> Vec<String> {
    vec![
        read::tool_name(scope),
        list::tool_name(scope),
        search::tool_name(scope),
        write::tool_name(scope),
        edit::tool_name(scope),
        script::tool_name(scope),
    ]
}

#[async_trait::async_trait]
impl AgentTrait for Coding {
    fn name(&self) -> &str {
        "coding"
    }

    fn description(&self) -> &str {
        "Work on the code in one file store: read, list and search it, and — if permitted — \
         write, edit and run its scripts"
    }

    fn config_spec(&self) -> Vec<FormField> {
        let mut spec = scope_fields();
        spec.push(
            FormField::new(CFG_MAY_EDIT, BasicType::Bool)
                .label("May create and change files")
                .default_value(false),
        );
        spec.push(
            FormField::new(CFG_MAY_RUN_SCRIPTS, BasicType::Bool)
                .label("May run the project's package.json scripts")
                .default_value(false),
        );
        spec.push(
            FormField::new(CFG_MAX_CHARS, BasicType::Int)
                .label("Maximum characters per file read")
                .default_value(DEFAULT_MAX_CHARS as i64),
        );
        spec.push(
            FormField::new(CFG_MAX_RESULTS, BasicType::Int)
                .label("Maximum search matches")
                .default_value(DEFAULT_MAX_RESULTS as i64),
        );
        spec.push(
            FormField::new(CFG_TIMEOUT, BasicType::Int)
                .label("Script timeout (seconds)")
                .default_value(DEFAULT_TIMEOUT_SECONDS as i64),
        );
        spec
    }

    /// The store exists, the root is inside it, the bounds are whole numbers and
    /// every name this scope would derive is one a provider accepts.
    ///
    /// The longest of the names is what decides the last of those: a scope whose
    /// `search_files_…` fits but whose `run_script_…` does not would otherwise
    /// pass here and fail at the vendor, which is the failure this check exists
    /// to move forward in time.
    async fn validate_config(&self, check: &TraitCheck<'_>) -> Result<()> {
        let scope = check_scope(check).await?;
        config_count(check.config, CFG_MAX_CHARS, DEFAULT_MAX_CHARS)?;
        config_count(check.config, CFG_MAX_RESULTS, DEFAULT_MAX_RESULTS as u64)?;
        config_count(check.config, CFG_TIMEOUT, DEFAULT_TIMEOUT_SECONDS)?;
        for key in [CFG_MAY_EDIT, CFG_MAY_RUN_SCRIPTS] {
            match check.config.get(key) {
                None | Some(Json::Null) | Some(Json::Bool(_)) => {}
                Some(other) => {
                    return Err(Error::invalid(format!(
                        "`{key}` should be true or false, got {other}"
                    )));
                }
            }
        }
        for name in tool_names(&scope) {
            check_tool_name(&name)?;
        }
        Ok(())
    }

    fn tools(&self, _catalog: &Catalog, config: &Attrs) -> Vec<ToolSpec> {
        let scope = scope_as_written(config);
        let mut tools = vec![
            read::spec(&scope, config),
            list::spec(&scope),
            search::spec(&scope, config),
        ];
        if may(config, CFG_MAY_EDIT) {
            tools.push(write::spec(&scope));
            tools.push(edit::spec(&scope));
        }
        if may(config, CFG_MAY_RUN_SCRIPTS) {
            tools.push(script::spec(&scope));
        }
        tools
    }

    async fn call(
        &self,
        config: &Attrs,
        tool: &str,
        args: &Json,
        ctx: &mut TraitContext<'_>,
    ) -> Result<Json> {
        let scope = configured_scope(config)?;
        match tool {
            _ if tool == read::tool_name(&scope) => read::call(&scope, config, args, ctx).await,
            _ if tool == list::tool_name(&scope) => list::call(&scope, args, ctx).await,
            _ if tool == search::tool_name(&scope) => search::call(&scope, config, args, ctx).await,
            _ if tool == write::tool_name(&scope) => {
                permit(config, CFG_MAY_EDIT, "change files", ctx)?;
                write::call(&scope, args, ctx).await
            }
            _ if tool == edit::tool_name(&scope) => {
                permit(config, CFG_MAY_EDIT, "change files", ctx)?;
                edit::call(&scope, args, ctx).await
            }
            _ if tool == script::tool_name(&scope) => {
                permit(config, CFG_MAY_RUN_SCRIPTS, "run scripts", ctx)?;
                script::call(&scope, config, args, ctx).await
            }
            other => Err(Error::invalid(format!(
                "`{other}` is not one of this trait's tools; it offers {}",
                tool_names(&scope).join(", ")
            ))),
        }
    }
}

/// Whether a grant is on. An absent checkbox reads as off, which is the reading
/// that cannot turn a forgotten field into a permission.
fn may(config: &Attrs, key: &str) -> bool {
    config.get(key).and_then(Json::as_bool).unwrap_or(false)
}

/// Refuse a tool whose grant is off, naming the setting that would allow it.
///
/// Unreachable through the tools the model is offered — a withheld tool is not
/// declared — and kept anyway, because a run resumed from a transcript that
/// carried the call is the case where "the model cannot see it" stops being the
/// enforcement.
fn permit(config: &Attrs, key: &str, what: &str, ctx: &TraitContext<'_>) -> Result<()> {
    if may(config, key) {
        return Ok(());
    }
    Err(Error::invalid(format!(
        "agent `{}` may not {what}: its `coding` trait has `{key}` switched off. \
         Tell the user an administrator must turn it on.",
        ctx.agent
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scope() -> FileScope {
        FileScope {
            store: "app-src".to_owned(),
            root: "web".to_owned(),
        }
    }

    fn config(edit: bool, run: bool) -> Attrs {
        [
            ("store".to_owned(), json!("app-src")),
            ("root".to_owned(), json!("web")),
            (CFG_MAY_EDIT.to_owned(), json!(edit)),
            (CFG_MAY_RUN_SCRIPTS.to_owned(), json!(run)),
        ]
        .into_iter()
        .collect()
    }

    /// Every tool this trait can offer is named after the scope, so one agent
    /// may have it twice over two directories (§11.2).
    #[test]
    fn every_tool_is_named_after_the_scope() {
        assert_eq!(
            tool_names(&scope()),
            [
                "read_file_app_src_web",
                "list_files_app_src_web",
                "search_files_app_src_web",
                "write_file_app_src_web",
                "edit_file_app_src_web",
                "run_script_app_src_web",
            ]
        );
    }

    /// A grant is on only when the admin ticked it. **A missing field is not a
    /// permission**, which is the reading that keeps a half-filled form from
    /// handing out an edit — and the one the integration suite then pins against
    /// the tools actually offered.
    #[test]
    fn an_unticked_or_absent_checkbox_grants_nothing() {
        assert!(may(&config(true, false), CFG_MAY_EDIT));
        assert!(!may(&config(true, false), CFG_MAY_RUN_SCRIPTS));

        let bare: Attrs = [("store".to_owned(), json!("app-src"))]
            .into_iter()
            .collect();
        assert!(!may(&bare, CFG_MAY_EDIT));
        assert!(!may(&bare, CFG_MAY_RUN_SCRIPTS));
        // …and something that is not a boolean at all is not a grant either.
        let odd: Attrs = [(CFG_MAY_EDIT.to_owned(), json!("yes"))]
            .into_iter()
            .collect();
        assert!(!may(&odd, CFG_MAY_EDIT));
    }
}
