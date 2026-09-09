//! Delegation: one agent running another, in a run of its own (§11.3).
//!
//! The seam a `subagent` trait needs, and the one thing about it this crate can
//! own: a trait cannot construct a [`Runner`](crate::Runner) for the agent it
//! names — it has no registry, no provider connector and no way to know how deep
//! it already is — so the machinery that is *already running it* offers those as
//! a capability on the context, exactly as the trigger dispatcher and the
//! JavaScript engine are offered.
//!
//! ## Delegation, not handoff
//!
//! The two shapes multi-agent systems use are worth naming, because this is the
//! other one. A **handoff** transfers the conversation: the specialist inherits
//! the history and owns every message from then on, and the agent that routed is
//! finished. **Delegation** keeps the parent in charge: the sub-agent is called
//! like a tool, does one bounded task and hands back a result the parent reads
//! and answers with.
//!
//! v2 delegates. The reason is not preference: an agent is a *record with a role
//! floor and a set of traits*, and a run is a transcript with one subject on it.
//! A handoff would have to either rewrite a live run's subject — making the
//! transcript a record of two different agents' authority under one heading — or
//! swap the tool set mid-conversation, which is the one thing
//! [`Turn`](crate::Turn) refuses for the same reason. Delegation needs neither:
//! the child is its own [`Run`], with its own subject, its own budget and its own
//! row, linked to its parent by [`ATTR_PARENT_RUN`].
//!
//! ## The three things a delegation is bounded by
//!
//! - **Authority.** The child runs as the *parent run's own caller*, never as the
//!   server. Delegation cannot be a way to reach a table the person chatting
//!   could not read, which is decision 5 applied one level down. The sub-agent's
//!   own `min_role` still gates it on top of that, exactly as a trigger's does
//!   when an agent runs one (§11.3): being allowed to chat with an agent does not
//!   thereby allow everything it can reach.
//! - **Depth.** A chain of distinct agents is not a cycle, but it is unbounded
//!   cost, so [`DelegateRequest::max_depth`] stops it by a number the admin
//!   chose.
//! - **Cycles.** `a → b → a` is refused by *name*, with the path in the message,
//!   because the depth limit would otherwise report a budget where the real fault
//!   is a loop the admin can see and fix.
//!
//! ## Context is not inherited
//!
//! The child sees the **briefing and nothing else** — not the parent's
//! transcript, not the tool results the parent has already read. That is the
//! property the whole pattern is for: a sub-agent exists so that a long, noisy
//! task can happen somewhere other than in the parent's context window, and a
//! child that inherited the parent's history would spend the tokens the
//! delegation was meant to save. It also means the briefing is the *entire*
//! channel between them, which is why the trait that writes one asks the model
//! for a task, its context and the output wanted rather than for a bare sentence.

use sc_error::Result;

use crate::machine::Conclusion;
use crate::run::RunId;

/// The run attribute holding the id of the run that delegated this one.
///
/// Sparse (§9's rule): present only on a delegated run, so "was this asked for by
/// a person or by another agent?" is answerable off the row rather than inferred
/// from its description.
pub const ATTR_PARENT_RUN: &str = "parent_run";

/// The run attribute holding the **name** of the agent that delegated this run —
/// the name, not the id, for the reason [`Run::subject`](crate::Run::subject) is
/// a name: a transcript must stay readable after the agent it was of is deleted.
pub const ATTR_DELEGATED_BY: &str = "delegated_by";

/// How deep delegation may nest when nothing says otherwise.
///
/// Three: enough for a supervisor that calls a specialist that calls a helper,
/// and small enough that a mistaken chain costs a conversation rather than a
/// bill. Every level multiplies the token spend of the one above it, which is the
/// reason a limit exists at all — a cycle is already refused by name.
pub const DEFAULT_MAX_DEPTH: u32 = 3;

/// One agent asking another to do one task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegateRequest<'a> {
    /// The sub-agent's name, as `_fd_agents` stores it.
    pub agent: &'a str,
    /// The run doing the asking, recorded on the child as [`ATTR_PARENT_RUN`].
    ///
    /// On the request rather than read off the delegator because the caller —
    /// a trait, holding [`TraitContext::run`](crate::TraitContext::run) — is the
    /// one that knows which run its tool call belongs to.
    pub parent_run: RunId,
    /// Everything the sub-agent will be told, as its first and only user
    /// message. The whole channel between the two: see the module docs.
    pub briefing: &'a str,
    /// A step budget for this delegation, overriding the sub-agent's own
    /// [`max_steps`](crate::Agent::max_steps).
    ///
    /// `None` means the sub-agent's own, which is the common case — an override
    /// exists because the agent that *calls* one is entitled to bound what it
    /// spends on a task, and the sub-agent's number was chosen for the sub-agent
    /// working alone.
    pub max_steps: Option<u32>,
    /// How many levels of delegation may be in flight at once, counting this one.
    /// See [`DEFAULT_MAX_DEPTH`].
    pub max_depth: u32,
}

