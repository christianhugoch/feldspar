//! The [`AgentTrait`] extension point and the contexts it works in (§11.2).
//!
//! A **trait** is an elementary agent capability. Most contribute one or more
//! **tools** to the loop; some only change the turn (an extra paragraph of system
//! prompt, data preloaded into the conversation). This is the same "settings as
//! data" move an `Action`, a file-store backend and a
//! framework already make: a trait declares its configuration as
//! [`FormField`]s, so the admin UI renders a form for a trait it has never heard
//! of, and that same declaration is what validates a saved agent.
//!
//! This crate registers **no traits at all**. The built-in set lives in
//! `sc-core-traits` at layer 9, above the row layer, for the reason §10.1 gives
//! for `sc-core-actions`: a trait that writes a row must go through `sc-api`'s
//! rows module so the write is coerced, validated and *observed* by triggers,
//! and that module is above this one.
//!
//! ## Who a tool runs as
//!
//! Every tool executes as the [`RunCaller`] the run was created with — the user
//! who is chatting, not the server (decision 5). So `query_table` is an ordinary
//! read with that caller, §7.3's ownership and row-level security apply
//! unchanged, and an agent cannot become a way around them. A run started by a
//! trigger carries the trigger's authority instead, and that difference is set at
//! exactly one place: where the run is created.

use std::sync::Arc;

use sc_auth::User;
use sc_catalog::Catalog;
use sc_error::{Error, Result};
use sc_expr::JsEvaluator;
use sc_llm::ToolSpec;
use sc_types::{Attrs, FormField};
use serde_json::Value as Json;

use crate::run::RunId;

/// One elementary agent capability: configurable, contributing tools.
///
/// Object-safe and dynamically dispatched, for the reason an `Action` is: which
/// traits an agent has is decided at runtime from stored configuration, and the
/// set is meant to grow from outside this crate.
#[async_trait::async_trait]
pub trait AgentTrait: Send + Sync {
    /// The name the trait is registered and stored under (`query_table`, …).
    /// Stable: it is what a saved agent references.
    fn name(&self) -> &str;

    /// One line for the admin UI's trait picker.
    fn description(&self) -> &str;

    /// The configuration this trait takes, as form fields.
    fn config_spec(&self) -> Vec<FormField>;

    /// Check a configuration beyond what [`config_spec`](AgentTrait::config_spec)
    /// can express — the part only this trait knows: that a table it names exists
    /// and can be addressed by primary key, that a trigger it exposes is real,
    /// that a named field is.
    ///
    /// Runs where the generic check runs: on save, in front of the admin, *and*
    /// again on load, so an agent whose world changed underneath it leaves the
    /// live set with a reason rather than failing mid-conversation.
    async fn validate_config(&self, check: &TraitCheck<'_>) -> Result<()> {
        let _ = check;
        Ok(())
    }

    /// The tools this configuration contributes. May be empty — a trait that only
    /// changes the turn contributes none.
    ///
    /// **Tool names are derived from the configuration**, not fixed by the trait:
    /// one `query_table` configured against `books` and another against `orders`
    /// are two tools the model must be able to tell apart, so they are
    /// `query_books` and `query_orders`. A collision between two enabled traits is
    /// refused on save by [`validate_agent`](crate::validate_agent), where it is
    /// fixable, rather than discovered when the model picks the wrong one.
    ///
    /// The `catalog` is here because a tool's **description and JSON schema are
    /// generated from the thing it is configured against** (§11.3): `query_books`
    /// tells the model which fields it may filter on rather than leaving it to
    /// guess, and a guess that misses costs a turn. A declaration built from the
    /// configuration alone could not say any of that.
    ///
    /// Infallible, because it is called wherever the tool set is needed —
    /// including while reporting *why* an agent is invalid. A trait whose target
    /// has since been dropped should still return its tool under the name the
    /// configuration gives it, described as best it can: dropping the tool
    /// silently would turn "this agent names a table that is gone" into "this
    /// agent has no tools", and the second is not a repairable message.
    fn tools(&self, catalog: &Catalog, config: &Attrs) -> Vec<ToolSpec>;

