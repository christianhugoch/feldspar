//! The TypeScript side: `t(…)`, `tc(…)` and `<T text="…">`, with tree-sitter
//! (tasks 2.1 and 2.2).
//!
//! Two vendored queries and one parse per file. The queries find call sites,
//! JSX elements, JSX attributes and JSX text; what each one *means* is decided
//! here, because the interesting cases are the ones a query cannot express — a
//! call named `t` whose argument is a variable is an error and not a
//! non-match, and a `label` attribute is only worth reporting when its value
//! reads like a sentence.
//!
//! `.ts` is parsed with the TypeScript grammar and `.tsx` with the TSX one:
//! they are two grammars and not one, because `<T>` is a type assertion in the
//! first and an element in the second. `.js` and `.jsx` both get the JavaScript
//! grammar, which has JSX in it.

use std::sync::OnceLock;

use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

use super::{Extracted, Extraction, Finding, LINTED_ATTRIBUTES, Problem, Unwrapped, quote};
use crate::catalog::context_key;

/// Files larger than this are not parsed: a bundle, a lockfile or a generated
/// client says nothing a catalogue wants, and parsing one is seconds.
pub const MAX_PARSED_BYTES: usize = 2 * 1024 * 1024;

/// The element the `<T text="…">` form uses.
pub const T_ELEMENT: &str = "T";

/// A grammar this pass can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    JavaScript,
    TypeScript,
    Tsx,
}

impl Language {
    /// The grammar for a path, by its extension. `None` for a file this pass
    /// does not read.
    pub fn for_path(path: &str) -> Option<Language> {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        match name.rsplit_once('.')?.1 {
            "js" | "jsx" | "mjs" | "cjs" => Some(Language::JavaScript),
            "ts" | "mts" | "cts" => Some(Language::TypeScript),
            "tsx" => Some(Language::Tsx),
            _ => None,
        }
    }

    /// Whether this grammar has JSX in it. The TypeScript grammar has not —
    /// `<T text="…">` in a `.ts` file is a syntax error, not a message.
    pub fn has_jsx(self) -> bool {
        !matches!(self, Language::TypeScript)
    }

    fn grammar(self) -> tree_sitter::Language {
        match self {
            Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        }
    }
}

const CALLS_QUERY: &str = include_str!("../../queries/i18n-calls.scm");
const JSX_QUERY: &str = include_str!("../../queries/i18n-jsx.scm");

/// The two compiled queries for one grammar, built once per process.
struct Queries {
    calls: Query,
    jsx: Option<Query>,
}

fn queries(language: Language) -> Option<&'static Queries> {
    static JAVASCRIPT: OnceLock<Option<Queries>> = OnceLock::new();
    static TYPESCRIPT: OnceLock<Option<Queries>> = OnceLock::new();
    static TSX: OnceLock<Option<Queries>> = OnceLock::new();
    let cell = match language {
        Language::JavaScript => &JAVASCRIPT,
        Language::TypeScript => &TYPESCRIPT,
        Language::Tsx => &TSX,
    };
    cell.get_or_init(|| {
        let grammar = language.grammar();
        let calls = Query::new(&grammar, CALLS_QUERY).ok()?;
        let jsx = match language.has_jsx() {
            true => Some(Query::new(&grammar, JSX_QUERY).ok()?),
            false => None,
        };
        Some(Queries { calls, jsx })
    })
    .as_ref()
}

/// Whether both queries compile against `language`'s grammar. For tests: a
/// query that does not compile makes every file in that language silently
/// empty, which is the one failure this pass must not have.
pub fn queries_compile(language: Language) -> Result<(), String> {
    let grammar = language.grammar();
    Query::new(&grammar, CALLS_QUERY).map_err(|e| format!("{language:?} i18n-calls.scm: {e}"))?;
    if language.has_jsx() {
        Query::new(&grammar, JSX_QUERY).map_err(|e| format!("{language:?} i18n-jsx.scm: {e}"))?;
    }
    Ok(())
}

/// Every message `source` declares, and every call site that should have
/// declared one and could not be read.
///
/// `file` is only carried into the output — nothing is read from disk here, so
/// a caller scanning a file store passes the store's path and a caller scanning
/// a checkout passes the path relative to its root.
pub fn extract_js(language: Language, file: &str, source: &[u8]) -> Extraction {
    scan(language, file, source).0
}

/// Every literal a person reads that no `t` wraps (task 2.2).
pub fn lint_js(language: Language, file: &str, source: &[u8]) -> Vec<Finding> {
    scan(language, file, source).1
}

