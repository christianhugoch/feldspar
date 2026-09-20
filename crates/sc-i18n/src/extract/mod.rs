//! Extraction and the lint: finding the messages, and finding the ones nobody
//! wrapped (tasks 2.1–2.3, decision D6).
//!
//! Extraction is **a Rust pass over the source**, not a step in anybody's build.
//! There is no babel plugin, no webpack loader and no `i18next-parser` in a
//! `package.json`: the server already has tree-sitter (it builds a repo map with
//! it), the admin SPA and an application's `.tsx` are the same two grammars, and
//! a build step is a thing that has to be installed, configured and kept working
//! in every project a coding agent scaffolds. One pass, run by
//! `feldspar i18n extract`, by `feldspar i18n check` in CI, and by the
//! Translations screen when it wants to know what an application says.
//!
//! Three source populations, two scanners:
//!
//! | Population | What is scanned | Where |
//! |---|---|---|
//! | **A**, Rust — `t!(` / `tc!(` | `crates/**/*.rs` | [`rust`] |
//! | **A**, the admin SPA and the builder — `t(` / `tc(` / `<T text="…">` | `ui/*/src` | [`js`] |
//! | **B**, an application's own | its `src` tree | [`js`] |
//!
//! The Rust side is a lexer and not a parser, for the reason [`rust`] gives; the
//! TypeScript side is tree-sitter, because a `.tsx` file is not something to
//! find `t(` in with a regular expression.
//!
//! # The rule that makes this worth having
//!
//! **A call whose first argument is not a string literal is an error**, named by
//! file and line ([`Problem`]). Silently skipping it is how an application ends
//! up half-translated with nobody knowing: the key never reaches the catalogue,
//! the screen shows English, the coverage figure says 100%, and the only way to
//! find out is for somebody who speaks the language to read every page.
//!
//! Behind the **`extract` feature**, with tree-sitter and its grammars — the
//! arrangement `sc-repomap`'s `grammars` has, for `sc-repomap`'s reason: this is
//! a crate every layer above it depends on, and a server serving a translated
//! page has no use for a parser.

pub mod js;
pub mod rust;

use std::collections::{BTreeMap, BTreeSet};

pub use js::{Language, extract_js, lint_js};
pub use rust::extract_rust;

/// One message found at one call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    /// The catalogue key: the English source text, with a `context\u{4}` prefix
    /// when the call site gave one (see [`context_key`](crate::context_key)).
    pub key: String,
    /// The file, as the caller named it — a path relative to the tree that was
    /// scanned, so the output is the same on two machines.
    pub file: String,
    /// 1-based.
    pub line: u32,
}

/// A call site that should have yielded a message and did not.
///
/// Not a warning. `feldspar i18n check` fails on one, because the alternative is
/// a screen that is silently untranslatable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub file: String,
    /// 1-based.
    pub line: u32,
    /// What is wrong, as a sentence — printed after `file:line: `.
    pub message: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.file, self.line, self.message)
    }
}

/// What one pass over one tree found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extraction {
    /// Every call site, in the order they were read.
    pub messages: Vec<Extracted>,
    /// Every call site that could not be read (the rule above).
    pub problems: Vec<Problem>,
}

impl Extraction {
    /// Every distinct key, sorted — the set a catalogue is measured against.
    pub fn keys(&self) -> Vec<String> {
        self.messages
            .iter()
            .map(|m| m.key.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Where each key is used, sorted by key: what the Translations screen shows
    /// beside a row, and what turns a rejected translation into something a
    /// person can go and look at.
    pub fn sites(&self) -> BTreeMap<&str, Vec<&Extracted>> {
        let mut out: BTreeMap<&str, Vec<&Extracted>> = BTreeMap::new();
        for message in &self.messages {
            out.entry(message.key.as_str()).or_default().push(message);
        }
        out
    }

    /// Fold another pass's findings in — one file, or one whole subtree, into
    /// the run's total.
    pub fn merge(&mut self, other: Extraction) {
        self.messages.extend(other.messages);
        self.problems.extend(other.problems);
    }
}

/// What the lint found: a literal in front of a person that no `t` wraps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub file: String,
    /// 1-based.
    pub line: u32,
    /// The literal itself, trimmed and clipped — the thing to search for.
    pub text: String,
    /// Where it was.
    pub what: Unwrapped,
}

/// The two places the lint looks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unwrapped {
    /// Text between tags: `<button>Save</button>`.
    JsxText,
    /// One of the four attributes a person reads, by name.
    Attribute(String),
}

/// The attributes whose value a person reads.
///
/// Four and not "every attribute whose value is a string": `className`, `id`,
/// `type`, `href` and `data-*` are strings a person never sees, and a lint that
/// cries about them is a lint that gets turned off.
pub const LINTED_ATTRIBUTES: &[&str] = &["label", "title", "placeholder", "aria-label"];

/// The longest literal a finding quotes.
const MAX_QUOTED: usize = 60;

impl Finding {
    /// The line a lint run prints.
    pub fn message(&self) -> String {
        match &self.what {
            Unwrapped::JsxText => format!(
                "{}:{}: untranslated text `{}` — wrap it in <T> or t()",
                self.file, self.line, self.text
            ),
            Unwrapped::Attribute(name) => format!(
                "{}:{}: untranslated {name} `{}` — write {name}={{t(\"{}\")}}",
                self.file, self.line, self.text, self.text
            ),
        }
    }
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

/// Whether a literal is text a person reads, and therefore text somebody should
/// have wrapped.
///
/// A heuristic, and it has to be: the lint's job is to be read by somebody
/// sweeping a codebase, so a false positive costs a glance and a false negative
/// costs a screen that is never translated. It errs towards reporting, and it
/// excludes only the shapes that are unambiguously not prose — nothing with a
/// letter run in it is exempt on the grounds of being short.
pub fn looks_like_prose(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return false;
    }
    // A run of two or more letters: `{`, `·`, `—`, `1`, `%s` and `x` are not
    // sentences, and a punctuation-only text node is most of what JSX contains.
    let mut run = 0usize;
    let mut word = false;
    for c in text.chars() {
        match c.is_alphabetic() {
            true => {
                run += 1;
                word |= run >= 2;
            }
            false => run = 0,
        }
    }
    if !word {
        return false;
    }
    // A URL or a path is not prose even though it reads like it.
    if text.starts_with("http://") || text.starts_with("https://") || text.starts_with('/') {
        return false;
    }
    // One token carrying a separator is an identifier, a class list, a MIME
    // type or a filename: `form-control`, `snake_case`, `text/plain`,
    // `index.tsx`. Two tokens with a space between them are a sentence.
    if !text.contains(char::is_whitespace)
        && (text.contains('_')
            || text.contains('-')
            || text.contains('.')
            || text.contains('/')
            || text.contains(':'))
    {
        return false;
    }
    true
}

/// A literal as a finding quotes it: one line, clipped.
pub(crate) fn quote(text: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.chars().count() <= MAX_QUOTED {
        true => flat,
        false => {
            let cut: String = flat.chars().take(MAX_QUOTED).collect();
            format!("{}…", cut.trim_end())
        }
    }
}
