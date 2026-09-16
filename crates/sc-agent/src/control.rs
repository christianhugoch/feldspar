//! Loop control: fingerprints, the doom-loop detectors, the malformed-call cap,
//! trait signals and the escalation ladder (TODO §10, R§10).
//!
//! A cheap model that is stuck does not say so. It reads the same file again,
//! re-runs the same failing check, or answers every result with the same
//! sentence, and it goes on until a budget stops it. Everything here exists to
//! notice that early and to respond in proportion.
//!
//! ## What counts as trouble
//!
//! After every round of tool calls, [`LoopControl::observe_round`] is shown the
//! round: each call's **fingerprint**, whether it was **malformed**, the
//! **signals** its trait raised, and the text the model said before calling.
//!
//! - **Identical consecutive calls**: the same fingerprint `identical_calls`
//!   times in a row (default 3).
//! - **A repeated fan-out**: the same *set* of two or more fingerprints in
//!   `repeated_rounds` consecutive rounds (default 2).
//! - **Repeated text**: the same normalised assistant text in
//!   `repeated_text` consecutive rounds (default 3).
//! - **Signals**: a trait raised the same [`Signal`] `signals` times (default
//!   3) since the ladder was last calm.
//!
//! A detector keeps firing while its condition holds, so a model that ignores
//! the warning climbs the ladder on its very next repeat.
//!
//! ## The ladder
//!
//! One rung per round in which anything fired:
//!
//! 1. **Warn**: a harness note is appended to the round's last tool *result*.
//!    Not to the system prompt, which would break the cached prefix.
//! 2. **Escalate**: the next single model call goes to the strong role.
//! 3. **Stop**: the run ends as [`Conclusion::Stuck`](crate::Conclusion::Stuck).
//!
//! `calm_rounds` consecutive rounds with nothing firing (default 5) bring the
//! ladder back to the bottom, so a long run is not stopped by three unrelated
//! hiccups an hour apart. A new message from the person resets everything.
//!
//! ## The malformed-call cap
//!
//! Separate from the ladder, because there is nothing to warn about: a call to
//! an unknown tool, with arguments that did not parse, or with arguments that
//! fail the tool's schema already came back as an error naming what was wrong.
//! `malformed_calls` of them in a row (default 3) end the run as `Stuck`.
//!
//! All of this state is part of [`AgentLoop`](crate::AgentLoop), so it survives
//! a save and a resume.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::agent::Agent;

/// The attribute: identical consecutive calls before the ladder climbs.
pub const ATTR_MAX_IDENTICAL_CALLS: &str = "max_identical_calls";
/// The attribute: consecutive rounds with the same set of calls before the
/// ladder climbs.
pub const ATTR_MAX_REPEATED_ROUNDS: &str = "max_repeated_rounds";
/// The attribute: consecutive rounds with the same assistant text before the
/// ladder climbs.
pub const ATTR_MAX_REPEATED_TEXT: &str = "max_repeated_text";
/// The attribute: consecutive malformed calls before the run ends `Stuck`.
pub const ATTR_MAX_MALFORMED_CALLS: &str = "max_malformed_calls";
/// The attribute: how many times one signal may be raised before the ladder
/// climbs.
pub const ATTR_MAX_SIGNALS: &str = "max_signals";
/// The attribute: consecutive untroubled rounds that bring the ladder back to
/// the bottom.
pub const ATTR_CALM_ROUNDS: &str = "calm_rounds";

/// Something a trait noticed that the loop should count (TODO §10).
///
/// Raised through [`TraitContext::signal`](crate::TraitContext::signal). A
/// signal is not an error — the tool result already says what went wrong — it
/// is how a trait tells the loop that the *same kind* of failure is piling up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    /// An edit still failed after the edit engine's whole cascade.
    EditFailed,
    /// `check` failed with errors the baseline did not have.
    CheckFailed,
}

impl Signal {
    /// The stored spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Signal::EditFailed => "edit_failed",
            Signal::CheckFailed => "check_failed",
        }
    }

    /// How it reads in a note to the model.
    fn describe(&self, count: u32) -> String {
        match self {
            Signal::EditFailed => format!("{count} edits have failed to apply"),
            Signal::CheckFailed => format!("the checks have failed {count} times"),
        }
    }
}

impl std::fmt::Display for Signal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `value` as canonical JSON text: object keys sorted, no whitespace.
///
/// Written out rather than left to `serde_json`, whose key order depends on a
/// feature some other crate in the build may turn on.
pub fn canonical_json(value: &Json) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Json, out: &mut String) {
    match value {
        Json::Object(fields) => {
            let mut names: Vec<&String> = fields.keys().collect();
            names.sort();
            out.push('{');
            for (i, name) in names.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Json::String(name.clone()).to_string());
                out.push(':');
                write_canonical(&fields[name.as_str()], out);
            }
            out.push('}');
        }
        Json::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        // A string's own whitespace is content and is kept.
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// A call's fingerprint: the tool and the canonical form of `key`, which is the
/// arguments unless the trait's
/// [`fingerprint`](crate::AgentTrait::fingerprint) hook said otherwise.
pub fn fingerprint(tool: &str, key: &Json) -> String {
    format!("{tool} {}", canonical_json(key))
}