    /// Run one of this trait's tools.
    ///
    /// `config` is *this enabled instance's* configuration, `tool` is one of the
    /// names [`tools`](AgentTrait::tools) produced for it, and `args` is the
    /// arguments object the model sent — already parsed, never a fragment.
    ///
    /// An `Err` here is not a failure of the run: the loop turns it into the tool
    /// *result* the model reads (§11.2), because "no such table" is something a
    /// model can act on and an exception is not. So an implementation should
    /// return errors that read as instructions to a reader who cannot see the
    /// stack.
    async fn call(
        &self,
        config: &Attrs,
        tool: &str,
        args: &Json,
        ctx: &mut TraitContext<'_>,
    ) -> Result<Json>;

    /// Change the turn without adding a tool — an extra paragraph of system
    /// prompt, data preloaded into the conversation.
    ///
    /// Called before **every** model call, not once per run, so a trait whose
    /// contribution depends on what has happened so far can say something
    /// different on the second step than on the first.
    async fn on_turn(&self, config: &Attrs, turn: &mut Turn<'_>) -> Result<()> {
        let _ = (config, turn);
        Ok(())
    }
}

/// What a trait's own configuration check ([`AgentTrait::validate_config`]) gets.
pub struct TraitCheck<'a> {
    /// The live catalog: what tables, fields and triggers exist.
    pub catalog: &'a Catalog,
    /// The configuration being validated, keyed by
    /// [`config_spec`](AgentTrait::config_spec) field names.
    pub config: &'a Attrs,
    /// The name of the agent this instance is enabled on, for messages that have
    /// to be actionable in a list of agents.
    pub agent: &'a str,
}

/// Who a run's tools execute as (decision 5).
///
/// Two shapes, and only two: a **user**, which is a chat turn — the person is
/// there, and every tool is their read or their write — and the **system**, which
/// is a trigger-started run carrying the trigger's own authority. There is no
/// third, and in particular no default: [`Run::new`](crate::Run::new) takes one,
/// so a run that never decided cannot exist.
///
/// The two fields are what a trait hands to the row layer:
/// `sc_api::caller_context_at(caller.role, caller.user.as_ref())` is the whole
/// conversion, and it is spelled at the call site rather than here because
/// `sc-api` is above this crate.
#[derive(Debug, Clone, PartialEq)]
pub struct RunCaller {
    /// The user every tool runs as, or `None` for a system run.
    pub user: Option<User>,
    /// The role every tool's access is checked at — the user's own, or admin for
    /// a system run.
    pub role: u8,
}

impl RunCaller {
    /// A run on behalf of `user`, at that user's role. This is a chat turn.
    pub fn user(user: User) -> RunCaller {
        RunCaller {
            role: user.role,
            user: Some(user),
        }
    }

    /// A run with the authority of the trigger that started it: admin, no user.
    ///
    /// Named for what it is rather than called a default, because the whole point
    /// of decision 5 is that the choice is visible at the one place a run is
    /// created.
    pub fn system() -> RunCaller {
        RunCaller {
            user: None,
            role: 1,
        }
    }

    /// Whether this caller meets a role floor — how an agent's `min_role` and a
    /// trigger's are both checked.
    ///
    /// Roles descend: 1 is admin and 100 is public, so *meeting* a floor is
    /// having a number no larger than it.
    pub fn meets_role(&self, min_role: Option<u8>) -> bool {
        self.role <= min_role.unwrap_or(1)
    }
}

/// Everything one tool call has access to.
///
/// Borrowed rather than owned (hence the lifetime): a call is a single `await` on
/// the driver's stack.
pub struct TraitContext<'a> {
    /// The data layer. A trait that touches rows goes through the row layer
    /// (`sc-api`, above this crate) with [`caller`](TraitContext::caller), rather
    /// than reaching the driver directly.
    pub catalog: &'a Catalog,
    /// Who this tool runs as.
    pub caller: &'a RunCaller,
    /// The name of the agent whose tool this is, for error messages: a tool
    /// failure the admin cannot attribute to an agent is one they cannot fix.
    pub agent: &'a str,
    /// The run this call belongs to, so a trait can record against it.
    pub run: RunId,
    /// The JavaScript engine, where the deployment has one.
    ///
    /// A tool that reads rows needs it exactly when the table it reads has an
    /// **untranslatable** ownership formula (§7.3): the rows come back and the
    /// formula decides per row, in V8. It is carried rather than reached for
    /// because a run may be driven from a context that has no engine, and the
    /// honest answer there is [`require_evaluator`](TraitContext::require_evaluator)'s
    /// configuration error rather than a read that quietly skips the check.
    pub evaluator: Option<&'a Arc<dyn JsEvaluator>>,
}