/// One parse, both answers. The parse is the expensive part and every caller
/// that wants one wants the other soon after — `feldspar i18n check` prints
/// coverage and unwrapped literals from the same run, and so does the
/// Translations screen.
pub fn scan(language: Language, file: &str, source: &[u8]) -> (Extraction, Vec<Finding>) {
    let mut extraction = Extraction::default();
    let mut findings = Vec::new();
    if source.len() > MAX_PARSED_BYTES {
        return (extraction, findings);
    }
    let Some(queries) = queries(language) else {
        return (extraction, findings);
    };
    let mut parser = Parser::new();
    if parser.set_language(&language.grammar()).is_err() {
        return (extraction, findings);
    }
    let Some(tree) = parser.parse(source, None) else {
        return (extraction, findings);
    };
    let root = tree.root_node();

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&queries.calls, root, source);
    while let Some(m) = matches.next() {
        let mut function = None;
        let mut arguments = None;
        for capture in m.captures() {
            match queries.calls.capture_names()[capture.index as usize] {
                "function" => function = Some(capture.node),
                "arguments" => arguments = Some(capture.node),
                _ => {}
            }
        }
        let (Some(function), Some(arguments)) = (function, arguments) else {
            continue;
        };
        let name = function.utf8_text(source).unwrap_or_default();
        if name == "t" || name == "tc" {
            read_call(name, function, arguments, file, source, &mut extraction);
        }
    }

    let Some(jsx) = &queries.jsx else {
        return (extraction, findings);
    };
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(jsx, root, source);
    while let Some(m) = matches.next() {
        match m.pattern_index {
            // An opening or self-closing element. Only `<T …>` is a message;
            // everything else is somebody's component.
            0 | 1 => {
                let mut element = None;
                let mut node = None;
                for capture in m.captures() {
                    match jsx.capture_names()[capture.index as usize] {
                        "element" => element = Some(capture.node),
                        "element_node" => node = Some(capture.node),
                        _ => {}
                    }
                }
                let (Some(element), Some(node)) = (element, node) else {
                    continue;
                };
                if element.utf8_text(source).unwrap_or_default() == T_ELEMENT {
                    read_t_element(node, file, source, &mut extraction);
                }
            }
            // An attribute with a string value.
            2 => {
                let mut attribute = None;
                let mut value = None;
                for capture in m.captures() {
                    match jsx.capture_names()[capture.index as usize] {
                        "attribute" => attribute = Some(capture.node),
                        "value" => value = Some(capture.node),
                        _ => {}
                    }
                }
                let (Some(attribute), Some(value)) = (attribute, value) else {
                    continue;
                };
                let name = attribute.utf8_text(source).unwrap_or_default();
                if !LINTED_ATTRIBUTES.contains(&name) {
                    continue;
                }
                let Some(text) = literal(value, source) else {
                    continue;
                };
                if super::looks_like_prose(&text) {
                    findings.push(Finding {
                        file: file.to_owned(),
                        line: line_of(value),
                        text: quote(&text),
                        what: Unwrapped::Attribute(name.to_owned()),
                    });
                }
            }
            // Text between tags.
            3 => {
                let Some(node) = m.captures().first().map(|c| c.node) else {
                    continue;
                };
                let text = node.utf8_text(source).unwrap_or_default();
                if super::looks_like_prose(text) {
                    findings.push(Finding {
                        file: file.to_owned(),
                        line: line_of(node),
                        text: quote(text),
                        what: Unwrapped::JsxText,
                    });
                }
            }
            _ => {}
        }
    }

    (extraction, findings)
}

/// `t("…")` or `tc("context", "…")`.
///
/// Every failure here is a [`Problem`] and never a silent skip — the rule in
/// the module documentation. The message says what was expected, because the
/// fix is nearly always to hoist a computed string into a `t()` of its own.
fn read_call(
    name: &str,
    function: Node<'_>,
    arguments: Node<'_>,
    file: &str,
    source: &[u8],
    out: &mut Extraction,
) {
    let line = line_of(function);
    let mut cursor = arguments.walk();
    let args: Vec<Node<'_>> = arguments
        .named_children(&mut cursor)
        .filter(|n| n.kind() != "comment")
        .collect();
    let wanted = match name {
        "tc" => 2,
        _ => 1,
    };
    if args.len() < wanted {
        out.problems.push(Problem {
            file: file.to_owned(),
            line,
            message: match name {
                "tc" => "tc() needs a context and a message: tc(\"verb\", \"Order\")".to_owned(),
                _ => "t() needs a message: t(\"Add a task\")".to_owned(),
            },
        });
        return;
    }
    let mut literals = Vec::new();
    for arg in args.iter().take(wanted) {
        match literal(*arg, source) {
            Some(text) => literals.push(text),
            None => {
                out.problems.push(Problem {
                    file: file.to_owned(),
                    line: line_of(*arg),
                    message: format!(
                        "{name}() was given `{}`, which is not a string literal — the message id \
                         is the English source text, so it has to be written at the call site",
                        quote(arg.utf8_text(source).unwrap_or_default())
                    ),
                });
                return;
            }
        }
    }
    let key = match name {
        "tc" => context_key(&literals[0], &literals[1]),
        _ => literals[0].clone(),
    };
    out.messages.push(Extracted {
        key,
        file: file.to_owned(),
        line,
    });
}

