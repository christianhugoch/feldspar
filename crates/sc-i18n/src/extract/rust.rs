//! The Rust side: `t!(` and `tc!(` call sites in `crates/**/*.rs` (task 2.3).
//!
//! A lexer, not a parser, and deliberately: what has to be recognised is a
//! macro name followed by a locale expression and one or two string literals,
//! and everything that makes Rust hard to parse — generics, turbofish, where
//! clauses, macros that are not this one — is exactly the part this never has
//! to understand. What it *does* have to understand is which bytes are code:
//! `///` examples in this crate's own documentation are full of `t!(loc, "…")`,
//! and a scanner that read them would file the documentation's sentences in the
//! shipped catalogue. So comments, string literals (raw ones included), byte
//! strings, character literals and lifetimes are all skipped, and a macro is
//! only a macro when it starts at a token boundary — which is also what keeps
//! `format!(` from being read as `t!(`.
//!
//! One more thing is skipped, and it is the one that would otherwise put
//! nonsense in a shipped catalogue: a `#[cfg(test)] mod tests` block. Every
//! crate in this workspace has one at the foot of its files, they are full of
//! `t!(loc, "Save changes")`, and a translator handed that sentence has no way
//! to know it is a fixture nobody reads.
//!
//! The same [`Extraction`] the TypeScript pass produces, and the same rule: a
//! call whose message is not a literal is a [`Problem`] naming file and line.

use super::{Extracted, Extraction, Problem, quote};
use crate::catalog::context_key;

/// Every `core` message `source` declares.
///
/// `file` is carried into the output and nothing else — the caller says what to
/// call the file, so a checkout's paths are relative to its root.
pub fn extract_rust(file: &str, source: &str) -> Extraction {
    let src = source.as_bytes();
    let lines = line_starts(src);
    let mut out = Extraction::default();
    let mut i = 0usize;
    while i < src.len() {
        let b = src[i];
        // A comment: everything in it, including a `t!(…)` in an example.
        if b == b'/' && matches!(src.get(i + 1), Some(b'/') | Some(b'*')) {
            i = skip_comment(src, i);
            continue;
        }
        // A string literal, of any of Rust's spellings.
        if b == b'"' || ((b == b'r' || b == b'b') && !ident_char(src.get(i.wrapping_sub(1)))) {
            if let Some((_, next)) = string_at(src, i) {
                i = next;
                continue;
            }
        }
        if b == b'\'' {
            i = skip_char_or_lifetime(src, i);
            continue;
        }
        // `#[cfg(test)] mod tests { … }` — the whole block, messages and all.
        if b == b'#' && src[i..].starts_with(CFG_TEST) {
            if let Some(next) = skip_test_module(src, i + CFG_TEST.len()) {
                i = next;
                continue;
            }
        }
        if b == b't' && !ident_char(src.get(i.wrapping_sub(1))) {
            if src[i..].starts_with(b"tc!(") {
                read_macro("tc", src, i + 4, file, &lines, i, &mut out);
            } else if src[i..].starts_with(b"t!(") {
                read_macro("t", src, i + 3, file, &lines, i, &mut out);
            }
        }
        if !ident_char(src.get(i.wrapping_sub(1))) {
            read_declaration(src, i, file, &lines, &mut out);
        }
        i += 1;
    }
    out
}

/// Read `t!(loc, "…"[, name = v]…)` or `tc!(loc, "ctx", "…"[, …])`.
///
/// `start` is just past the opening parenthesis; `at` is where the macro name
/// began, which is the line every message about this call site names.
fn read_macro(
    name: &str,
    src: &[u8],
    start: usize,
    file: &str,
    lines: &[usize],
    at: usize,
    out: &mut Extraction,
) {
    let line = line_at(lines, at);
    let problem = |out: &mut Extraction, message: String| {
        out.problems.push(Problem {
            file: file.to_owned(),
            line,
            message,
        });
    };
    // The locale is the first argument and is an arbitrary expression — a
    // field, a method call, a `&self.locale`. It is skipped rather than read.
    let Some(comma) = top_level_comma(src, start) else {
        problem(
            out,
            format!("{name}! needs a locale and a message: {name}!(locale, …)"),
        );
        return;
    };
    let mut cursor = comma + 1;
    let mut literals = Vec::new();
    let wanted = match name {
        "tc" => 2,
        _ => 1,
    };
    for nth in 0..wanted {
        if nth > 0 {
            let Some(comma) = top_level_comma(src, cursor) else {
                problem(
                    out,
                    "tc! needs a context and a message: tc!(locale, \"verb\", \"Order\")"
                        .to_owned(),
                );
                return;
            };
            cursor = comma + 1;
        }
        let cursor_at = skip_trivia(src, cursor);
        match string_at(src, cursor_at) {
            Some((value, next)) => {
                literals.push(value);
                cursor = next;
            }
            None => {
                problem(
                    out,
                    format!(
                        "{name}! was given `{}`, which is not a string literal — the message id \
                         is the English source text, so it has to be written at the call site",
                        quote(&snippet(src, cursor_at))
                    ),
                );
                return;
            }
        }
    }
    out.messages.push(Extracted {
        key: match name {
            "tc" => context_key(&literals[0], &literals[1]),
            _ => literals[0].clone(),
        },
        file: file.to_owned(),
        line,
    });
}

