//! The extractor and the lint (tasks 2.1–2.3).
//!
//! The fixtures beside this file are read as bytes rather than compiled: they
//! are what a screen looks like, including the two call sites that are errors,
//! and a `.tsx` that does not type-check is exactly the input this pass has to
//! survive.

use sc_i18n::CONTEXT_SEPARATOR;
use sc_i18n::extract::js::{Language, queries_compile, scan};
use sc_i18n::extract::{Unwrapped, extract_js, extract_rust, lint_js};

const SAMPLE: &str = include_str!("../fixtures/extract-sample.tsx");
const DIRTY: &str = include_str!("../fixtures/lint-dirty.tsx");
const CLEAN: &str = include_str!("../fixtures/lint-clean.tsx");

/// A query that does not compile makes every file in its language silently
/// empty — the one failure this pass must not have, and the one a test of the
/// *output* would not notice on a grammar upgrade.
#[test]
fn every_vendored_query_compiles_against_its_grammar() {
    for language in [Language::JavaScript, Language::TypeScript, Language::Tsx] {
        queries_compile(language).expect("the vendored queries compile");
    }
}

#[test]
fn a_tsx_file_yields_its_calls_its_contexts_and_its_t_elements() {
    let found = extract_js(Language::Tsx, "Screen.tsx", SAMPLE.as_bytes());
    let keys = found.keys();
    assert!(keys.contains(&"Add a task".to_owned()), "{keys:?}");
    assert!(keys.contains(&"{count} rows".to_owned()), "{keys:?}");
    assert!(keys.contains(&"Delete \"{name}\"?".to_owned()), "{keys:?}");
    // `tc("verb", "Order")` and `<T text="Order" context="noun" />` are two
    // different messages that read the same in English — which is the whole
    // reason a context exists.
    assert!(
        keys.contains(&format!("verb{CONTEXT_SEPARATOR}Order")),
        "{keys:?}"
    );
    assert!(
        keys.contains(&format!("noun{CONTEXT_SEPARATOR}Order")),
        "{keys:?}"
    );
    // A template literal with no substitution is a string somebody wrote in
    // backticks, and the `<T>` and the `t()` spelling of the same English are
    // one key.
    let sites = found.sites();
    assert_eq!(sites["Add a task"].len(), 2, "{:?}", sites["Add a task"]);
}

#[test]
fn a_call_whose_message_is_not_a_literal_is_an_error_naming_its_line() {
    let found = extract_js(Language::Tsx, "Screen.tsx", SAMPLE.as_bytes());
    assert_eq!(found.problems.len(), 2, "{:?}", found.problems);
    for problem in &found.problems {
        assert_eq!(problem.file, "Screen.tsx");
        assert!(problem.line > 0);
        assert!(
            problem.message.contains("not a string literal"),
            "{problem}"
        );
    }
    // The line is the argument's, and the two are on consecutive lines of the
    // fixture — so a wrong line here is a wrong line, not an off-by-one that
    // happens to look plausible.
    let lines: Vec<u32> = found.problems.iter().map(|p| p.line).collect();
    assert_eq!(lines[1], lines[0] + 1, "{lines:?}");
}

#[test]
fn the_lint_reports_one_of_each_and_nothing_on_a_clean_file() {
    let findings = lint_js(Language::Tsx, "TaskRow.tsx", DIRTY.as_bytes());
    let kinds: Vec<(&str, &str)> = findings
        .iter()
        .map(|f| {
            (
                match &f.what {
                    Unwrapped::JsxText => "text",
                    Unwrapped::Attribute(name) => name.as_str(),
                },
                f.text.as_str(),
            )
        })
        .collect();
    for wanted in [
        ("text", "Overdue since yesterday"),
        ("placeholder", "Search tasks"),
        ("title", "Remove this task"),
        ("aria-label", "Open in a new tab"),
        ("label", "Due date"),
    ] {
        assert!(kinds.contains(&wanted), "{wanted:?} missing from {kinds:?}");
    }
    // `className`, `id`, `name`, `type` and `href` are strings nobody reads.
    assert_eq!(kinds.len(), 5, "{kinds:?}");

    assert!(
        lint_js(Language::Tsx, "TaskRow.tsx", CLEAN.as_bytes()).is_empty(),
        "{:?}",
        lint_js(Language::Tsx, "TaskRow.tsx", CLEAN.as_bytes())
    );
}

