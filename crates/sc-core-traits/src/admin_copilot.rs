//! `admin_copilot` — the agent that builds the application (§11.3, TODO Phase 7).
//!
//! The first **app-building** trait, and a deliberate revision of the boundary
//! the earlier traits drew. Everything before it reaches *rows*; this one reaches
//! the **catalog** and the **trigger set**: asked to "create the database schema
//! for a law firm's ERP system" it creates the connected tables and their fields
//! in one act; asked to "email the client when a matter closes" it writes the
//! trigger that does it. It edits what is already there — including, under its
//! own grant, the access rules of §7.3 — deletes what it is granted to delete,
//! and answers questions about either without ever seeing a row.
//!
//! Three things make it unlike every other built-in, each stated here because
//! each is a rule broken on purpose:
//!
//! - **It names no table in its configuration.** Every other trait does, because
//!   "which tables may this agent see?" must be answerable off the agent's
//!   definition. This one *cannot*: the tables it makes do not exist when it is
//!   configured. So it is scoped by **what it may do** rather than by what it may
//!   reach — four checkboxes ([`CFG_ALLOW_CREATE`] and its siblings) — and that
//!   difference is the phase's most load-bearing deviation.
//! - **Its grants are configuration rather than separate traits.** `insert_row`,
//!   `update_rows` and `delete_rows` are three traits over one table; these four
//!   are booleans on one trait, because the operations share a **batch**:
//!   creating `matters` with a key to an existing `clients` is a create *and* an
//!   edit, and a batch that half-applied for want of a grant is the state the
//!   transaction exists to avoid. A batch containing an ungranted operation is
//!   refused **whole**, naming the operation and the checkbox that would allow it.
//! - **The caller must be an admin.** Every other trait leans on §7.3 to decide
//!   what a caller may see; a schema has no ownership formula to fall back on,
//!   and the admin API guards every catalog endpoint with `admin()`. So every
//!   tool here refuses a run whose [`RunCaller`](sc_agent::RunCaller) is not
//!   role 1 — otherwise an agent exposed to a role-80 user through a chat view
//!   would hand them the table editor.
//!
//! ## The four grants, over both halves
//!
//! The same four checkboxes scope the schema tools and the trigger tools, and
//! they are read the same way in both: creating a table and creating a trigger
//! are both [`CFG_ALLOW_CREATE`], deleting a trigger is [`CFG_ALLOW_DROP`]
//! alongside dropping a table, and a trigger's `min_role` — which decides who may
//! `POST /actions/{name}` — is an **access rule**, so it needs
//! [`CFG_ALLOW_ACCESS`] exactly as a table's role floors do. A second set of
//! checkboxes for the trigger half would have been four more decisions for the
//! admin to make, on the same question, with the same right answers.
//!
//! ## Why the schema editor takes a list and the trigger editor does not
//!
//! [`edit_schema`](TOOL_EDIT) takes an **ordered list of operations** because a
//! schema is a set of *connected* tables: a per-operation tool turns a
//! twelve-table ERP into forty round trips, and a foreign key may point at a
//! table created earlier in the same list. One list is one turn, one transaction
//! and one refusal.
//!
//! [`save_trigger`](TOOL_SAVE_TRIGGER) takes **one trigger** because triggers are
//! not connected: two of them are two independent rows, nothing in one resolves
//! against the other, and a batch would buy an all-or-nothing guarantee nobody
//! needs while making every refusal ambiguous about which trigger caused it.
//!
//! ## The hard part: an action's settings (decision recorded here)
//!
//! A trigger is *one event plus one configured action*, and the configuration is
//! the difficult half: there is an open-ended set of actions, each declaring its
//! own [`config_spec`](sc_action::Action::config_spec) — and `send_email`'s spec
//! is not even fixed, since it grows a checkbox per File field of the trigger's
//! table. Putting every action's every setting into one tool's JSON schema would
//! be an enormous, mostly-irrelevant declaration re-sent on every model call of
//! every conversation, and would go stale the moment a plugin registers an action.
//!
//! **Saltcorn 1 solved this with a nested inference call**: `create_action` chose
//! the action and the trigger conditions, and a *second*, ad-hoc model call —
//! with a tool built for that one action — filled in its parameters. v2 does
//! **progressive disclosure inside the one loop** instead:
//! [`describe_action`](TOOL_DESCRIBE_ACTION) hands back one action's settings
//! when the model asks for them, `save_trigger` takes `configuration` as an open
//! object, and a configuration that does not validate comes back as a refusal
//! **carrying the settings it should have used**.
//!
//! Four reasons, in the order they mattered:
//!
//! - **The parameters are exactly what the conversation decides.** A nested call
//!   has to be re-briefed, and it is re-briefed by the model that is about to
//!   guess: "email the client, not the fee earner" lives in the transcript the
//!   sub-call cannot see. That is the same argument
//!   [`sc_agent::delegate`](sc_agent::delegate) makes for why a sub-agent does
//!   not inherit its parent's context — read the other way round. Delegation pays
//!   for itself when the child's *work* is long and noisy; filling in one form is
//!   neither.
//! - **A refusal has to reach the model that can fix it.** Validation of a
//!   trigger is real ([`validate_trigger`](sc_action::validate_trigger) resolves
//!   every formula in the scope the event will give it), so the first attempt is
//!   often wrong. In one loop that is a tool result and the next turn corrects
//!   it. Inside a nested call it is either an error nobody can attribute or a
//!   retry loop no one can see.
//! - **A hidden second inference is a run nobody can read.** §11 is built on a
//!   run being a transcript with one subject, a step budget and a row in
//!   `_sc_runs`. A tool that quietly calls the model again has none of those, and
//!   would be the one place in the system where tokens are spent off the record.
//!   If a task genuinely wants its own context window, `subagent` already does
//!   that — visibly, with a run of its own.
//! - **It costs less.** Saltcorn 1's flow spends two inferences on every action,
//!   always. This spends one extra *tool* round trip, only when the model does
//!   not already know the settings — and because the refusal carries the settings
//!   with it, a model that guesses well pays nothing at all.
//!
//! The one thing kept from Saltcorn 1's design is the sequencing it was reaching
//! for: **choose the action first, then configure it**. `describe_action`'s two
//! levels (every action's name and one line; then one action's full settings) are
//! that sequence made explicit and cheap.

