//! What a build's output *said*, parsed into things a reader can act on.
//!
//! A build that failed is not an exception, it is news — and the useful part of
//! that news is a list of file/line/message triples, not a wall of tool output.
//! Two callers want exactly that list and would otherwise each parse it:
//!
//! - `sc_core_traits::build_application`, the agent trait whose whole argument
//!   is that "the build's diagnostics are the tool result";
//! - the MCP projection of `buildApplication` (§13.6), where a coding agent that
//!   just changed a schema rebuilds the application it changed and has to be
//!   told what did not compile.
//!
//! Two copies of these regexes would be two answers to "what did tsc say?", so
//! the parser lives at the layer that owns the build itself.
//!
//! Anything unrecognised is **not lost**: every caller sends the whole output
//! beside this list, and this is the index into it rather than a replacement
//! for it.

use serde_json::{Value as Json, json};

/// One diagnostic the build tools reported.
#[derive(Debug, PartialEq, Eq)]
struct Diagnostic {
    file: String,
    line: u32,
    column: u32,
    message: String,
}

/// The diagnostics a build log names.
///
/// The Rust sibling of the IDE's `buildDiagnostics.ts`, understanding the same
/// shapes for the same reason: the type errors exist only in the tools' output,
/// and file/line/message is what a reader — a person in the Problems panel, a
/// model deciding what to edit — can act on.
pub fn build_diagnostics(log: &str) -> Vec<Json> {
    // `src/App.tsx(12,5): error TS2322: Type 'x' is not assignable…` — tsc.
    let tsc = regex_lite::Regex::new(
        r"^(\S[^(]*)\((\d+),(\d+)\):\s*(?:error|warning)\s+([A-Za-z]+\d+):\s*(.+)$",
    );
    // `src/App.tsx:12:5: ERROR: Expected ";"` — esbuild and its imitators.
    let positioned = regex_lite::Regex::new(
        r"^\s*(?:\[[^\]]*\]\s*)?([^\s:]+\.[A-Za-z0-9]+):(\d+):(\d+):\s*(?:(?:ERROR|WARNING|error|warning):\s*)?(\S.*)$",
    );
    // `╭─[ src/main.ts:2:1 ]` — rolldown's boxed report, whose message is the
    // line above the frame.
    let frame = regex_lite::Regex::new(r"[╭┌][─-]*\[\s*([^\s\]]+?):(\d+):(\d+)\s*\]");
    let (Ok(tsc), Ok(positioned), Ok(frame)) = (tsc, positioned, frame) else {
        // A pattern that does not compile is this module's bug, not the build's;
        // the caller still gets the whole output.
        return Vec::new();
    };

    let mut found: Vec<Diagnostic> = Vec::new();
    let mut previous = String::new();
    for raw in log.lines() {
        let line = strip_ansi(raw);
        let line = line.trim_end();
        if let Some(c) = frame.captures(line) {
            push(&mut found, &c, 1, 2, 3, previous.trim());
            continue;
        }
        if let Some(c) = tsc.captures(line) {
            let message = format!("{}: {}", &c[4], &c[5]);
            push(&mut found, &c, 1, 2, 3, &message);
            previous.clear();
            continue;
        }
        if let Some(c) = positioned.captures(line) {
            let message = c[4].to_owned();
            push(&mut found, &c, 1, 2, 3, &message);
            previous.clear();
            continue;
        }
        if !line.trim().is_empty() {
            previous = line.trim().to_owned();
        }
    }
    found
        .iter()
        .map(|d| {
            json!({
                "file": d.file,
                "line": d.line,
                "column": d.column,
                "message": d.message,
            })
        })
        .collect()
}

/// Record one diagnostic, skipping a duplicate and one with nothing to say.
fn push(
    found: &mut Vec<Diagnostic>,
    caps: &regex_lite::Captures<'_>,
    file: usize,
    line: usize,
    column: usize,
    message: &str,
) {
    if message.trim().is_empty() {
        return;
    }
    let diagnostic = Diagnostic {
        file: caps[file].to_owned(),
        line: caps[line].parse().unwrap_or(1),
        column: caps[column].parse().unwrap_or(1),
        message: message.trim().to_owned(),
    };
    if !found.contains(&diagnostic) {
        found.push(diagnostic);
    }
}

/// Terminal colour, which a build that thought it had a TTY leaves behind.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tscs_diagnostics_are_parsed_with_their_file_and_line() {
        let log = "\
build command `npm run build` failed in /srv/store/web with exit status: 2
src/App.tsx(12,5): error TS2322: Type 'number' is not assignable to type 'string'.
src/App.tsx(19,1): error TS1005: ';' expected.
";
        let found = build_diagnostics(log);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0]["file"], "src/App.tsx");
        assert_eq!(found[0]["line"], 12);
        assert_eq!(found[0]["column"], 5);
        assert!(
            found[0]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("not assignable"),
            "{found:?}"
        );
    }

    #[test]
    fn esbuilds_one_line_form_is_parsed_too_and_colour_is_ignored() {
        let log = "\u{1b}[31msrc/main.ts:2:1: ERROR: Expected \";\" but found \"}\"\u{1b}[0m";
        let found = build_diagnostics(log);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0]["file"], "src/main.ts");
        assert_eq!(found[0]["line"], 2);
        assert_eq!(found[0]["message"], "Expected \";\" but found \"}\"");
    }

    #[test]
    fn a_boxed_report_takes_its_message_from_the_line_above_it() {
        let log = "\
[builtin:vite-transform] 'export' modifier cannot be used here.
   ╭─[ src/main.ts:2:1 ]
";
        let found = build_diagnostics(log);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0]["file"], "src/main.ts");
        assert!(
            found[0]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("cannot be used here"),
            "{found:?}"
        );
    }

    #[test]
    fn output_with_no_diagnostics_in_it_produces_none() {
        assert!(
            build_diagnostics("vite v5.0.0 building for production...\n✓ 34 modules").is_empty()
        );
    }
}
