//! What a model can do, as far as the loop needs to know: [`ModelCapabilities`]
//! and its resolution (design §11.1, TODO §4).
//!
//! **Two sources, in order.** The built-in rules come first: the backend, plus
//! patterns over the model's name. Then a model row's **non-blank** overrides
//! (§3a). A row records only where it differs, so improving a rule here reaches
//! every row that left the setting blank. A row that wants the old answer back
//! sets it.
//!
//! The rules are deliberately coarse. They exist so that a model added with
//! blank settings works reasonably on its first request. They do not have to be
//! right about every model a host serves, because the admin can override any of
//! them.

use sc_types::Attrs;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::def::{ANTHROPIC_BACKEND, OPENAI_CHAT_BACKEND, OPENAI_RESPONSES_BACKEND};

/// Model setting: whether the model accepts parallel tool calls (`yes`/`no`).
pub const CFG_PARALLEL_TOOL_CALLS: &str = "parallel_tool_calls";
/// Model setting: whether parallel tool calls are on when the agent does not
/// say (`on`/`off`).
pub const CFG_PARALLEL_TOOL_CALLS_DEFAULT: &str = "parallel_tool_calls_default";
/// Model setting: whether the backend offers `apply_patch` as a native tool
/// for this model (`yes`/`no`).
pub const CFG_NATIVE_APPLY_PATCH: &str = "native_apply_patch";
/// Model setting: whether opaque, vendor-signed reasoning is sent back
/// (`yes`/`no`).
pub const CFG_REASONING_REPLAY: &str = "reasoning_replay";
/// Model setting: how prompt caching works (`explicit`/`automatic`/`none`).
pub const CFG_PROMPT_CACHING: &str = "prompt_caching";
/// Model setting: the preferred edit format.
pub const CFG_EDIT_FORMAT: &str = "edit_format";
/// Model setting: the context window, in tokens.
pub const CFG_CONTEXT_WINDOW: &str = "context_window";
/// Model setting: the default working budget, in tokens.
pub const CFG_WORKING_BUDGET: &str = "working_budget";
/// Model setting: whether a tool result may carry an image (`yes`/`no`).
pub const CFG_VISION: &str = "vision";

/// The context window assumed for a model no rule recognises.
pub const UNKNOWN_CONTEXT_WINDOW: u64 = 32_000;
/// The working budget of a model no rule recognises (§9: "32k tokens for an
/// unknown model").
pub const UNKNOWN_WORKING_BUDGET: u64 = 32_000;
/// The largest working budget a rule gives, however large the window. A bigger
/// context costs more per step and is used less well, so a model with a
/// million-token window still compacts at a size a run can afford.
pub const MAX_BUILT_IN_WORKING_BUDGET: u64 = 100_000;

/// How a model's prompt cache is driven.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptCaching {
    /// The request marks breakpoints itself (Anthropic's `cache_control`).
    Explicit,
    /// The host caches common prefixes on its own (OpenAI), and may take a
    /// routing key.
    Automatic,
    /// No caching, or none the harness can rely on.
    None,
}

impl PromptCaching {
    /// The setting's stored spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            PromptCaching::Explicit => "explicit",
            PromptCaching::Automatic => "automatic",
            PromptCaching::None => "none",
        }
    }

    fn parse(text: &str) -> Option<PromptCaching> {
        match text {
            "explicit" => Some(PromptCaching::Explicit),
            "automatic" => Some(PromptCaching::Automatic),
            "none" => Some(PromptCaching::None),
            _ => None,
        }
    }
}

/// The edit format a model works best with (§6). `coding`'s `auto` resolves to
/// this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditFormat {
    /// Search and replace over exact text.
    StrReplace,
    /// OpenAI's V4A patch format.
    ApplyPatch,
    /// Whole files only.
    WholeFile,
}

impl EditFormat {
    /// The setting's stored spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            EditFormat::StrReplace => "str_replace",
            EditFormat::ApplyPatch => "apply_patch",
            EditFormat::WholeFile => "whole_file",
        }
    }

    fn parse(text: &str) -> Option<EditFormat> {
        match text {
            "str_replace" => Some(EditFormat::StrReplace),
            "apply_patch" => Some(EditFormat::ApplyPatch),
            "whole_file" => Some(EditFormat::WholeFile),
            _ => None,
        }
    }
}