mod schema;
mod triggers;

use sc_agent::{AgentTrait, TraitCheck, TraitContext};
use sc_api::schema_edit::{self, Grants};
use sc_catalog::Catalog;
use sc_error::{Error, Result};
use sc_llm::ToolSpec;
use sc_types::{Attrs, BasicType, FormField};
use serde_json::{Map, Value as Json};

pub use triggers::{
    TOOL_DELETE_TRIGGER, TOOL_DESCRIBE_ACTION, TOOL_DESCRIBE_TRIGGERS, TOOL_SAVE_TRIGGER,
};

/// May create tables, fields and triggers.
pub const CFG_ALLOW_CREATE: &str = schema_edit::GRANT_CREATE;
/// May change what is already there.
pub const CFG_ALLOW_EDIT: &str = schema_edit::GRANT_EDIT;
/// May drop tables and fields, and delete triggers. Off by default.
pub const CFG_ALLOW_DROP: &str = schema_edit::GRANT_DROP;
/// May write the access rules of §7.3, and a trigger's `min_role`. Off by
/// default, and above `allow_drop` — a drop announces itself and a widened role
/// floor does not.
pub const CFG_ALLOW_ACCESS: &str = schema_edit::GRANT_ACCESS_CHANGES;

/// The reading tool's name. Fixed rather than derived, because this trait is
/// configured against no table to derive one from — which is also what makes a
/// second `admin_copilot` on one agent refusable on save (§11.2): the two
/// instances offer the same names, and the collision check refuses that where
/// it is fixable rather than leaving the model to pick between duplicates.
pub const TOOL_DESCRIBE: &str = "describe_schema";
/// The writing tool's name.
pub const TOOL_EDIT: &str = "edit_schema";

/// Build and inspect the schema and the triggers over it.
pub struct AdminCopilot;

/// The tools this trait offers — all six of them, under fixed names.
pub fn tool_names() -> [&'static str; 6] {
    [
        TOOL_DESCRIBE,
        TOOL_EDIT,
        TOOL_DESCRIBE_TRIGGERS,
        TOOL_DESCRIBE_ACTION,
        TOOL_SAVE_TRIGGER,
        TOOL_DELETE_TRIGGER,
    ]
}