/// The thresholds, read from the agent's attributes when a run is created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ControlLimits {
    /// Identical consecutive calls before the ladder climbs.
    pub identical_calls: u32,
    /// Consecutive rounds with the same set of calls before the ladder climbs.
    pub repeated_rounds: u32,
    /// Consecutive rounds with the same text before the ladder climbs.
    pub repeated_text: u32,
    /// Consecutive malformed calls before the run ends.
    pub malformed_calls: u32,
    /// Raises of one signal before the ladder climbs.
    pub signals: u32,
    /// Untroubled rounds that bring the ladder back to the bottom.
    pub calm_rounds: u32,
}

impl Default for ControlLimits {
    fn default() -> Self {
        ControlLimits {
            identical_calls: 3,
            repeated_rounds: 2,
            repeated_text: 3,
            malformed_calls: 3,
            signals: 3,
            calm_rounds: 5,
        }
    }
}

impl ControlLimits {
    /// The limits `agent` sets, each falling back to its default. Zero reads as
    /// the default, as `max_steps` does: a detector that fires before anything
    /// has happened would stop every run.
    pub fn of(agent: &Agent) -> ControlLimits {
        let d = ControlLimits::default();
        let read = |key: &str, default: u32| {
            agent
                .attributes
                .get(key)
                .and_then(Json::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| *n > 0)
                .unwrap_or(default)
        };
        ControlLimits {
            identical_calls: read(ATTR_MAX_IDENTICAL_CALLS, d.identical_calls),
            repeated_rounds: read(ATTR_MAX_REPEATED_ROUNDS, d.repeated_rounds),
            repeated_text: read(ATTR_MAX_REPEATED_TEXT, d.repeated_text),
            malformed_calls: read(ATTR_MAX_MALFORMED_CALLS, d.malformed_calls),
            signals: read(ATTR_MAX_SIGNALS, d.signals),
            calm_rounds: read(ATTR_CALM_ROUNDS, d.calm_rounds),
        }
    }
}

/// Where the run is on the ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rung {
    /// Nothing has fired recently.
    #[default]
    Calm,
    /// The model has been warned.
    Warned,
    /// One step has been, or is about to be, handed to the strong role.
    Escalated,
}

/// One call of a round, as the loop control reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct RoundCall {
    /// The tool's name, for the note.
    pub tool: String,
    /// The call's fingerprint.
    pub fingerprint: String,
    /// Whether it named an unknown tool or had arguments that did not parse
    /// or did not match the schema.
    pub malformed: bool,
    /// What its trait raised.
    pub signals: Vec<Signal>,
}

/// What the loop does after a round.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Carry on, appending `note` to the round's last tool result if there is
    /// one.
    Continue {
        /// The harness note for the model.
        note: Option<String>,
    },
    /// End the run as `Stuck`.
    Stuck {
        /// Why, for the admin.
        reason: String,
    },
}

/// The detectors' counters and the ladder: part of the loop's state.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LoopControl {
    limits: ControlLimits,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_call: Option<String>,
    identical: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    last_round: Vec<String>,
    repeated_rounds: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_text: Option<String>,
    repeated_text: u32,
    malformed: u32,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    signals: BTreeMap<Signal, u32>,
    rung: Rung,
    calm: u32,
    escalate_next: bool,
}

impl LoopControl {
    /// Fresh counters under `limits`.
    pub fn new(limits: ControlLimits) -> LoopControl {
        LoopControl {
            limits,
            ..LoopControl::default()
        }
    }

    /// The thresholds.
    pub fn limits(&self) -> ControlLimits {
        self.limits
    }

    /// Where the run is on the ladder.
    pub fn rung(&self) -> Rung {
        self.rung
    }

    /// Whether the next model call goes to the strong role.
    pub fn escalating(&self) -> bool {
        self.escalate_next
    }

    /// The escalated step was taken.
    pub fn escalation_taken(&mut self) {
        self.escalate_next = false;
    }

    /// The person said something: whatever was going wrong, they have now
    /// weighed in, so every counter and the ladder start again.
    pub fn reset(&mut self) {
        *self = LoopControl::new(self.limits);
    }