/// What one model can do, resolved (§4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    /// The model accepts several tool calls in one turn.
    pub parallel_tool_calls: bool,
    /// Parallel calls are on when the agent does not say. Off for every
    /// built-in rule, because sequential calls are easier to fingerprint (R§12).
    pub parallel_tool_calls_default: bool,
    /// The backend offers `apply_patch` as a native tool for this model.
    pub native_apply_patch: bool,
    /// Opaque, vendor-signed reasoning items are sent back in the history.
    pub reasoning_replay: bool,
    /// How the prompt cache is driven.
    pub prompt_caching: PromptCaching,
    /// The edit format `auto` resolves to.
    pub edit_format: EditFormat,
    /// The context window, in tokens.
    pub context_window: u64,
    /// The default working budget, in tokens: where compaction is measured
    /// from (§9).
    pub working_budget: u64,
    /// A tool result may carry an image.
    pub vision: bool,
}

impl ModelCapabilities {
    /// The capabilities for `model` on `backend`: the built-in rules, then the
    /// model row's non-blank overrides in `config`.
    ///
    /// A value in `config` of the wrong shape is ignored here rather than
    /// reported. Saving and loading a row both validate it against
    /// [`model_config_spec`](crate::model_config_spec), so a bad value never
    /// gets this far from a stored row.
    pub fn resolve(backend: &str, model: &str, config: &Attrs) -> ModelCapabilities {
        let mut caps = ModelCapabilities::built_in(backend, model);
        caps.apply_overrides(config);
        caps
    }

    /// The built-in rules alone: the backend, plus patterns over the model's
    /// name.
    pub fn built_in(backend: &str, model: &str) -> ModelCapabilities {
        let name = model.trim().to_ascii_lowercase();
        let openai_family = is_openai_family(&name);
        let window = context_window(&name);
        let working_budget = match window {
            Some(window) => (window / 2).min(MAX_BUILT_IN_WORKING_BUDGET),
            None => UNKNOWN_WORKING_BUDGET,
        };
        let context_window = window.unwrap_or(UNKNOWN_CONTEXT_WINDOW);

        match backend {
            ANTHROPIC_BACKEND => ModelCapabilities {
                parallel_tool_calls: true,
                parallel_tool_calls_default: false,
                native_apply_patch: false,
                // Thinking signatures, whenever thinking is on. A model that
                // never thinks has nothing to replay, so this costs nothing.
                reasoning_replay: true,
                prompt_caching: PromptCaching::Explicit,
                edit_format: EditFormat::StrReplace,
                context_window,
                working_budget,
                vision: name.starts_with("claude") && !name.starts_with("claude-2"),
            },
            OPENAI_RESPONSES_BACKEND => ModelCapabilities {
                parallel_tool_calls: true,
                parallel_tool_calls_default: false,
                native_apply_patch: name.starts_with("gpt-5") || name.contains("codex"),
                reasoning_replay: is_openai_reasoning(&name),
                prompt_caching: if openai_family {
                    PromptCaching::Automatic
                } else {
                    PromptCaching::None
                },
                edit_format: if openai_family {
                    EditFormat::ApplyPatch
                } else {
                    EditFormat::StrReplace
                },
                context_window,
                working_budget,
                vision: openai_vision(&name),
            },
            OPENAI_CHAT_BACKEND => ModelCapabilities {
                parallel_tool_calls: true,
                parallel_tool_calls_default: false,
                // Chat Completions has neither (§1.14).
                native_apply_patch: false,
                reasoning_replay: false,
                prompt_caching: if openai_family || name.contains("deepseek") {
                    PromptCaching::Automatic
                } else {
                    PromptCaching::None
                },
                edit_format: if openai_family {
                    EditFormat::ApplyPatch
                } else {
                    EditFormat::StrReplace
                },
                context_window,
                working_budget,
                vision: openai_vision(&name) || open_weight_vision(&name),
            },
            // An unknown backend cannot be connected, so what it "can do" is
            // never used. The most conservative answer is still an answer.
            _ => ModelCapabilities {
                parallel_tool_calls: false,
                parallel_tool_calls_default: false,
                native_apply_patch: false,
                reasoning_replay: false,
                prompt_caching: PromptCaching::None,
                edit_format: EditFormat::StrReplace,
                context_window,
                working_budget,
                vision: false,
            },
        }
    }