/// The names a *declared* label is written under.
///
/// The second half of the `core` domain, and the reason it needs one: the
/// settings screen's headings, a stream provider's `Broker URL`, a file
/// backend's operations are **data**, not `t!` call sites, and the server
/// translates them at the API edge with `translate_spec` (§16.x, D5). A
/// catalogue that never heard of them is a `translate_spec` that can never
/// find anything, so the scanner that fills the catalogue has to read them
/// where they are written — which is a builder method or a struct field, and
/// never a macro.
///
/// Five names and not "every string": these are the ones a person reads. A
/// `name` is an identifier, a `key` is a key, and a lint that swept those up
/// would hand a translator a list of field names to translate.
const DECLARED: &[&[u8]] = &[b"label", b"description", b"sublabel", b"help", b"blurb"];

/// A declared label at `at`: `.label("…")`, `label: "…"`, or
/// `ConfigDef::help(field, "…")`.
///
/// A non-literal is **skipped silently**, which is where this parts company
/// with [`read_macro`]. A `t!` whose message is computed is a message nothing
/// can ever translate and therefore a bug worth reporting; a `.label(name)` on
/// a field built in a loop is an ordinary thing to write, it is not a call site
/// somebody forgot to wrap, and reporting it would make the lint noise that
/// gets a lint turned off.
fn read_declaration(src: &[u8], at: usize, file: &str, lines: &[usize], out: &mut Extraction) {
    let Some(name) = DECLARED.iter().find(|n| src[at..].starts_with(n)) else {
        return;
    };
    let after = at + name.len();
    // `help` is the one written as a free function taking the field first:
    // `ConfigDef::help(FormField::new(…), "what this setting does")`.
    let arg = match src.get(after) {
        Some(b':') if src.get(after + 1) != Some(&b':') => skip_trivia(src, after + 1),
        Some(b'(') if *name == b"help" => match top_level_comma(src, after + 1) {
            Some(comma) => skip_trivia(src, comma + 1),
            None => skip_trivia(src, after + 1),
        },
        Some(b'(') => skip_trivia(src, after + 1),
        _ => return,
    };
    if let Some((value, _)) = string_at(src, arg)
        && !value.trim().is_empty()
    {
        out.messages.push(Extracted {
            key: value,
            file: file.to_owned(),
            line: line_at(lines, at),
        });
    }
}

/// Spelled as rustfmt writes it, which is how every one of them in this
/// workspace is spelled.
const CFG_TEST: &[u8] = b"#[cfg(test)]";

/// Past a `#[cfg(test)] mod … { … }`, or `None` when the attribute is on
/// something else — a `use`, a single function, an item this need not know
/// about. `None` leaves the scan exactly where it was, so an unrecognised
/// shape costs nothing but the messages inside it being read.
fn skip_test_module(src: &[u8], after_attribute: usize) -> Option<usize> {
    let i = skip_trivia(src, after_attribute);
    if !src[i..].starts_with(b"mod") || ident_char(src.get(i + 3)) {
        return None;
    }
    let mut j = i + 3;
    while j < src.len() && src[j] != b'{' {
        // A `;` first means `#[cfg(test)] mod tests;` — a module in another
        // file, which this scan will reach on its own.
        if src[j] == b';' {
            return None;
        }
        j += 1;
    }
    (j < src.len()).then(|| skip_block(src, j))
}