/// `<T text="…" />`, optionally with `context="…"`.
fn read_t_element(node: Node<'_>, file: &str, source: &[u8], out: &mut Extraction) {
    let line = line_of(node);
    let mut cursor = node.walk();
    let mut text = None;
    let mut context = None;
    for child in node.named_children(&mut cursor) {
        if child.kind() != "jsx_attribute" {
            continue;
        }
        let Some(name) = child.named_child(0) else {
            continue;
        };
        let name = name.utf8_text(source).unwrap_or_default();
        if name != "text" && name != "context" {
            continue;
        }
        let Some(value) = child.named_child(1) else {
            continue;
        };
        let Some(read) = literal(value, source) else {
            out.problems.push(Problem {
                file: file.to_owned(),
                line: line_of(value),
                message: format!(
                    "<T {name}=…> is not a string literal — the message id is the English \
                     source text, so it has to be written at the call site"
                ),
            });
            return;
        };
        match name {
            "text" => text = Some(read),
            _ => context = Some(read),
        }
    }
    let Some(text) = text else {
        out.problems.push(Problem {
            file: file.to_owned(),
            line,
            message: "<T> has no text=\"…\"".to_owned(),
        });
        return;
    };
    out.messages.push(Extracted {
        key: match context {
            Some(context) => context_key(&context, &text),
            None => text,
        },
        file: file.to_owned(),
        line,
    });
}

/// 1-based, as every compiler and editor counts.
fn line_of(node: Node<'_>) -> u32 {
    u32::try_from(node.start_position().row + 1).unwrap_or(u32::MAX)
}

/// The value of a string literal, or `None` when the node is not one.
///
/// A template literal **with no substitution** counts: `` t(`Add a task`) `` is
/// a string somebody wrote in backticks, and refusing it would be a rule about
/// quotation marks rather than about translation. One with a `${…}` in it does
/// not, and is the error this returns `None` for — the interpolation has to
/// become a `{placeholder}` and an argument.
fn literal(node: Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "string" | "template_string" => {}
        _ => return None,
    }
    let mut out = String::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "string_fragment" => out.push_str(child.utf8_text(source).ok()?),
            "escape_sequence" => out.push_str(&unescape(child.utf8_text(source).ok()?)),
            // `${…}`, and anything else a future grammar puts in here.
            _ => return None,
        }
    }
    Some(out)
}

/// One JavaScript escape sequence, decoded. Anything unrecognised is the
/// character itself, which is what the language says.
fn unescape(escape: &str) -> String {
    let mut chars = escape.chars();
    if chars.next() != Some('\\') {
        return escape.to_owned();
    }
    let Some(c) = chars.next() else {
        return String::new();
    };
    let rest: String = chars.collect();
    match c {
        'n' => "\n".to_owned(),
        't' => "\t".to_owned(),
        'r' => "\r".to_owned(),
        'b' => "\u{8}".to_owned(),
        'f' => "\u{c}".to_owned(),
        'v' => "\u{b}".to_owned(),
        '0' if rest.is_empty() => "\0".to_owned(),
        'x' => from_hex(&rest).unwrap_or_else(|| escape.to_owned()),
        'u' => {
            let digits = rest.trim_start_matches('{').trim_end_matches('}');
            from_hex(digits).unwrap_or_else(|| escape.to_owned())
        }
        // A line continuation: a backslash at the end of a line is nothing.
        '\n' => String::new(),
        other => other.to_string(),
    }
}

fn from_hex(digits: &str) -> Option<String> {
    let code = u32::from_str_radix(digits, 16).ok()?;
    Some(char::from_u32(code)?.to_string())
}
