//! `t!` and `tc!`: the two call sites a Rust message is written at.
//!
//! ```
//! # use sc_i18n::{Locale, t, tc};
//! # let loc = Locale::parse("fr").unwrap();
//! let a = t!(loc, "Incorrect password");
//! let b = t!(loc, "Delete {name}?", name = "Tasks");
//! let c = t!(loc, "{count} rows", count = 3);
//! let d = tc!(loc, "verb", "Order");
//! ```
//!
//! The locale is the **first argument and always explicit** (D8). The message is
//! a literal, because the literal *is* the key (D1) and because the extractor
//! that builds the shipped catalogue is a scanner over these call sites — a
//! `t!(loc, some_variable)` would be a string nothing can ever translate.

/// Translate a `core` message.
///
/// `t!(loc, "text")` or `t!(loc, "text {with} arguments", with = value)`. The
/// locale may be a [`Locale`](crate::Locale) or a reference to one.
#[macro_export]
macro_rules! t {
    ($loc:expr, $text:literal) => {
        $crate::core::translate($crate::locale_ref(&$loc), $text, &[])
    };
    ($loc:expr, $text:literal, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::core::translate(
            $crate::locale_ref(&$loc),
            $text,
            &[$((stringify!($name), $crate::Arg::from($value))),+],
        )
    };
}

/// Translate a `core` message that needs disambiguating from another with the
/// same English.
///
/// `tc!(loc, "verb", "Order")` is stored under `verb\u{4}Order` and renders
/// `Order` when there is no translation — the context is a note to the
/// translator, never something a reader sees.
#[macro_export]
macro_rules! tc {
    ($loc:expr, $context:literal, $text:literal) => {
        $crate::core::translate(
            $crate::locale_ref(&$loc),
            concat!($context, "\u{4}", $text),
            &[],
        )
    };
    ($loc:expr, $context:literal, $text:literal, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::core::translate(
            $crate::locale_ref(&$loc),
            concat!($context, "\u{4}", $text),
            &[$((stringify!($name), $crate::Arg::from($value))),+],
        )
    };
}

/// Accept either a [`Locale`](crate::Locale) or a `&Locale` at a `t!` call site.
///
/// Every one of the hundreds of call sites this milestone adds writes `t!(loc,
/// …)`, and whether `loc` is owned or borrowed at that point is an accident of
/// the surrounding function. Making the macro cope is one generic function; the
/// alternative is a `&` that is sometimes right and sometimes an error.
pub fn locale_ref<L: std::borrow::Borrow<crate::Locale>>(locale: &L) -> &crate::Locale {
    locale.borrow()
}

#[cfg(test)]
mod tests {
    use crate::Locale;

    #[test]
    fn a_message_with_no_catalogue_is_its_own_english() {
        let loc = Locale::parse("fr").unwrap();
        assert_eq!(t!(loc, "Save changes"), "Save changes");
        assert_eq!(t!(loc, "Delete {name}?", name = "Tasks"), "Delete Tasks?");
        assert_eq!(t!(loc, "{count} rows", count = 3), "3 rows");
        // The context never reaches the reader.
        assert_eq!(tc!(loc, "verb", "Order"), "Order");
    }

    #[test]
    fn the_locale_may_be_owned_or_borrowed() {
        let owned = Locale::parse("de").unwrap();
        let borrowed = &owned;
        assert_eq!(t!(owned, "Save"), "Save");
        assert_eq!(t!(borrowed, "Save"), "Save");
    }

    #[test]
    fn arguments_take_the_ordinary_rust_types() {
        let loc = Locale::source();
        let name = String::from("Tasks");
        assert_eq!(t!(loc, "Delete {name}?", name = &name), "Delete Tasks?");
        assert_eq!(
            t!(loc, "{count} of {total}", count = 2usize, total = 9i64),
            "2 of 9"
        );
    }
}