#[async_trait::async_trait]
impl AgentTrait for AdminCopilot {
    fn name(&self) -> &str {
        "admin_copilot"
    }

    fn description(&self) -> &str {
        "Read and change the database schema and the triggers over it: create, \
         alter and drop tables and fields, and configure the actions that run \
         when something happens"
    }

    fn config_spec(&self) -> Vec<FormField> {
        vec![
            FormField::new(CFG_ALLOW_CREATE, BasicType::Bool)
                .label("May create tables, fields and triggers")
                .default_value(true),
            FormField::new(CFG_ALLOW_EDIT, BasicType::Bool)
                .label("May change existing tables, fields and triggers")
                .default_value(true),
            FormField::new(CFG_ALLOW_DROP, BasicType::Bool)
                .label("May drop tables and fields, and delete triggers")
                .default_value(false),
            FormField::new(CFG_ALLOW_ACCESS, BasicType::Bool)
                .label(
                    "May change access rules (roles, ownership formula, row-level \
                     security, a trigger's minimum role)",
                )
                .default_value(false),
        ]
    }

    /// There is no table to resolve, so there is little here the spec cannot
    /// already say — which is itself the deviation §11.3 records. What is worth
    /// stating is that a configuration granting nothing is *not* an error: the
    /// grants bound the **writing** tools only, and an agent with none of them is
    /// a read-only describer of the schema and the triggers, which is a thing an
    /// admin may deliberately want.
    async fn validate_config(&self, check: &TraitCheck<'_>) -> Result<()> {
        for key in [
            CFG_ALLOW_CREATE,
            CFG_ALLOW_EDIT,
            CFG_ALLOW_DROP,
            CFG_ALLOW_ACCESS,
        ] {
            match check.config.get(key) {
                None | Some(Json::Null) | Some(Json::Bool(_)) => {}
                Some(other) => {
                    return Err(Error::invalid(format!(
                        "`{key}` should be true or false, got {other}"
                    )));
                }
            }
        }
        Ok(())
    }

    fn tools(&self, catalog: &Catalog, config: &Attrs) -> Vec<ToolSpec> {
        let grants = grants(config);
        let rls = catalog.primary().capabilities().row_level_security;
        vec![
            ToolSpec::new(
                TOOL_DESCRIBE,
                schema::describe_description(catalog),
                schema::describe_parameters(),
            ),
            ToolSpec::new(
                TOOL_EDIT,
                schema::edit_description(&grants, rls),
                schema::edit_parameters(),
            ),
            ToolSpec::new(
                TOOL_DESCRIBE_TRIGGERS,
                triggers::describe_triggers_description(),
                triggers::describe_triggers_parameters(),
            ),
            ToolSpec::new(
                TOOL_DESCRIBE_ACTION,
                triggers::describe_action_description(),
                triggers::describe_action_parameters(),
            ),
            ToolSpec::new(
                TOOL_SAVE_TRIGGER,
                triggers::save_description(&grants),
                triggers::save_parameters(),
            ),
            ToolSpec::new(
                TOOL_DELETE_TRIGGER,
                triggers::delete_description(&grants),
                triggers::delete_parameters(),
            ),
        ]
    }

    async fn call(
        &self,
        config: &Attrs,
        tool: &str,
        args: &Json,
        ctx: &mut TraitContext<'_>,
    ) -> Result<Json> {
        require_admin(ctx, tool)?;
        let grants = grants(config);
        match tool {
            TOOL_DESCRIBE => schema::describe(ctx.catalog, args),
            TOOL_EDIT => schema::edit(ctx.catalog, config, args).await,
            TOOL_DESCRIBE_TRIGGERS => triggers::describe_triggers(ctx, args).await,
            TOOL_DESCRIBE_ACTION => triggers::describe_action(ctx, args).await,
            TOOL_SAVE_TRIGGER => triggers::save(ctx, &grants, args).await,
            TOOL_DELETE_TRIGGER => triggers::delete(ctx, &grants, args).await,
            other => Err(Error::invalid(format!(
                "this trait offers {}, not `{other}`",
                tool_names().map(|name| format!("`{name}`")).join(", ")
            ))),
        }
    }
}