/// Past the balanced block whose `{` is at `i`, with strings, comments and
/// character literals skipped so a brace inside one does not count.
fn skip_block(src: &[u8], mut i: usize) -> usize {
    let mut depth = 0usize;
    while i < src.len() {
        match src[i] {
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return i;
                }
            }
            b'/' if matches!(src.get(i + 1), Some(b'/') | Some(b'*')) => i = skip_comment(src, i),
            b'\'' => i = skip_char_or_lifetime(src, i),
            b'"' => i = string_at(src, i).map_or(src.len(), |(_, next)| next),
            b'r' | b'b' if !ident_char(src.get(i.wrapping_sub(1))) => {
                i = match string_at(src, i) {
                    Some((_, next)) => next,
                    None => i + 1,
                }
            }
            _ => i += 1,
        }
    }
    i
}

/// The index of the comma that ends this argument, or `None` when the call ends
/// first. Nesting, strings, comments and character literals are skipped.
fn top_level_comma(src: &[u8], mut i: usize) -> Option<usize> {
    let mut depth = 0i32;
    while i < src.len() {
        match src[i] {
            b'(' | b'[' | b'{' => {
                depth += 1;
                i += 1;
            }
            b')' | b']' | b'}' => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                i += 1;
            }
            b',' if depth == 0 => return Some(i),
            b'/' if matches!(src.get(i + 1), Some(b'/') | Some(b'*')) => i = skip_comment(src, i),
            b'\'' => i = skip_char_or_lifetime(src, i),
            b'"' => i = string_at(src, i).map_or(src.len(), |(_, next)| next),
            b'r' | b'b' if !ident_char(src.get(i.wrapping_sub(1))) => {
                i = match string_at(src, i) {
                    Some((_, next)) => next,
                    None => i + 1,
                }
            }
            _ => i += 1,
        }
    }
    None
}

