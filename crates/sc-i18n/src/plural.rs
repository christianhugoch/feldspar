//! CLDR plural categories (decision D3).
//!
//! A catalogue entry is a string, or an object keyed by plural category:
//!
//! ```json
//! { "{count} rows": { "one": "{count} ligne", "other": "{count} lignes" } }
//! ```
//!
//! Which key is used is decided by the argument named `count`, by the same CLDR
//! data the browser's `Intl.PluralRules` uses — `icu_plurals` with
//! `compiled_data`. The two halves of this facility therefore agree because they
//! are reading the same table, not because somebody kept two tables in step.
//!
//! Deliberately *not* ICU's inline `{n, plural, …}`: inline plurals need a
//! parser on both sides and hand a translator a syntax to get wrong inside a
//! sentence they are already getting right.

use std::fmt;

use icu_plurals::{PluralCategory as IcuCategory, PluralRules};

use crate::Locale;

/// A CLDR plural category.
///
/// Re-declared here rather than re-exported from `icu_plurals` so the catalogue
/// format has a type of its own: these six spellings are what appears in a JSON
/// file an admin or an LLM writes, and they should not change because a
/// dependency's enum did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PluralCategory {
    /// CLDR `zero` — Arabic, Latvian.
    Zero,
    /// CLDR `one` — the singular in most languages.
    One,
    /// CLDR `two` — Arabic, Hebrew, Slovenian.
    Two,
    /// CLDR `few` — Polish, Russian, Romanian.
    Few,
    /// CLDR `many` — Polish, Russian, Ukrainian.
    Many,
    /// CLDR `other` — the catch-all every locale has.
    Other,
}

impl PluralCategory {
    /// Every category, in CLDR's order.
    pub const ALL: [PluralCategory; 6] = [
        PluralCategory::Zero,
        PluralCategory::One,
        PluralCategory::Two,
        PluralCategory::Few,
        PluralCategory::Many,
        PluralCategory::Other,
    ];

    /// The spelling used as a JSON key.
    pub fn as_str(self) -> &'static str {
        match self {
            PluralCategory::Zero => "zero",
            PluralCategory::One => "one",
            PluralCategory::Two => "two",
            PluralCategory::Few => "few",
            PluralCategory::Many => "many",
            PluralCategory::Other => "other",
        }
    }

    /// The category a JSON key names, or `None` when it names none — which is
    /// what makes a catalogue entry with a typo in it an error rather than a
    /// variant nothing ever selects.
    pub fn parse(key: &str) -> Option<PluralCategory> {
        match key {
            "zero" => Some(PluralCategory::Zero),
            "one" => Some(PluralCategory::One),
            "two" => Some(PluralCategory::Two),
            "few" => Some(PluralCategory::Few),
            "many" => Some(PluralCategory::Many),
            "other" => Some(PluralCategory::Other),
            _ => None,
        }
    }

    fn from_icu(category: IcuCategory) -> PluralCategory {
        match category {
            IcuCategory::Zero => PluralCategory::Zero,
            IcuCategory::One => PluralCategory::One,
            IcuCategory::Two => PluralCategory::Two,
            IcuCategory::Few => PluralCategory::Few,
            IcuCategory::Many => PluralCategory::Many,
            IcuCategory::Other => PluralCategory::Other,
        }
    }
}

impl fmt::Display for PluralCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The category `count` falls into in `locale`.
///
/// Falls back to [`Other`](PluralCategory::Other) when CLDR has no data for the
/// locale — which is also the one category every locale has, so a message with
/// an `other` variant renders in every language whether or not ICU has heard of
/// it.
pub fn category_for(locale: &Locale, count: i64) -> PluralCategory {
    match PluralRules::try_new((&locale.icu()).into(), Default::default()) {
        Ok(rules) => PluralCategory::from_icu(rules.category_for(count)),
        Err(_) => PluralCategory::Other,
    }
}

/// The categories a locale actually uses — English has `one` and `other`, Arabic
/// has all six, Japanese has only `other`.
///
/// This is what a translation is checked against (D9): a French translation of a
/// plural message that supplies only `other` has lost a form, and a translation
/// that invents `many` for English has gained one nothing will ever select.
pub fn categories_for(locale: &Locale) -> Vec<PluralCategory> {
    let Ok(rules) = PluralRules::try_new((&locale.icu()).into(), Default::default()) else {
        return vec![PluralCategory::Other];
    };
    let used: Vec<PluralCategory> = rules.categories().map(PluralCategory::from_icu).collect();
    match used.is_empty() {
        true => vec![PluralCategory::Other],
        false => used,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(tag: &str) -> Locale {
        Locale::parse(tag).unwrap()
    }

    #[test]
    fn english_has_one_and_other() {
        assert_eq!(category_for(&loc("en"), 1), PluralCategory::One);
        assert_eq!(category_for(&loc("en"), 0), PluralCategory::Other);
        assert_eq!(category_for(&loc("en"), 7), PluralCategory::Other);
        let mut cats = categories_for(&loc("en"));
        cats.sort();
        assert_eq!(cats, [PluralCategory::One, PluralCategory::Other]);
    }

    #[test]
    fn french_counts_zero_as_singular_and_russian_has_few() {
        // The textbook difference between en and fr, and the reason the count is
        // not compared against 1 anywhere in this crate.
        assert_eq!(category_for(&loc("fr"), 0), PluralCategory::One);
        assert_eq!(category_for(&loc("ru"), 3), PluralCategory::Few);
        assert_eq!(category_for(&loc("ru"), 5), PluralCategory::Many);
    }

    #[test]
    fn a_locale_with_one_form_has_one_category() {
        assert_eq!(categories_for(&loc("ja")), [PluralCategory::Other]);
    }

    #[test]
    fn category_keys_round_trip() {
        for category in PluralCategory::ALL {
            assert_eq!(PluralCategory::parse(category.as_str()), Some(category));
        }
        assert_eq!(PluralCategory::parse("plural"), None);
    }
}