/// The clean fixture is also the extraction's: every wrapped string is a key.
#[test]
fn one_parse_answers_both_questions() {
    let (found, findings) = scan(Language::Tsx, "TaskRow.tsx", CLEAN.as_bytes());
    assert!(findings.is_empty(), "{findings:?}");
    assert_eq!(
        found.keys(),
        vec![
            "Delete".to_owned(),
            "Due".to_owned(),
            "Open in a new tab".to_owned(),
            "Overdue since yesterday".to_owned(),
            "Remove this task".to_owned(),
            "Search tasks".to_owned(),
        ]
    );
}

/// `.ts` has no JSX: `<T text="…">` there is a type assertion, and the
/// TypeScript grammar is the one that says so.
#[test]
fn the_typescript_grammar_reads_calls_and_no_jsx() {
    let source = r#"
        export function messages(t: (s: string) => string) {
          return [t("Save"), t('Cancel'), tc("verb", "Order")];
        }
    "#;
    let found = extract_js(Language::TypeScript, "messages.ts", source.as_bytes());
    assert_eq!(
        found.keys(),
        vec![
            "Cancel".to_owned(),
            "Save".to_owned(),
            format!("verb{CONTEXT_SEPARATOR}Order"),
        ]
    );
    assert!(found.problems.is_empty(), "{:?}", found.problems);
}

// --------------------------------------------------------------------------
// The Rust scanner (2.3).
// --------------------------------------------------------------------------

#[test]
fn the_rust_scanner_reads_both_macros_and_skips_what_is_not_code() {
    let source = r##"
        /// An example in the documentation: t!(loc, "Not a real message").
        // And a line comment: tc!(loc, "verb", "Neither is this").
        fn messages(loc: &Locale) -> Vec<String> {
            let _ = format!("a {} b", 1);
            let quote = '\'';
            let lifetime: &'static str = "t!(loc, \"nor this\")";
            vec![
                t!(loc, "Incorrect password"),
                t!(loc, "Delete {name}?", name = "Tasks"),
                tc!(loc, "verb", "Order"),
                t!(self.request.locale(), r#"Raw "quoted" message"#),
                t!(loc, "A message split \
                   across two lines"),
            ]
        }
    "##;
    let found = extract_rust("crates/sc-demo/src/lib.rs", source);
    assert_eq!(
        found.keys(),
        vec![
            "A message split across two lines".to_owned(),
            "Delete {name}?".to_owned(),
            "Incorrect password".to_owned(),
            "Raw \"quoted\" message".to_owned(),
            format!("verb{CONTEXT_SEPARATOR}Order"),
        ],
        "{:?}",
        found.keys()
    );
    assert!(found.problems.is_empty(), "{:?}", found.problems);
    assert_eq!(found.messages[0].file, "crates/sc-demo/src/lib.rs");
}

#[test]
fn a_rust_call_whose_message_is_a_constant_is_an_error() {
    let source = "fn f(loc: &Locale) -> String { t!(loc, MESSAGE) }";
    let found = extract_rust("lib.rs", source);
    assert!(found.messages.is_empty(), "{:?}", found.messages);
    assert_eq!(found.problems.len(), 1, "{:?}", found.problems);
    assert!(
        found.problems[0].message.contains("MESSAGE"),
        "{}",
        found.problems[0]
    );
    assert_eq!(found.problems[0].line, 1);
}

/// Every crate here keeps its unit tests at the foot of the file it tests, and
/// they are full of `t!(loc, "Save changes")`. A shipped catalogue must not be.
#[test]
fn a_cfg_test_module_is_not_a_source_of_messages() {
    let source = r#"
        pub fn greeting(loc: &Locale) -> String {
            t!(loc, "Welcome back")
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn it_greets() {
                let brace = '}';
                assert_eq!(t!(loc, "A fixture nobody reads"), "A fixture nobody reads");
            }
        }

        pub fn farewell(loc: &Locale) -> String {
            t!(loc, "Goodbye")
        }
    "#;
    let found = extract_rust("lib.rs", source);
    assert_eq!(
        found.keys(),
        vec!["Goodbye".to_owned(), "Welcome back".to_owned()],
        "{:?}",
        found.keys()
    );
}