impl TraitContext<'_> {
    /// The engine, or the configuration error that says the server has none.
    ///
    /// Fail closed and fail loudly: a trait that cannot evaluate an ownership
    /// formula must not fall back to reading the rows anyway.
    pub fn require_evaluator(&self) -> Result<&Arc<dyn JsEvaluator>> {
        self.evaluator.ok_or_else(|| {
            Error::config(format!(
                "agent `{}`: this needs the JavaScript evaluator, \
                 and none is configured on this server",
                self.agent
            ))
        })
    }
}

/// One turn about to be sent to the model, as [`AgentTrait::on_turn`] may change
/// it.
///
/// Deliberately narrow: a trait may *append* to the system prompt and it may
/// append messages. It cannot rewrite the agent's own prompt or edit the
/// conversation, because a trait that could silently change what the admin wrote
/// is one whose effect nobody could read off the agent's definition.
pub struct Turn<'a> {
    /// Who the run is for.
    pub caller: &'a RunCaller,
    /// The name of the agent taking the turn.
    pub agent: &'a str,
    /// Which model call this is, counting from 1 — so a trait can say something
    /// different on a later step than on the first.
    pub step: u32,
    /// Paragraphs appended to the system prompt, in the order traits added them.
    extra_system: Vec<String>,
}

impl<'a> Turn<'a> {
    /// A turn with nothing added yet.
    pub fn new(caller: &'a RunCaller, agent: &'a str, step: u32) -> Turn<'a> {
        Turn {
            caller,
            agent,
            step,
            extra_system: Vec::new(),
        }
    }

    /// Append a paragraph to the system prompt for this turn only.
    ///
    /// Blank text is dropped rather than stored, so a trait that computed nothing
    /// to say does not push a stray blank line into the prompt.
    pub fn append_system(&mut self, text: impl Into<String>) {
        let text = text.into();
        if !text.trim().is_empty() {
            self.extra_system.push(text);
        }
    }

    /// Everything appended, in order.
    pub fn extra_system(&self) -> &[String] {
        &self.extra_system
    }

    /// The agent's system prompt with every appended paragraph after it, blank
    /// line separated — what the request's `system` carries.
    pub fn system_prompt(&self, base: &str) -> String {
        let mut parts: Vec<&str> = Vec::with_capacity(self.extra_system.len() + 1);
        if !base.trim().is_empty() {
            parts.push(base);
        }
        parts.extend(self.extra_system.iter().map(String::as_str));
        parts.join("\n\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn a_system_run_is_admin_with_no_user() {
        let caller = RunCaller::system();
        assert_eq!(caller.role, 1);
        assert!(caller.user.is_none());
        // A trigger's authority reaches everything a role floor could ask for.
        assert!(caller.meets_role(None));
        assert!(caller.meets_role(Some(80)));
    }

    #[test]
    fn a_user_run_carries_that_users_role() {
        let user = User::new(Uuid::new_v4(), 40).unwrap();
        let caller = RunCaller::user(user);
        assert_eq!(caller.role, 40);
        assert!(caller.user.is_some());
        // 40 meets a floor of 80 (roles descend) but not an admin-only agent's.
        assert!(caller.meets_role(Some(80)));
        assert!(caller.meets_role(Some(40)));
        assert!(!caller.meets_role(Some(10)));
        assert!(!caller.meets_role(None));
    }

    #[test]
    fn a_turn_appends_to_the_prompt_without_rewriting_it() {
        let caller = RunCaller::system();
        let mut turn = Turn::new(&caller, "librarian", 1);
        assert_eq!(
            turn.system_prompt("You answer questions."),
            "You answer questions."
        );

        turn.append_system("The books table has 12 rows.");
        // Blank additions are dropped rather than stored.
        turn.append_system("   ");
        assert_eq!(turn.extra_system().len(), 1);
        assert_eq!(
            turn.system_prompt("You answer questions."),
            "You answer questions.\n\nThe books table has 12 rows."
        );
    }

    #[test]
    fn an_agent_with_no_prompt_of_its_own_still_gets_the_additions() {
        let caller = RunCaller::system();
        let mut turn = Turn::new(&caller, "a", 2);
        turn.append_system("first");
        turn.append_system("second");
        assert_eq!(turn.system_prompt(""), "first\n\nsecond");
    }
}