/// The four grants as configured; an absent checkbox reads as its default.
fn grants(config: &Attrs) -> Grants {
    let flag =
        |key: &str, default: bool| config.get(key).and_then(Json::as_bool).unwrap_or(default);
    Grants {
        create: flag(CFG_ALLOW_CREATE, true),
        edit: flag(CFG_ALLOW_EDIT, true),
        drop: flag(CFG_ALLOW_DROP, false),
        access_changes: flag(CFG_ALLOW_ACCESS, false),
    }
}

/// Refuse a run whose caller is not an admin, in words the model can relay.
///
/// The check is on the **run's** caller, not on the agent's `min_role`: an agent
/// may be reachable at role 80 for everything else it does and still must not
/// hand that caller the schema.
fn require_admin(ctx: &TraitContext<'_>, tool: &str) -> Result<()> {
    if ctx.caller.role == 1 {
        return Ok(());
    }
    Err(Error::invalid(format!(
        "`{tool}` is only available to an administrator, and this conversation is \
         with a role-{} user. Tell them the schema and the triggers over it can \
         only be seen or changed by an admin.",
        ctx.caller.role
    )))
}

/// Refuse an operation the admin did not tick the box for, naming the box.
///
/// The sibling of [`schema_edit`]'s own grant check, kept separate because that
/// one says "the whole batch was refused" — true of a list of schema operations
/// and untrue of one trigger.
fn require_grant(granted: bool, what: &str, key: &str) -> Result<()> {
    if granted {
        return Ok(());
    }
    Err(Error::invalid(format!(
        "not permitted to {what}; nothing was changed. Turn on `{key}` in this \
         agent's `admin_copilot` settings to allow it."
    )))
}

fn optional_string(obj: &Map<String, Json>, key: &str) -> Result<Option<String>> {
    match obj.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(Error::invalid(format!(
            "`{key}` should be a string, got {other}"
        ))),
    }
}

fn optional_bool(obj: &Map<String, Json>, key: &str) -> Result<Option<bool>> {
    match obj.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::Bool(b)) => Ok(Some(*b)),
        Some(other) => Err(Error::invalid(format!(
            "`{key}` should be true or false, got {other}"
        ))),
    }
}

fn optional_role(obj: &Map<String, Json>, key: &str) -> Result<Option<u8>> {
    match obj.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::Number(n)) => n
            .as_i64()
            .and_then(|n| u8::try_from(n).ok())
            .filter(|r| (1..=100).contains(r))
            .map(Some)
            .ok_or_else(|| {
                Error::invalid(format!(
                    "`{key}` should be a role between 1 and 100, got {n}"
                ))
            }),
        Some(other) => Err(Error::invalid(format!(
            "`{key}` should be a number between 1 and 100, got {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(pairs: &[(&str, bool)]) -> Attrs {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), json!(v)))
            .collect()
    }

    #[test]
    fn dropping_and_access_changes_are_off_unless_asked_for() {
        // An empty configuration is the safe one: it can build, it cannot
        // destroy, and it cannot widen anybody's access.
        let g = grants(&Attrs::new());
        assert!(g.create && g.edit);
        assert!(!g.drop && !g.access_changes);

        let g = grants(&config(&[(CFG_ALLOW_DROP, true), (CFG_ALLOW_ACCESS, true)]));
        assert!(g.drop && g.access_changes);

        let g = grants(&config(&[
            (CFG_ALLOW_CREATE, false),
            (CFG_ALLOW_EDIT, false),
        ]));
        assert!(!g.create && !g.edit);
    }

    #[test]
    fn an_ungranted_operation_names_the_checkbox_that_would_allow_it() {
        let err = require_grant(false, "create a trigger", CFG_ALLOW_CREATE)
            .unwrap_err()
            .to_string();
        assert!(err.contains("create a trigger"), "{err}");
        assert!(err.contains(CFG_ALLOW_CREATE), "{err}");
        // The message an agent relays says nothing was changed, because for one
        // trigger that is the whole truth — there is no half-applied batch.
        assert!(err.contains("nothing was changed"), "{err}");
        assert!(require_grant(true, "create a trigger", CFG_ALLOW_CREATE).is_ok());
    }
}