    /// Apply a model row's non-blank settings over these.
    fn apply_overrides(&mut self, config: &Attrs) {
        if let Some(v) = yes_no(config, CFG_PARALLEL_TOOL_CALLS) {
            self.parallel_tool_calls = v;
        }
        if let Some(v) = on_off(config, CFG_PARALLEL_TOOL_CALLS_DEFAULT) {
            self.parallel_tool_calls_default = v;
        }
        if let Some(v) = yes_no(config, CFG_NATIVE_APPLY_PATCH) {
            self.native_apply_patch = v;
        }
        if let Some(v) = yes_no(config, CFG_REASONING_REPLAY) {
            self.reasoning_replay = v;
        }
        if let Some(v) = text(config, CFG_PROMPT_CACHING).and_then(PromptCaching::parse) {
            self.prompt_caching = v;
        }
        if let Some(v) = text(config, CFG_EDIT_FORMAT).and_then(EditFormat::parse) {
            self.edit_format = v;
        }
        if let Some(v) = tokens(config, CFG_CONTEXT_WINDOW) {
            self.context_window = v;
            // A window set by hand with no budget beside it keeps the budget
            // inside the window.
            if tokens(config, CFG_WORKING_BUDGET).is_none() {
                self.working_budget = self.working_budget.min(v);
            }
        }
        if let Some(v) = tokens(config, CFG_WORKING_BUDGET) {
            self.working_budget = v;
        }
        if let Some(v) = yes_no(config, CFG_VISION) {
            self.vision = v;
        }
        // Parallel calls a model cannot make are never on by default.
        if !self.parallel_tool_calls {
            self.parallel_tool_calls_default = false;
        }
    }
}

/// OpenAI's own model names: `gpt-…`, `o1`/`o3`/`o4…`, `codex…`.
fn is_openai_family(name: &str) -> bool {
    name.starts_with("gpt-")
        || name.starts_with("chatgpt")
        || name.contains("codex")
        || is_o_series(name)
}

/// `o1`, `o3-mini`, `o4-mini` and so on.
fn is_o_series(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next() == Some('o') && chars.next().is_some_and(|c| c.is_ascii_digit())
}

/// The OpenAI models that produce encrypted reasoning.
fn is_openai_reasoning(name: &str) -> bool {
    (name.starts_with("gpt-5") && !name.contains("chat"))
        || is_o_series(name)
        || name.contains("codex")
}

/// OpenAI models that accept images.
fn openai_vision(name: &str) -> bool {
    name.starts_with("gpt-4o")
        || name.starts_with("gpt-4.1")
        || name.starts_with("gpt-5")
        || name.starts_with("o3")
        || name.starts_with("o4")
}

/// Open-weight models whose names say they accept images.
fn open_weight_vision(name: &str) -> bool {
    [
        "-vl", "vision", "llava", "pixtral", "gemma-3", "gemma3", "llama-4", "llama4",
    ]
    .iter()
    .any(|p| name.contains(p))
}

/// The context window a name implies, where a rule knows it.
fn context_window(name: &str) -> Option<u64> {
    if name.starts_with("claude") {
        return Some(200_000);
    }
    if name.starts_with("gpt-4.1") {
        return Some(1_000_000);
    }
    if name.starts_with("gpt-5") || name.contains("codex") {
        return Some(400_000);
    }
    if name.starts_with("gpt-4o") || name.starts_with("o1") {
        return Some(128_000);
    }
    if name.starts_with("o3") || name.starts_with("o4") {
        return Some(200_000);
    }
    if name.contains("deepseek") {
        return Some(128_000);
    }
    if name.starts_with("gpt-oss") || name.contains("qwen3") || name.contains("kimi") {
        return Some(128_000);
    }
    None
}

