//! `admin_copilot` — the agent that builds the application (§11.3, §13.6).
//!
//! The first **app-building** trait, and a deliberate revision of the boundary
//! the earlier traits drew. Everything before it reaches *rows*; this one reaches
//! the **catalog**, the **trigger set** and an application's **custom SQL
//! queries**: asked to "create the database schema for a law firm's ERP system"
//! it creates the connected tables and their fields in one act; asked to "email
//! the client when a matter closes" it writes the trigger that does it; asked for
//! "an endpoint that returns each fee earner's billed hours" it writes the SQL and
//! the database types the answer. It edits what is already there — including,
//! under its own grant, the access rules of §7.3 — deletes what it is granted to
//! delete, and answers questions about any of the three without ever seeing a
//! row.
//!
//! ## What is here, and what is not
//!
//! **The tools themselves are not here.** They are
//! [`sc_api::mcp`]'s — assembled into one [`ToolSet`] by
//! [`sc_app::mcp::tool_set`] — because this agent is no longer their only
//! caller: the administration MCP server (§13.6) offers the same nine tools to
//! an external coding agent, under a token's grants instead of an agent's
//! checkboxes. Two callers over one implementation, rather than two
//! implementations that check grants slightly differently and drift within a
//! release — the argument [`sc_api::schema_edit`]'s module comment makes for the
//! *operation*, applied to the *tool*.
//!
//! What is left here is what an `AgentTrait` is: the name and description the
//! admin picks it by, the configuration form, the validation of that form, and
//! the translation from a run's [`TraitContext`] to a
//! [`ToolContext`](sc_api::mcp::ToolContext). Everything else delegates.
//!
//! Three things still make it unlike every other built-in, each stated here
//! because each is a rule broken on purpose:
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
//!   tool refuses a run whose [`RunCaller`](sc_agent::RunCaller) is not role 1 —
//!   otherwise an agent exposed to a role-80 user through a chat view would hand
//!   them the table editor. The check lives with the tools, because it must mean
//!   the same thing for the MCP caller.
//!
//! ## The four grants, over all three parts
//!
//! The same four checkboxes scope the schema tools, the trigger tools and the
//! application tools, and they are read the same way in each: creating a table, a
//! trigger and a custom SQL query are all [`CFG_ALLOW_CREATE`]; deleting a trigger
//! or a query is [`CFG_ALLOW_DROP`] alongside dropping a table; and a trigger's
//! `min_role` — which decides who may `POST /actions/{name}` — and a query's,
//! which decides who may call its endpoint, are **access rules**, so they need
//! [`CFG_ALLOW_ACCESS`] exactly as a table's role floors do. A second and a third
//! set of checkboxes would have been eight more decisions for the admin to make,
//! on the same question, with the same right answers.
//!
//! ## The two areas, which are the other question
//!
//! A grant says *what this agent may do*; [`CFG_ALLOW_TRIGGERS`] and
//! [`CFG_ALLOW_APPLICATIONS`] say *to which of the three it may do it*. Both
//! default on, and an area that is off takes its tools out of the model's list
//! rather than leaving them to be refused — the opposite of how a grant behaves,
//! deliberately. A model that may not drop a table still has to be able to say
//! that dropping one is what the admin asked for; an area that is off is not part
//! of this agent's job at all, and a tool the model can see is a tool it will try.
//!
//! The schema has no area of its own: it is what the trait *is*, and an
//! `admin_copilot` that may not describe a schema is an agent with no reason to
//! carry the trait.
//!
//! ## The two design decisions the tools embody
//!
//! Both are recorded where the tools now live, and named here because this is
//! where an admin reads about the agent:
//!
//! - [`edit_schema`](TOOL_EDIT) takes an **ordered list of operations** and
//!   [`save_trigger`](TOOL_SAVE_TRIGGER) takes **one trigger**, because a schema
//!   is a set of *connected* tables and two triggers are two independent rows.
//! - An action's settings arrive through
//!   [`describe_action`](TOOL_DESCRIBE_ACTION) — **progressive disclosure inside
//!   the one loop** — rather than through the nested inference call Saltcorn 1
//!   used, so the parameters are decided by the model that can see the
//!   conversation and a refusal reaches the model that can fix it.