impl<'a> DelegateRequest<'a> {
    /// A delegation to `agent` with `briefing`, from `parent_run`, at the
    /// default depth bound.
    pub fn new(agent: &'a str, briefing: &'a str, parent_run: RunId) -> DelegateRequest<'a> {
        DelegateRequest {
            agent,
            parent_run,
            briefing,
            max_steps: None,
            max_depth: DEFAULT_MAX_DEPTH,
        }
    }

    /// Bound the sub-agent's steps for this delegation.
    pub fn max_steps(mut self, steps: Option<u32>) -> DelegateRequest<'a> {
        self.max_steps = steps;
        self
    }

    /// Bound how deep delegation may nest.
    pub fn max_depth(mut self, depth: u32) -> DelegateRequest<'a> {
        self.max_depth = depth;
        self
    }
}

/// How a delegated run ended.
///
/// The run **id** rather than its transcript, deliberately: the caller that wants
/// to know what the sub-agent actually did reads `_fd_runs`, which is where the
/// chat panel reads every other run from, and a tool result that carried the
/// whole transcript would put back into the parent's context precisely what
/// delegating it took out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delegated {
    /// The sub-agent that ran.
    pub agent: String,
    /// Its run, `getRun`-able like any other.
    pub run: RunId,
    /// How it ended — answered, out of steps, or stopped.
    pub conclusion: Conclusion,
    /// How many model calls it made, so a parent can see what the answer cost it.
    pub steps: u32,
}

impl Delegated {
    /// What the sub-agent finished by saying, for a run that answered.
    ///
    /// `None` covers both ways there is nothing to hand back: a run that never
    /// concluded in an answer, and one that concluded with an empty one — a
    /// sub-agent that called tools all turn and then said nothing. The second is
    /// the documented failure mode of this pattern, and collapsing it into "the
    /// answer was blank" is how it goes unnoticed.
    pub fn answer(&self) -> Option<&str> {
        self.conclusion.answer().filter(|a| !a.trim().is_empty())
    }
}

/// What one agent needs from its own machinery in order to run another.
///
/// Implemented by [`Runner`](crate::Runner) — the thing that already holds the
/// catalog, the trait registry, the caller and the position in the chain — and
/// reached by a trait through
/// [`TraitContext::require_delegate`](crate::TraitContext::require_delegate). A
/// trait object rather than a function so a context that *cannot* delegate says
/// so (a deployment with no provider connector assembled) instead of a trait
/// discovering it halfway.
#[async_trait::async_trait]
pub trait Delegator: Send + Sync {
    /// Run `request.agent` on its briefing, in a run of its own, and report how
    /// it ended.
    ///
    /// An `Err` is a delegation that could not *happen* — no such agent, a depth
    /// or cycle bound, a role floor, a provider that refused. A sub-agent that
    /// ran and finished badly is an `Ok` whose [`Delegated::conclusion`] says so,
    /// because those are different things to the agent reading the result: the
    /// first is a configuration to report, the second is a task to reformulate.
    async fn delegate(&self, request: DelegateRequest<'_>) -> Result<Delegated>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_defaults_to_the_sub_agents_own_budget_and_the_standard_depth() {
        let req = DelegateRequest::new("researcher", "find out X", RunId::new());
        assert_eq!(req.max_steps, None);
        assert_eq!(req.max_depth, DEFAULT_MAX_DEPTH);
        let req = req.max_steps(Some(5)).max_depth(1);
        assert_eq!(req.max_steps, Some(5));
        assert_eq!(req.max_depth, 1);
    }

    #[test]
    fn a_sub_agent_that_said_nothing_has_no_answer_to_hand_back() {
        let delegated = |conclusion| Delegated {
            agent: "researcher".to_owned(),
            run: RunId::new(),
            conclusion,
            steps: 3,
        };
        assert_eq!(
            delegated(Conclusion::Answered {
                answer: "  it is 12  ".to_owned()
            })
            .answer(),
            Some("  it is 12  ")
        );
        // The failure mode this pattern is known for: tools all turn, then a
        // final message with nothing in it.
        assert_eq!(
            delegated(Conclusion::Answered {
                answer: "   ".to_owned()
            })
            .answer(),
            None
        );
        assert_eq!(delegated(Conclusion::MaxSteps).answer(), None);
        assert_eq!(delegated(Conclusion::Aborted).answer(), None);
    }
}