    /// Count one round and decide what happens next.
    ///
    /// `text` is what the model said before calling; `calls` are the calls in
    /// the order they ran.
    pub fn observe_round(&mut self, text: &str, calls: &[RoundCall]) -> Verdict {
        // The cap first: a model that cannot form a call is past warning.
        for call in calls {
            if call.malformed {
                self.malformed += 1;
                if self.malformed >= self.limits.malformed_calls {
                    return Verdict::Stuck {
                        reason: format!(
                            "{} malformed tool calls in a row, the last to `{}`",
                            self.malformed, call.tool
                        ),
                    };
                }
            } else {
                self.malformed = 0;
            }
        }

        let mut troubles: Vec<String> = Vec::new();

        let mut repeated_tool: Option<&str> = None;
        for call in calls {
            if self.last_call.as_deref() == Some(call.fingerprint.as_str()) {
                self.identical += 1;
            } else {
                self.identical = 1;
                self.last_call = Some(call.fingerprint.clone());
            }
            if self.identical >= self.limits.identical_calls {
                repeated_tool = Some(&call.tool);
            }
        }
        if let Some(tool) = repeated_tool {
            troubles.push(format!(
                "you have called `{tool}` with the same arguments {} times in a row",
                self.identical
            ));
        }

        let mut set: Vec<String> = calls.iter().map(|c| c.fingerprint.clone()).collect();
        set.sort();
        set.dedup();
        if set.len() >= 2 {
            if set == self.last_round {
                self.repeated_rounds += 1;
            } else {
                self.repeated_rounds = 1;
            }
            if self.repeated_rounds >= self.limits.repeated_rounds {
                troubles.push(format!(
                    "you have made the same {} calls {} rounds in a row",
                    set.len(),
                    self.repeated_rounds
                ));
            }
        } else {
            // One call is the identical-call detector's business.
            self.repeated_rounds = 0;
        }
        self.last_round = set;

        let normalised = normalise_text(text);
        if normalised.is_empty() {
            self.last_text = None;
            self.repeated_text = 0;
        } else {
            if self.last_text.as_deref() == Some(normalised.as_str()) {
                self.repeated_text += 1;
            } else {
                self.repeated_text = 1;
                self.last_text = Some(normalised);
            }
            if self.repeated_text >= self.limits.repeated_text {
                troubles.push(format!(
                    "you have said the same thing {} times in a row",
                    self.repeated_text
                ));
            }
        }

        let mut raised: Vec<Signal> = calls.iter().flat_map(|c| c.signals.clone()).collect();
        raised.sort();
        raised.dedup();
        for signal in &raised {
            let count = calls
                .iter()
                .flat_map(|c| &c.signals)
                .filter(|s| *s == signal)
                .count();
            let total = self.signals.entry(*signal).or_default();
            *total += u32::try_from(count).unwrap_or(u32::MAX);
            if *total >= self.limits.signals {
                troubles.push(signal.describe(*total));
            }
        }

        if troubles.is_empty() {
            self.calm += 1;
            if self.calm >= self.limits.calm_rounds {
                self.rung = Rung::Calm;
                self.signals.clear();
                self.calm = 0;
            }
            return Verdict::Continue { note: None };
        }
        self.calm = 0;
        let what = troubles.join("; ");
        match self.rung {
            Rung::Calm => {
                self.rung = Rung::Warned;
                Verdict::Continue {
                    note: Some(format!(
                        "[harness] {}. Doing the same thing again will not give a \
                         different result: re-read what the tools returned and try a \
                         different approach.",
                        capitalise(&what)
                    )),
                }
            }
            Rung::Warned => {
                self.rung = Rung::Escalated;
                self.escalate_next = true;
                Verdict::Continue {
                    note: Some(format!(
                        "[harness] {}, after a warning. The next step is taken by a \
                         stronger model: work out why this is not working before \
                         calling anything else.",
                        capitalise(&what)
                    )),
                }
            }
            Rung::Escalated => Verdict::Stuck {
                reason: format!("still stuck after a warning and an escalation: {what}"),
            },
        }
    }
}