use sc_agent::{AgentTrait, ToolsContext, TraitCheck, TraitContext};
use sc_api::mcp::{Areas, ToolContext, ToolSet};
use sc_api::schema_edit::{self, Grants};
use sc_error::Result;
use sc_llm::ToolSpec;
use sc_types::{Attrs, BasicType, FormField};
use serde_json::Value as Json;

pub use sc_api::mcp::{
    TOOL_DELETE_TRIGGER, TOOL_DESCRIBE, TOOL_DESCRIBE_ACTION, TOOL_DESCRIBE_TRIGGERS, TOOL_EDIT,
    TOOL_SAVE_TRIGGER,
};
pub use sc_app::mcp::{TOOL_DELETE_QUERY, TOOL_DESCRIBE_APPS, TOOL_SAVE_QUERY};

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

/// Whether the trigger half is offered at all. On by default.
///
/// The first of the two **area** checkboxes, and a different kind of setting
/// from the four grants above it: a grant says what this agent may *do*, an area
/// says which of the three things it may do it *to*. They compose — an agent with
/// the triggers area on and `allow_drop` off can write a trigger and cannot
/// delete one — and an area that is off removes its tools from the model's list
/// entirely rather than leaving them there to be refused. A tool the model can
/// see is a tool it will try, and a conversation spent discovering what an agent
/// is not for is a conversation the admin pays for.
pub const CFG_ALLOW_TRIGGERS: &str = sc_api::mcp::Area::Triggers.key();
/// Whether the application half — an application's custom SQL queries — is
/// offered at all. On by default, and read exactly as [`CFG_ALLOW_TRIGGERS`] is.
pub const CFG_ALLOW_APPLICATIONS: &str = sc_api::mcp::Area::Applications.key();

/// Build and inspect the schema and the triggers over it.
pub struct AdminCopilot;

/// Every tool this trait can offer, under fixed names — the schema's two, the
/// triggers' four and the applications' three.
///
/// *Can*, not *does*: the two area checkboxes decide whether the trigger and
/// application halves are offered at all, so a configured instance offers a
/// subset of these. This is the whole set, which is what the admin UI's "what
/// will this be called?" and §11.2's collision check want — a name that any
/// configuration could produce is a name that could collide.
pub fn tool_names() -> [&'static str; 9] {
    [
        TOOL_DESCRIBE,
        TOOL_EDIT,
        TOOL_DESCRIBE_TRIGGERS,
        TOOL_DESCRIBE_ACTION,
        TOOL_SAVE_TRIGGER,
        TOOL_DELETE_TRIGGER,
        TOOL_DESCRIBE_APPS,
        TOOL_SAVE_QUERY,
        TOOL_DELETE_QUERY,
    ]
}

#[async_trait::async_trait]
impl AgentTrait for AdminCopilot {
    fn name(&self) -> &str {
        "admin_copilot"
    }

    fn description(&self) -> &str {
        "Build the application: create, alter and drop tables and fields, \
         configure the actions that run when something happens, and write the \
         custom SQL queries an application serves as API endpoints"
    }

    fn config_spec(&self) -> Vec<FormField> {
        vec![
            FormField::new(CFG_ALLOW_CREATE, BasicType::Bool)
                .label("May create tables, fields, triggers and custom SQL queries")
                .default_value(true),
            FormField::new(CFG_ALLOW_EDIT, BasicType::Bool)
                .label("May change existing tables, fields, triggers and custom SQL queries")
                .default_value(true),
            FormField::new(CFG_ALLOW_DROP, BasicType::Bool)
                .label("May drop tables and fields, and delete triggers and custom SQL queries")
                .default_value(false),
            FormField::new(CFG_ALLOW_ACCESS, BasicType::Bool)
                .label(
                    "May change access rules (roles, ownership formula, row-level \
                     security, a trigger's minimum role, who may call a custom \
                     SQL query)",
                )
                .default_value(false),
            FormField::new(CFG_ALLOW_TRIGGERS, BasicType::Bool)
                .label("May work on triggers")
                .default_value(true),
            FormField::new(CFG_ALLOW_APPLICATIONS, BasicType::Bool)
                .label("May work on applications' custom SQL queries")
                .default_value(true),
        ]
    }

