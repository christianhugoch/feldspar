//! Save-time validation of views and pages (TODO "Saltcorn UI" §1, §11).
//!
//! The checks that need nothing but values are pure functions here, so they are
//! asserted without a database; the store resolves the application, the roles
//! and the registry and hands them in.

use sc_app::Application;
use sc_error::{Error, Result};

use crate::patterns::{PatternInfo, find_pattern};
use crate::view::View;

/// Characters a name may not contain, because the name is a path segment
/// (`/view/:name`, `/page/:name`) and each of these either ends a segment or
/// changes what the URL means even when escaped by a careless link.
const RESERVED: [char; 5] = ['/', '\\', '?', '#', '%'];

/// Refuse a view or page name that cannot be its own URL path segment.
///
/// **Spaces are allowed**, and deliberately: v1 names views `List Books` and
/// `Filter books`, and a restored backup is exactly the case this has to accept.
/// A space percent-encodes to one unambiguous segment; a `/` does not.
pub fn check_name(kind: &str, name: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(Error::invalid(format!("a {kind} needs a name")));
    }
    if name.trim() != name {
        return Err(Error::invalid(format!(
            "{kind} name `{name}` starts or ends with whitespace"
        )));
    }
    if name == "." || name == ".." {
        return Err(Error::invalid(format!(
            "`{name}` is not a {kind} name: it is a relative path segment"
        )));
    }
    if let Some(c) = name
        .chars()
        .find(|c| RESERVED.contains(c) || c.is_control())
    {
        return Err(Error::invalid(format!(
            "{kind} name `{}` contains {}, which cannot appear in the URL the {kind} is served at",
            name.escape_debug(),
            match c {
                c if c.is_control() => "a control character".to_owned(),
                c => format!("`{c}`"),
            }
        )));
    }
    Ok(())
}

/// Refuse a view whose pattern is not registered, or whose table is missing or
/// outside the application's subset.
pub(crate) fn check_view_shape(
    view: &View,
    app: &Application,
    patterns: &[PatternInfo],
) -> Result<()> {
    let Some(pattern) = find_pattern(patterns, &view.viewpattern) else {
        let names: Vec<&str> = patterns.iter().map(|p| p.name.as_str()).collect();
        return Err(Error::invalid(format!(
            "view `{}` uses the view pattern `{}`, which is not registered; \
             the registered patterns are {}",
            view.name,
            view.viewpattern,
            names.join(", ")
        )));
    };
    match (&view.table_name, pattern.tableless) {
        (None, false) => Err(Error::invalid(format!(
            "view `{}` has no table, and the `{}` pattern needs one",
            view.name, pattern.name
        ))),
        (Some(table), _) if !app.tables.iter().any(|t| t.0 == *table) => {
            Err(Error::invalid(format!(
                "view `{}` is over the table `{table}`, which is not in application `{}`'s \
                 table subset; add it to the application first",
                view.name, app.name
            )))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patterns::builtin_patterns;
    use sc_app::FrameworkRef;
    use sc_catalog::TableId;

    #[test]
    fn a_v1_name_with_spaces_is_a_name() {
        assert!(check_name("view", "List Books").is_ok());
        assert!(check_name("page", "BooksOverview").is_ok());
    }

    #[test]
    fn a_name_that_is_not_one_path_segment_is_refused_by_what_is_wrong() {
        let msg = |n: &str| check_name("view", n).unwrap_err().to_string();
        assert!(msg("").contains("needs a name"));
        assert!(msg(" books").contains("whitespace"));
        assert!(msg("..").contains("relative path segment"));
        assert!(msg("books/authors").contains("`/`"));
        assert!(msg("50%").contains("`%`"));
        assert!(msg("a\nb").contains("control character"));
    }

    #[test]
    fn a_tableless_view_of_a_pattern_that_needs_a_table_is_refused() {
        let app = Application::new("Books", "books", FrameworkRef::new("saltcorn-ui"))
            .with_table(TableId("books".to_owned()));
        let mut view = View::new(app.id, "List Books", "List", "books");
        assert!(check_view_shape(&view, &app, &builtin_patterns()).is_ok());
        view.table_name = None;
        let err = check_view_shape(&view, &app, &builtin_patterns()).unwrap_err();
        assert!(err.to_string().contains("needs one"), "{err}");
    }
}