/// Assistant text as the repeated-text detector compares it: trimmed,
/// lower-cased, with whitespace runs collapsed.
fn normalise_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(tool: &str, args: Json) -> RoundCall {
        RoundCall {
            tool: tool.to_owned(),
            fingerprint: fingerprint(tool, &args),
            malformed: false,
            signals: Vec::new(),
        }
    }

    fn note(verdict: &Verdict) -> Option<&str> {
        match verdict {
            Verdict::Continue { note } => note.as_deref(),
            Verdict::Stuck { .. } => None,
        }
    }

    #[test]
    fn canonical_json_sorts_keys_at_every_depth() {
        let a = json!({"b": 1, "a": {"y": [1, {"d": 2, "c": 3}], "x": " s "}});
        assert_eq!(
            canonical_json(&a),
            r#"{"a":{"x":" s ","y":[1,{"c":3,"d":2}]},"b":1}"#
        );
        let b: Json =
            serde_json::from_str(r#"{ "a" : {"x":" s ", "y":[1,{"c":3,"d":2}]}, "b":1 }"#).unwrap();
        assert_eq!(fingerprint("t", &a), fingerprint("t", &b));
        assert_ne!(fingerprint("t", &a), fingerprint("u", &a));
    }

    #[test]
    fn three_identical_calls_warn_then_escalate_then_stop() {
        let mut control = LoopControl::default();
        let read = || vec![call("read_file", json!({"path": "a"}))];
        assert_eq!(note(&control.observe_round("", &read())), None);
        assert_eq!(note(&control.observe_round("", &read())), None);
        let warned = control.observe_round("", &read());
        assert!(note(&warned).unwrap().contains("3 times"), "{warned:?}");
        assert_eq!(control.rung(), Rung::Warned);
        assert!(!control.escalating());

        let escalated = control.observe_round("", &read());
        assert!(note(&escalated).unwrap().contains("stronger model"));
        assert!(control.escalating());
        control.escalation_taken();

        let Verdict::Stuck { reason } = control.observe_round("", &read()) else {
            panic!("the third rung stops the run");
        };
        assert!(reason.contains("`read_file`"), "{reason}");
    }

    #[test]
    fn a_different_call_breaks_the_run_and_calm_rounds_reset_the_ladder() {
        let mut control = LoopControl::new(ControlLimits {
            calm_rounds: 2,
            ..ControlLimits::default()
        });
        for _ in 0..3 {
            control.observe_round("", &[call("a", json!({}))]);
        }
        assert_eq!(control.rung(), Rung::Warned);
        assert_eq!(
            note(&control.observe_round("", &[call("b", json!({}))])),
            None
        );
        assert_eq!(control.rung(), Rung::Warned);
        control.observe_round("", &[call("c", json!({}))]);
        assert_eq!(control.rung(), Rung::Calm);
    }

    #[test]
    fn a_repeated_fan_out_and_repeated_text_are_detected() {
        let mut control = LoopControl::default();
        let fan = || vec![call("a", json!({"n": 1})), call("b", json!({}))];
        assert_eq!(note(&control.observe_round("", &fan())), None);
        // The same set, in another order.
        let mut reversed = fan();
        reversed.reverse();
        assert!(
            note(&control.observe_round("", &reversed))
                .unwrap()
                .contains("same 2 calls")
        );

        let mut control = LoopControl::default();
        for (i, text) in ["Let me check.", "let me   check.", " LET ME CHECK. "]
            .iter()
            .enumerate()
        {
            let verdict = control.observe_round(text, &[call("a", json!({"i": i}))]);
            if i < 2 {
                assert_eq!(note(&verdict), None);
            } else {
                assert!(note(&verdict).unwrap().contains("same thing 3 times"));
            }
        }
    }

    #[test]
    fn malformed_calls_are_capped_and_a_good_call_resets_the_count() {
        let mut control = LoopControl::default();
        let bad = |n: u32| RoundCall {
            malformed: true,
            ..call("nope", json!({"n": n}))
        };
        control.observe_round("", &[bad(1), bad(2)]);
        control.observe_round("", &[call("fine", json!({}))]);
        control.observe_round("", &[bad(3), bad(4)]);
        let Verdict::Stuck { reason } = control.observe_round("", &[bad(5)]) else {
            panic!("three in a row stops the run");
        };
        assert!(reason.starts_with("3 malformed tool calls"), "{reason}");
    }

    #[test]
    fn signals_accumulate_until_they_climb_and_a_reset_clears_everything() {
        let mut control = LoopControl::default();
        let failed = |n: u32| RoundCall {
            signals: vec![Signal::EditFailed],
            ..call("edit", json!({"n": n}))
        };
        assert_eq!(note(&control.observe_round("", &[failed(1)])), None);
        // Something else in between does not reset the count.
        control.observe_round("", &[call("read", json!({}))]);
        assert_eq!(note(&control.observe_round("", &[failed(2)])), None);
        let warned = control.observe_round("", &[failed(3)]);
        assert!(
            note(&warned).unwrap().contains("3 edits have failed"),
            "{warned:?}"
        );

        control.reset();
        assert_eq!(control.rung(), Rung::Calm);
        assert_eq!(control, LoopControl::default());
    }

    #[test]
    fn limits_are_read_from_the_agent_and_zero_is_the_default() {
        let agent = Agent::new("a", "p")
            .attribute(ATTR_MAX_IDENTICAL_CALLS, 5)
            .attribute(ATTR_MAX_MALFORMED_CALLS, 0);
        let limits = ControlLimits::of(&agent);
        assert_eq!(limits.identical_calls, 5);
        assert_eq!(limits.malformed_calls, 3);
        let control = LoopControl::new(limits);
        let back: LoopControl =
            serde_json::from_value(serde_json::to_value(&control).unwrap()).unwrap();
        assert_eq!(back, control);
    }
}