/// A non-blank text setting.
fn text<'a>(config: &'a Attrs, key: &str) -> Option<&'a str> {
    config
        .get(key)
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn yes_no(config: &Attrs, key: &str) -> Option<bool> {
    match text(config, key)? {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

fn on_off(config: &Attrs, key: &str) -> Option<bool> {
    match text(config, key)? {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

/// A positive token count. Zero is treated as blank: no model has a window of
/// zero, and a budget of zero would compact before every call.
fn tokens(config: &Attrs, key: &str) -> Option<u64> {
    config.get(key).and_then(Json::as_u64).filter(|n| *n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn attrs(value: Json) -> Attrs {
        value.as_object().cloned().unwrap_or_default()
    }

    /// (backend, model, native apply_patch, replay, caching, edit, window, vision)
    type Row = (
        &'static str,
        &'static str,
        bool,
        bool,
        PromptCaching,
        EditFormat,
        u64,
        bool,
    );

    #[test]
    fn built_in_rules_by_backend_and_name() {
        let table: &[Row] = &[
            (
                ANTHROPIC_BACKEND,
                "claude-sonnet-5",
                false,
                true,
                PromptCaching::Explicit,
                EditFormat::StrReplace,
                200_000,
                true,
            ),
            (
                ANTHROPIC_BACKEND,
                "claude-haiku-4-5",
                false,
                true,
                PromptCaching::Explicit,
                EditFormat::StrReplace,
                200_000,
                true,
            ),
            (
                OPENAI_RESPONSES_BACKEND,
                "gpt-5.1",
                true,
                true,
                PromptCaching::Automatic,
                EditFormat::ApplyPatch,
                400_000,
                true,
            ),
            (
                OPENAI_RESPONSES_BACKEND,
                "gpt-4.1-mini",
                false,
                false,
                PromptCaching::Automatic,
                EditFormat::ApplyPatch,
                1_000_000,
                true,
            ),
            (
                OPENAI_RESPONSES_BACKEND,
                "o4-mini",
                false,
                true,
                PromptCaching::Automatic,
                EditFormat::ApplyPatch,
                200_000,
                true,
            ),
            (
                OPENAI_RESPONSES_BACKEND,
                "some-local-model",
                false,
                false,
                PromptCaching::None,
                EditFormat::StrReplace,
                UNKNOWN_CONTEXT_WINDOW,
                false,
            ),
            (
                OPENAI_CHAT_BACKEND,
                "gpt-5.1",
                false,
                false,
                PromptCaching::Automatic,
                EditFormat::ApplyPatch,
                400_000,
                true,
            ),
            (
                OPENAI_CHAT_BACKEND,
                "deepseek-chat",
                false,
                false,
                PromptCaching::Automatic,
                EditFormat::StrReplace,
                128_000,
                false,
            ),
            (
                OPENAI_CHAT_BACKEND,
                "qwen2.5-vl-7b",
                false,
                false,
                PromptCaching::None,
                EditFormat::StrReplace,
                UNKNOWN_CONTEXT_WINDOW,
                true,
            ),
            (
                OPENAI_CHAT_BACKEND,
                "llama3.2",
                false,
                false,
                PromptCaching::None,
                EditFormat::StrReplace,
                UNKNOWN_CONTEXT_WINDOW,
                false,
            ),
        ];
        for (backend, model, patch, replay, caching, edit, window, vision) in table {
            let caps = ModelCapabilities::built_in(backend, model);
            let at = format!("{backend}/{model}");
            assert_eq!(caps.native_apply_patch, *patch, "{at}: native apply_patch");
            assert_eq!(caps.reasoning_replay, *replay, "{at}: reasoning replay");
            assert_eq!(caps.prompt_caching, *caching, "{at}: caching");
            assert_eq!(caps.edit_format, *edit, "{at}: edit format");
            assert_eq!(caps.context_window, *window, "{at}: window");
            assert_eq!(caps.vision, *vision, "{at}: vision");
            assert!(caps.parallel_tool_calls, "{at}: parallel calls supported");
            assert!(
                !caps.parallel_tool_calls_default,
                "{at}: parallel calls off by default"
            );
            assert!(
                caps.working_budget <= caps.context_window,
                "{at}: budget within window"
            );
        }
        assert_eq!(
            ModelCapabilities::built_in(OPENAI_CHAT_BACKEND, "mystery").working_budget,
            UNKNOWN_WORKING_BUDGET
        );
        assert_eq!(
            ModelCapabilities::built_in(ANTHROPIC_BACKEND, "claude-opus-5").working_budget,
            MAX_BUILT_IN_WORKING_BUDGET
        );
    }

    #[test]
    fn a_rows_non_blank_settings_override_the_rules() {
        let config = attrs(json!({
            CFG_VISION: "yes",
            CFG_EDIT_FORMAT: "whole_file",
            CFG_CONTEXT_WINDOW: 64_000,
            CFG_PARALLEL_TOOL_CALLS_DEFAULT: "on",
            CFG_PROMPT_CACHING: "automatic",
            // Blank means the built-in default, not "no".
            CFG_REASONING_REPLAY: "",
        }));
        let caps = ModelCapabilities::resolve(OPENAI_CHAT_BACKEND, "llama3.2", &config);
        assert!(caps.vision);
        assert_eq!(caps.edit_format, EditFormat::WholeFile);
        assert_eq!(caps.context_window, 64_000);
        assert_eq!(caps.working_budget, UNKNOWN_WORKING_BUDGET);
        assert!(caps.parallel_tool_calls_default);
        assert_eq!(caps.prompt_caching, PromptCaching::Automatic);
        assert!(!caps.reasoning_replay);

        // A window smaller than the rule's budget pulls the budget in with it.
        let small = attrs(json!({ CFG_CONTEXT_WINDOW: 8_000 }));
        let caps = ModelCapabilities::resolve(ANTHROPIC_BACKEND, "claude-sonnet-5", &small);
        assert_eq!(caps.working_budget, 8_000);

        // Parallel calls a model cannot make are never on by default.
        let off =
            attrs(json!({ CFG_PARALLEL_TOOL_CALLS: "no", CFG_PARALLEL_TOOL_CALLS_DEFAULT: "on" }));
        let caps = ModelCapabilities::resolve(ANTHROPIC_BACKEND, "claude-sonnet-5", &off);
        assert!(!caps.parallel_tool_calls && !caps.parallel_tool_calls_default);
    }
}