/// Whitespace and comments, skipped.
fn skip_trivia(src: &[u8], mut i: usize) -> usize {
    loop {
        while i < src.len() && src[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < src.len() && src[i] == b'/' && matches!(src.get(i + 1), Some(b'/') | Some(b'*')) {
            i = skip_comment(src, i);
            continue;
        }
        return i;
    }
}

/// Past the end of the comment starting at `i`. Block comments nest, as Rust's
/// do.
fn skip_comment(src: &[u8], i: usize) -> usize {
    match src.get(i + 1) {
        Some(b'/') => {
            let mut j = i + 2;
            while j < src.len() && src[j] != b'\n' {
                j += 1;
            }
            j
        }
        Some(b'*') => {
            let mut j = i + 2;
            let mut depth = 1usize;
            while j < src.len() {
                if src[j..].starts_with(b"/*") {
                    depth += 1;
                    j += 2;
                } else if src[j..].starts_with(b"*/") {
                    depth -= 1;
                    j += 2;
                    if depth == 0 {
                        return j;
                    }
                } else {
                    j += 1;
                }
            }
            j
        }
        _ => i + 1,
    }
}

/// Past a character literal, or past the `'` of a lifetime.
///
/// `'a` is a lifetime and `'a'` is a character, and the difference is the
/// closing quote. Telling them apart matters because a lifetime's `'` would
/// otherwise open a literal that swallows the rest of the file.
fn skip_char_or_lifetime(src: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    if src.get(j) == Some(&b'\\') {
        j += 1;
        // `\u{1F600}` and `\x41` are longer than one byte after the backslash.
        while j < src.len() && src[j] != b'\'' && src[j] != b'\n' {
            j += 1;
        }
        return match src.get(j) {
            Some(b'\'') => j + 1,
            _ => j,
        };
    }
    // One character, then a quote, or it was a lifetime.
    let mut chars = src[j..].iter();
    let mut width = 0usize;
    if let Some(first) = chars.next() {
        width = utf8_width(*first);
    }
    match src.get(j + width) {
        Some(b'\'') => j + width + 1,
        _ => {
            while j < src.len() && ident_char(Some(&src[j])) {
                j += 1;
            }
            j
        }
    }
}

fn utf8_width(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// A string literal at `i`: `"…"`, `r"…"`, `r#"…"#`, `b"…"`, `br#"…"#`. The
/// value, and the index just past it.
fn string_at(src: &[u8], i: usize) -> Option<(String, usize)> {
    let mut j = i;
    if src.get(j) == Some(&b'b') {
        j += 1;
    }
    if src.get(j) == Some(&b'r') {
        j += 1;
        let mut hashes = 0usize;
        while src.get(j) == Some(&b'#') {
            hashes += 1;
            j += 1;
        }
        if src.get(j) != Some(&b'"') {
            return None;
        }
        j += 1;
        let start = j;
        let mut close = Vec::with_capacity(hashes + 1);
        close.push(b'"');
        close.extend(std::iter::repeat_n(b'#', hashes));
        while j < src.len() {
            if src[j..].starts_with(&close) {
                let value = String::from_utf8_lossy(&src[start..j]).into_owned();
                return Some((value, j + close.len()));
            }
            j += 1;
        }
        return None;
    }
    if src.get(j) != Some(&b'"') {
        return None;
    }
    j += 1;
    let mut value = String::new();
    while j < src.len() {
        match src[j] {
            b'"' => return Some((value, j + 1)),
            b'\\' => {
                let (decoded, next) = unescape(src, j);
                value.push_str(&decoded);
                j = next;
            }
            _ => {
                let width = utf8_width(src[j]);
                value.push_str(&String::from_utf8_lossy(
                    &src[j..(j + width).min(src.len())],
                ));
                j += width;
            }
        }
    }
    None
}

/// One Rust escape sequence at `i` (which is the backslash), decoded, and the
/// index just past it.
fn unescape(src: &[u8], i: usize) -> (String, usize) {
    let Some(c) = src.get(i + 1) else {
        return (String::new(), i + 1);
    };
    match c {
        b'n' => ("\n".to_owned(), i + 2),
        b't' => ("\t".to_owned(), i + 2),
        b'r' => ("\r".to_owned(), i + 2),
        b'0' => ("\0".to_owned(), i + 2),
        b'\\' => ("\\".to_owned(), i + 2),
        b'"' => ("\"".to_owned(), i + 2),
        b'\'' => ("'".to_owned(), i + 2),
        b'x' => {
            let digits = String::from_utf8_lossy(&src[i + 2..(i + 4).min(src.len())]).into_owned();
            match u32::from_str_radix(&digits, 16)
                .ok()
                .and_then(char::from_u32)
            {
                Some(c) => (c.to_string(), i + 4),
                None => ("\\x".to_owned(), i + 2),
            }
        }
        b'u' => {
            let mut j = i + 2;
            if src.get(j) != Some(&b'{') {
                return ("\\u".to_owned(), j);
            }
            j += 1;
            let start = j;
            while j < src.len() && src[j] != b'}' {
                j += 1;
            }
            let digits = String::from_utf8_lossy(&src[start..j]).into_owned();
            let next = (j + 1).min(src.len());
            match u32::from_str_radix(&digits, 16)
                .ok()
                .and_then(char::from_u32)
            {
                Some(c) => (c.to_string(), next),
                None => (String::new(), next),
            }
        }
        // A backslash before a newline eats the newline and the indentation
        // after it: a message written across two source lines is one line of
        // text, and the catalogue key has to be that one line.
        b'\n' => {
            let mut j = i + 2;
            while j < src.len() && src[j].is_ascii_whitespace() {
                j += 1;
            }
            (String::new(), j)
        }
        other => (char::from(*other).to_string(), i + 2),
    }
}

/// What a `Problem` quotes back: the argument as far as the next comma or
/// closing parenthesis, so the reader can see which call it is.
fn snippet(src: &[u8], i: usize) -> String {
    let end = top_level_comma(src, i).unwrap_or_else(|| {
        let mut j = i;
        let mut depth = 0i32;
        while j < src.len() {
            match src[j] {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' if depth == 0 => break,
                b')' | b']' | b'}' => depth -= 1,
                _ => {}
            }
            j += 1;
        }
        j
    });
    String::from_utf8_lossy(&src[i..end.min(src.len())]).into_owned()
}

fn ident_char(b: Option<&u8>) -> bool {
    matches!(b, Some(c) if c.is_ascii_alphanumeric() || *c == b'_')
}

fn line_starts(src: &[u8]) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in src.iter().enumerate() {
        if *b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

fn line_at(starts: &[usize], pos: usize) -> u32 {
    let index = starts.partition_point(|start| *start <= pos);
    u32::try_from(index.max(1)).unwrap_or(u32::MAX)
}
