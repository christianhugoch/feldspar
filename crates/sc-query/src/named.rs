//! Named parameters (`:name`) in admin-authored SQL, rewritten to the dialect's
//! own placeholders.
//!
//! A custom SQL query (§13.4) is written by an administrator, and `$1`/`?` is
//! not a thing anyone wants to count by hand — so the query is authored with
//! `:name` and rewritten here, once, at the point a [`Statement::Raw`] is
//! constructed. The values still arrive as **binds**; this only decides where
//! their placeholders go.
//!
//! Which means the scanner has one job and it must not get it wrong: a `:` that
//! is not a parameter must be left exactly as it was found. There are five ways
//! to write one, and every one of them is a way to corrupt an admin's query
//! silently, so each is skipped explicitly:
//!
//! - a **single-quoted literal**, `'a:b'` (with `''` for an embedded quote),
//! - a **dollar-quoted body**, `$fn$ … :not_a_param … $fn$` (any tag, including
//!   the empty `$$`),
//! - a **quoted identifier**, `"weird:column"`,
//! - a **line comment** (`-- :note`) or a **block comment** (`/* :note */`,
//!   which Postgres nests),
//! - a **cast**, `x::text` — which is why `::` is checked before `:`; read the
//!   other way round, that column would acquire a parameter called `text`.
//!
//! The same scan counts statements, because the two questions have the same
//! answer source: a `;` inside a literal or a comment does not separate
//! anything, and "a custom query is one statement" is a rule that has to be
//! decided on the same reading of the text that the rewriting is.

use sc_error::{Error, Result};

use crate::SqlDialect;

/// Admin SQL with its `:name` parameters rewritten to dialect placeholders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedSql {
    /// The SQL, with `:name` replaced by this dialect's placeholders.
    pub sql: String,
    /// The parameter names, in **placeholder order** — the order the binds must
    /// be supplied in. A name used twice takes one placeholder and appears once,
    /// so `:id` on both sides of an `OR` is one bind value, not two.
    pub params: Vec<String>,
    /// How many `;`-separated statements the text holds. Zero for SQL that is
    /// only whitespace and comments; more than one is a multi-statement body,
    /// which a custom query may not be.
    pub statements: usize,
}

/// Rewrite `:name` parameters in `sql` to `dialect`'s placeholders, reporting
/// the parameter names in placeholder order.
///
/// See the module documentation for what is deliberately *not* rewritten. An
/// unterminated literal, dollar-quoted body or block comment is an error rather
/// than a guess: the rest of the text cannot be classified once a delimiter is
/// missing, and rewriting on a guess is exactly the silent corruption this
/// scanner exists to avoid.
pub fn rewrite_named_params<D: SqlDialect + ?Sized>(dialect: &D, sql: &str) -> Result<NamedSql> {
    let b = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut params: Vec<String> = Vec::new();
    let mut statements = 0usize;
    // Whether anything other than whitespace and comments has appeared since the
    // last `;` — an empty tail (`select 1;`) is not a second statement.
    let mut has_code = false;
    let mut i = 0;

    while i < b.len() {
        match b[i] {
            b'\'' => {
                let end = quoted_end(b, i, b'\'')
                    .ok_or_else(|| Error::query("unterminated string literal in SQL"))?;
                out.push_str(&sql[i..end]);
                has_code = true;
                i = end;
            }
            b'"' => {
                let end = quoted_end(b, i, b'"')
                    .ok_or_else(|| Error::query("unterminated quoted identifier in SQL"))?;
                out.push_str(&sql[i..end]);
                has_code = true;
                i = end;
            }
            // A dollar quote (`$tag$ … $tag$`) — but `$1` is a placeholder and
            // `$` alone is an operator character, so this only applies when a
            // closing `$` follows an identifier-shaped tag.
            b'$' if let Some(tag) = dollar_tag(b, i) => {
                let body = i + tag.len();
                let end = find(b, body, tag.as_bytes()).ok_or_else(|| {
                    Error::query(format!("unterminated dollar-quoted body ({tag}) in SQL"))
                })? + tag.len();
                out.push_str(&sql[i..end]);
                has_code = true;
                i = end;
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                let end = b[i..]
                    .iter()
                    .position(|&c| c == b'\n')
                    .map_or(b.len(), |p| i + p);
                out.push_str(&sql[i..end]);
                i = end;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let end = block_comment_end(b, i)
                    .ok_or_else(|| Error::query("unterminated block comment in SQL"))?;
                out.push_str(&sql[i..end]);
                i = end;
            }
            // A cast, not a parameter: `x::text` names a type, not an argument.
            b':' if b.get(i + 1) == Some(&b':') => {
                out.push_str("::");
                has_code = true;
                i += 2;
            }
            b':' if b.get(i + 1).is_some_and(|c| is_ident_start(*c)) => {
                let mut end = i + 1;
                while end < b.len() && is_ident_char(b[end]) {
                    end += 1;
                }
                let name = &sql[i + 1..end];
                let position = match params.iter().position(|p| p == name) {
                    Some(found) => found,
                    None => {
                        params.push(name.to_owned());
                        params.len() - 1
                    }
                };
                out.push_str(&dialect.placeholder(position + 1));
                has_code = true;
                i = end;
            }
            b';' => {
                if has_code {
                    statements += 1;
                    has_code = false;
                }
                out.push(';');
                i += 1;
            }
            _ => {
                // Everything up to the next byte that could start one of the
                // above, copied verbatim. The delimiters are all ASCII, so the
                // run always ends on a character boundary.
                let start = i;
                i += 1;
                while i < b.len() && !is_delimiter(b[i]) {
                    i += 1;
                }
                let chunk = &sql[start..i];
                if chunk.bytes().any(|c| !c.is_ascii_whitespace()) {
                    has_code = true;
                }
                out.push_str(chunk);
            }
        }
    }
    if has_code {
        statements += 1;
    }
    Ok(NamedSql {
        sql: out,
        params,
        statements,
    })
}