    /// There is no table to resolve, so there is little here the spec cannot
    /// already say — which is itself the deviation §11.3 records. What is worth
    /// stating is that a configuration granting nothing is *not* an error: the
    /// grants bound the **writing** tools only, and an agent with none of them is
    /// a read-only describer of the schema and the triggers, which is a thing an
    /// admin may deliberately want.
    async fn validate_config(&self, check: &TraitCheck<'_>) -> Result<()> {
        // The same six keys asked the same question a token's grants are asked
        // (§13.6): one validator, because there is one vocabulary.
        sc_api::mcp::validate_flags(check.config)
    }

    /// The schema's two tools always, and each other half's only where its area
    /// checkbox is on — which is [`ToolSet::specs`]'s rule, not one this trait
    /// applies on top of it.
    fn tools(&self, cx: &ToolsContext<'_>, config: &Attrs) -> Vec<ToolSpec> {
        tool_set(config).specs(cx.catalog)
    }

    async fn call(
        &self,
        config: &Attrs,
        tool: &str,
        args: &Json,
        ctx: &mut TraitContext<'_>,
    ) -> Result<Json> {
        let tools = tool_set(config);
        tools.call(tool, args, &tool_context(ctx)).await
    }
}

/// The nine tools under this agent's configuration.
///
/// The whole of what configuring this trait *means*: six checkboxes become a
/// [`Grants`] and an [`Areas`], and the set does the rest. A token minted for
/// the MCP server builds the same value from the same six flags (§13.6), which
/// is why there is nothing else in this function to keep in step.
fn tool_set(config: &Attrs) -> ToolSet {
    sc_app::mcp::tool_set(grants(config), areas(config))
}

/// A run's context, narrowed to what an administrative tool uses.
///
/// The run id, the delegator and the JavaScript evaluator are not carried,
/// because none of these tools touches a row: they work in the catalog, the
/// trigger set and the application store.
fn tool_context<'a>(ctx: &'a TraitContext<'a>) -> ToolContext<'a> {
    ToolContext {
        catalog: ctx.catalog,
        user: ctx.caller.user.as_ref(),
        role: ctx.caller.role,
        triggers: ctx.triggers,
        actor: ctx.agent,
    }
}

/// The four grants as configured; an absent checkbox reads as its default.
///
/// A rename of `sc_api::mcp`'s reader rather than a second one: an agent's six
/// checkboxes and a token's six flags are the same six flags, so the defaults
/// they fall back to have to be the same defaults (§13.6).
fn grants(config: &Attrs) -> Grants {
    sc_api::mcp::grants_from_attrs(config)
}

/// The two areas as configured; an absent checkbox reads as on.
fn areas(config: &Attrs) -> Areas {
    sc_api::mcp::areas_from_attrs(config)
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
    fn both_areas_are_on_unless_switched_off() {
        assert_eq!(areas(&Attrs::new()), Areas::all());
        let a = areas(&config(&[
            (CFG_ALLOW_TRIGGERS, false),
            (CFG_ALLOW_APPLICATIONS, false),
        ]));
        assert_eq!(a, Areas::none());
    }

    /// The set this agent builds is the whole surface, in the order this crate
    /// has always published: the schema's two, the triggers' four, the
    /// applications' three.
    #[test]
    fn the_configured_set_is_the_nine_tools_this_trait_names() {
        let set = tool_set(&Attrs::new());
        assert_eq!(set.all_names(), tool_names().to_vec());
        assert_eq!(*set.grants(), grants(&Attrs::new()));
    }

    /// An area that is off removes its tools rather than leaving them to be
    /// refused — the rule stated in this module's docs, asserted through the
    /// value the trait actually builds.
    #[test]
    fn a_switched_off_area_takes_its_tools_out_of_the_offer() {
        let set = tool_set(&config(&[(CFG_ALLOW_TRIGGERS, false)]));
        assert!(!set.offers(TOOL_SAVE_TRIGGER));
        assert!(set.offers(TOOL_DESCRIBE) && set.offers(TOOL_SAVE_QUERY));

        let set = tool_set(&config(&[(CFG_ALLOW_APPLICATIONS, false)]));
        assert!(!set.offers(TOOL_SAVE_QUERY));
        assert!(set.offers(TOOL_SAVE_TRIGGER));
    }
}