/// The bytes that end a verbatim run — every one of them ASCII, so slicing at
/// one is always a character boundary.
fn is_delimiter(c: u8) -> bool {
    matches!(c, b'\'' | b'"' | b'$' | b':' | b';' | b'-' | b'/')
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// The index just past a quoted run starting at `start` (whose byte is `quote`),
/// honouring the doubled-quote escape SQL uses for both `'` and `"`.
fn quoted_end(b: &[u8], start: usize, quote: u8) -> Option<usize> {
    let mut i = start + 1;
    while i < b.len() {
        if b[i] == quote {
            if b.get(i + 1) == Some(&quote) {
                i += 2;
                continue;
            }
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

/// The dollar-quote tag opening at `start` (`$$`, `$fn$`, …), or `None` when the
/// `$` is something else — a positional placeholder or an operator.
fn dollar_tag(b: &[u8], start: usize) -> Option<String> {
    let mut i = start + 1;
    while i < b.len() && is_ident_char(b[i]) && !b[i].is_ascii_digit() {
        i += 1;
    }
    (b.get(i) == Some(&b'$')).then(|| String::from_utf8_lossy(&b[start..=i]).into_owned())
}

/// The index just past a block comment starting at `start`. Postgres nests them,
/// so an inner `/*` must be matched before the outer `*/` closes.
fn block_comment_end(b: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = start;
    while i + 1 < b.len() {
        if b[i] == b'/' && b[i + 1] == b'*' {
            depth += 1;
            i += 2;
        } else if b[i] == b'*' && b[i + 1] == b'/' {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return Some(i);
            }
        } else {
            i += 1;
        }
    }
    None
}

/// The index of the first occurrence of `needle` in `b` at or after `from`.
fn find(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || b.len() < needle.len() {
        return None;
    }
    (from..=b.len() - needle.len()).find(|&i| &b[i..i + needle.len()] == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Postgres-shaped dialect: `$1`, `$2`, … — enough to assert placement.
    struct Pg;

    impl SqlDialect for Pg {
        fn quote_ident(&self, ident: &str) -> String {
            format!("\"{}\"", ident.replace('"', "\"\""))
        }
        fn placeholder(&self, position: usize) -> String {
            format!("${position}")
        }
    }

    fn rewrite(sql: &str) -> NamedSql {
        rewrite_named_params(&Pg, sql).expect("rewrites")
    }

    #[test]
    fn named_parameters_become_placeholders_in_order() {
        let got = rewrite("SELECT * FROM books WHERE author = :author AND year > :year");
        assert_eq!(
            got.sql,
            "SELECT * FROM books WHERE author = $1 AND year > $2"
        );
        assert_eq!(got.params, vec!["author", "year"]);
        assert_eq!(got.statements, 1);
    }

    #[test]
    fn a_repeated_parameter_takes_one_placeholder_and_one_bind() {
        // Two mentions of one argument are one value; numbering them separately
        // would ask the caller for it twice.
        let got = rewrite("SELECT * FROM t WHERE a = :id OR b = :id");
        assert_eq!(got.sql, "SELECT * FROM t WHERE a = $1 OR b = $1");
        assert_eq!(got.params, vec!["id"]);
    }

    #[test]
    fn a_cast_is_not_a_parameter_called_text() {
        // The failure this prevents: `x::text` read as a parameter named `text`,
        // which both loses the cast and invents an argument.
        let got = rewrite("SELECT id::text, n::numeric FROM t WHERE id = :id");
        assert_eq!(got.sql, "SELECT id::text, n::numeric FROM t WHERE id = $1");
        assert_eq!(got.params, vec!["id"]);
    }

    #[test]
    fn a_colon_inside_a_string_literal_is_text() {
        let got = rewrite("SELECT 'a:b', 'it''s :here' FROM t WHERE x = :x");
        assert_eq!(got.sql, "SELECT 'a:b', 'it''s :here' FROM t WHERE x = $1");
        assert_eq!(got.params, vec!["x"]);
    }

    #[test]
    fn a_colon_inside_a_dollar_quoted_body_is_text() {
        let got = rewrite("SELECT $fn$ :nope ; $fn$, $$ :also ; $$ WHERE x = :x");
        assert_eq!(
            got.sql,
            "SELECT $fn$ :nope ; $fn$, $$ :also ; $$ WHERE x = $1"
        );
        assert_eq!(got.params, vec!["x"]);
        // …and the `;`s inside them did not make this three statements.
        assert_eq!(got.statements, 1);
    }

    #[test]
    fn a_colon_inside_a_quoted_identifier_is_part_of_the_name() {
        let got = rewrite(r#"SELECT "odd:name" FROM t WHERE x = :x"#);
        assert_eq!(got.sql, r#"SELECT "odd:name" FROM t WHERE x = $1"#);
        assert_eq!(got.params, vec!["x"]);
    }

    #[test]
    fn a_colon_inside_a_comment_is_a_comment() {
        let got = rewrite(
            "-- :note, and a ; too\nSELECT /* :inner /* nested :deep */ back */ x \
             FROM t WHERE x = :x",
        );
        assert!(got.sql.contains("-- :note, and a ; too"), "{}", got.sql);
        assert!(
            got.sql.contains("/* :inner /* nested :deep */ back */"),
            "{}",
            got.sql
        );
        assert_eq!(got.params, vec!["x"]);
        assert_eq!(got.statements, 1);
    }

    #[test]
    fn statements_are_counted_outside_literals_and_comments() {
        assert_eq!(rewrite("").statements, 0);
        assert_eq!(rewrite("  -- nothing here\n ").statements, 0);
        // A trailing semicolon does not open a second statement.
        assert_eq!(rewrite("SELECT 1;").statements, 1);
        assert_eq!(rewrite("SELECT 1; \n ").statements, 1);
        assert_eq!(rewrite("SELECT 1; DROP TABLE books").statements, 2);
    }

    #[test]
    fn a_positional_placeholder_is_left_alone() {
        // `$1` is not a dollar-quote opener, so it survives verbatim.
        let got = rewrite("SELECT * FROM t WHERE a = $1");
        assert_eq!(got.sql, "SELECT * FROM t WHERE a = $1");
        assert!(got.params.is_empty());
    }

    #[test]
    fn an_unterminated_literal_is_refused_rather_than_guessed_at() {
        for sql in [
            "SELECT 'unclosed FROM t",
            "SELECT $tag$ unclosed FROM t",
            "SELECT /* unclosed FROM t",
            r#"SELECT "unclosed FROM t"#,
        ] {
            assert!(rewrite_named_params(&Pg, sql).is_err(), "{sql}");
        }
    }

    #[test]
    fn multibyte_text_survives_the_scan() {
        let got = rewrite("SELECT 'héllo — ✓' FROM t WHERE x = :x");
        assert_eq!(got.sql, "SELECT 'héllo — ✓' FROM t WHERE x = $1");
    }
}
