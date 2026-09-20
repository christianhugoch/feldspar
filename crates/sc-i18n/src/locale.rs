//! The [`Locale`]: a BCP-47 language tag, its fallback chain and its direction.
//!
//! A language tag is a grammar, not a string to split on hyphens — `zh-Hans-CN`,
//! `pt-BR` and `en-US-u-ca-gregory` all mean something specific and none of them
//! is three fields — so this is a thin, opinionated wrapper over
//! [`icu_locale_core::Locale`] rather than a `String` with conventions.
//!
//! Two opinions are baked in:
//!
//! - **Extensions are dropped on parse.** `-u-ca-gregory` chooses a calendar; it
//!   does not choose a catalogue. Keeping it would make `fr` and
//!   `fr-u-ca-gregory` two different keys into the same JSON file, which is a
//!   bug waiting for its first user.
//! - **The canonical string is the key.** [`Locale::as_str`] is what names a
//!   catalogue file (`fr.json`, `zh-Hans.json`), what goes in `Content-Language`,
//!   and what an admin types into `enabled_locales`; parsing normalises case and
//!   subtag order so `PT-br` and `pt-BR` cannot become two locales.

use std::fmt;
use std::str::FromStr;

use icu_locale_core::subtags::{Language, Script};
use icu_locale_core::{LanguageIdentifier, Locale as IcuLocale, langid};
use sc_error::{Error, Result};

/// The writing direction a locale's script is laid out in.
///
/// Answered for the one consumer there is: `<html dir>` and the choice between
/// Bootstrap's two stylesheets (proposal §5). Not a layout audit — see the
/// milestone's out-of-scope list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Left to right: the default, and what an unknown script is assumed to be.
    LeftToRight,
    /// Right to left: Arabic, Hebrew, Thaana, Syriac, N'Ko, Adlam and the
    /// languages written in them.
    RightToLeft,
}

impl Direction {
    /// The value for an HTML `dir` attribute: `"ltr"` or `"rtl"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::LeftToRight => "ltr",
            Direction::RightToLeft => "rtl",
        }
    }
}

/// Scripts written right to left, by their BCP-47 subtag.
///
/// A table rather than `icu_locale::LocaleDirectionality`, which would pull the
/// whole likely-subtags data set into a layer-0 crate to answer a question with
/// six entries. The cost of a table is that a script added to Unicode is not in
/// it until somebody adds it; the cost of the data set is every build paying for
/// it. `dir` is a hint on a `<html>` element, so the table wins.
const RTL_SCRIPTS: [&str; 6] = ["Arab", "Hebr", "Thaa", "Syrc", "Nkoo", "Adlm"];

/// Languages written right to left when no script subtag says otherwise.
///
/// The primary subtags of the RTL languages anybody enables. `ku` and `pa` are
/// deliberately absent: both are written in an RTL script in one country and an
/// LTR script in another, so they are exactly the tags that must carry an
/// explicit script subtag (`ku-Arab`) to mean it, and [`RTL_SCRIPTS`] answers
/// those.
const RTL_LANGUAGES: [&str; 10] = ["ar", "he", "fa", "ur", "ps", "sd", "yi", "dv", "ckb", "ug"];

/// A language tag: what a request is served in, what a catalogue is filed under.
///
/// Cheap to clone (three small subtags), ordered and hashable, so it is fine as a
/// map key and fine to pass by value.
///
/// Equality, ordering and hashing are all **the canonical tag's**, and the
/// parsed identifier rides along as a derived view of it: parsing normalises, so
/// two `Locale`s with the same tag are the same locale by construction, and a
/// second field participating in `Eq` could only ever disagree with the first.
#[derive(Debug, Clone)]
pub struct Locale {
    /// The canonical tag (`"fr"`, `"pt-BR"`, `"zh-Hans"`), cached so `as_str` is
    /// a borrow rather than a render: it is read on every catalogue lookup and
    /// written into every response header.
    tag: String,
    id: LanguageIdentifier,
}

impl Locale {
    /// Parse a BCP-47 language tag.
    ///
    /// Extensions are dropped (see the module docs); case and subtag order are
    /// normalised. An empty or malformed tag is [`Error::invalid`] naming what
    /// was given — `enabled_locales` is a setting an admin types, so the message
    /// has to be readable by the person who typed it.
    pub fn parse(tag: &str) -> Result<Locale> {
        let trimmed = tag.trim();
        if trimmed.is_empty() {
            return Err(Error::invalid("a locale tag must not be empty"));
        }
        let parsed = IcuLocale::from_str(trimmed)
            .map_err(|e| Error::invalid(format!("`{trimmed}` is not a language tag: {e}")))?;
        if parsed.id.language == Language::UNKNOWN {
            return Err(Error::invalid(format!(
                "`{trimmed}` names no language (`und` is not a locale anything can be served in)"
            )));
        }
        Ok(Locale::from_id(parsed.id))
    }

    /// The locale this tree's source text is written in: English.
    ///
    /// Not a magic string scattered about — D1 makes the English *source* the
    /// message id, so "is this locale the one the keys are already in?" is a
    /// question several call sites ask, and it should have one answer.
    pub fn source() -> Locale {
        Locale::from_id(langid!("en"))
    }

    fn from_id(id: LanguageIdentifier) -> Locale {
        // Variants ride along (`ca-valencia` is a real catalogue); the tag is
        // what the canonical writer produces, not what was typed.
        let tag = id.to_string();
        Locale { tag, id }
    }

    /// The canonical tag: the catalogue's filename, the `Content-Language` value,
    /// the `<html lang>` attribute.
    pub fn as_str(&self) -> &str {
        &self.tag
    }

    /// The primary language subtag (`"pt"` for `pt-BR`).
    pub fn language(&self) -> &str {
        self.id.language.as_str()
    }

    /// The script subtag, if the tag carries one (`"Hans"` for `zh-Hans`).
    pub fn script(&self) -> Option<&str> {
        self.id.script.as_ref().map(Script::as_str)
    }

    /// The region subtag, if the tag carries one (`"BR"` for `pt-BR`).
    pub fn region(&self) -> Option<String> {
        self.id.region.map(|r| r.to_string())
    }

    /// The **fallback chain**: this locale, then each less specific one, ending
    /// at the bare language.
    ///
    /// `pt-BR` → `[pt-BR, pt]`; `zh-Hans-CN` → `[zh-Hans-CN, zh-Hans, zh]`; `fr`
    /// → `[fr]`. The server's `default_locale` is *not* in it — that is the last
    /// resort a negotiation falls back to, not a parent of this tag, and putting
    /// it here would make a French catalogue silently answer a Portuguese
    /// request.
    ///
    /// Region before script, because a region is the more specific of the two: a
    /// `zh-Hans-CN` catalogue that does not have a key should try `zh-Hans`
    /// before `zh`.
    pub fn fallback_chain(&self) -> Vec<Locale> {
        let mut chain = vec![self.clone()];
        let mut id = self.id.clone();
        if !id.variants.is_empty() {
            id.variants.clear();
            chain.push(Locale::from_id(id.clone()));
        }
        if id.region.is_some() {
            id.region = None;
            chain.push(Locale::from_id(id.clone()));
        }
        if id.script.is_some() {
            id.script = None;
            chain.push(Locale::from_id(id));
        }
        chain
    }

    /// Which way this locale's script runs.
    pub fn direction(&self) -> Direction {
        let rtl = match self.script() {
            Some(script) => RTL_SCRIPTS.contains(&script),
            None => RTL_LANGUAGES.contains(&self.language()),
        };
        match rtl {
            true => Direction::RightToLeft,
            false => Direction::LeftToRight,
        }
    }

    /// The ICU value, for the one thing that needs it: plural rules.
    pub(crate) fn icu(&self) -> IcuLocale {
        IcuLocale::from(self.id.clone())
    }
}

impl PartialEq for Locale {
    fn eq(&self, other: &Locale) -> bool {
        self.tag == other.tag
    }
}

impl Eq for Locale {}

impl PartialOrd for Locale {
    fn partial_cmp(&self, other: &Locale) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Locale {
    fn cmp(&self, other: &Locale) -> std::cmp::Ordering {
        self.tag.cmp(&other.tag)
    }
}

impl std::hash::Hash for Locale {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.tag.hash(state);
    }
}

impl fmt::Display for Locale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.tag)
    }
}

impl FromStr for Locale {
    type Err = Error;

    fn from_str(s: &str) -> Result<Locale> {
        Locale::parse(s)
    }
}

/// Parse a comma- (or whitespace-) separated list of locale tags.
///
/// The shape `enabled_locales` is stored in, for the reason `ssl_extra_domains`
/// is stored that way: a settings form has a text box, and a list an admin can
/// read back is worth more than a JSON array they have to get the brackets right
/// in. Every tag is parsed, so one typo is one error naming the typo rather than
/// a locale that silently never matches.
pub fn parse_locale_list(raw: &str) -> Result<Vec<Locale>> {
    let mut out: Vec<Locale> = Vec::new();
    for piece in raw.split([',', ' ', '\t', '\n', ';']) {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        let locale = Locale::parse(piece)?;
        if !out.contains(&locale) {
            out.push(locale);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing_normalises_and_drops_extensions() {
        assert_eq!(Locale::parse("PT-br").unwrap().as_str(), "pt-BR");
        assert_eq!(Locale::parse("  fr  ").unwrap().as_str(), "fr");
        assert_eq!(Locale::parse("zh-hans").unwrap().as_str(), "zh-Hans");
        // An extension chooses a calendar, not a catalogue.
        assert_eq!(
            Locale::parse("en-US-u-ca-gregory").unwrap().as_str(),
            "en-US"
        );
    }

    #[test]
    fn a_tag_that_is_not_one_is_refused_by_name() {
        let err = Locale::parse("not a tag").unwrap_err().to_string();
        assert!(err.contains("not a tag"), "{err}");
        assert!(Locale::parse("").is_err());
        // `und` parses as a language identifier and is still not a locale.
        assert!(Locale::parse("und").is_err());
    }

    #[test]
    fn pt_br_falls_back_to_pt() {
        let chain: Vec<String> = Locale::parse("pt-BR")
            .unwrap()
            .fallback_chain()
            .iter()
            .map(|l| l.as_str().to_owned())
            .collect();
        assert_eq!(chain, ["pt-BR", "pt"]);
    }

    #[test]
    fn the_chain_strips_region_then_script() {
        let chain: Vec<String> = Locale::parse("zh-Hans-CN")
            .unwrap()
            .fallback_chain()
            .iter()
            .map(|l| l.as_str().to_owned())
            .collect();
        assert_eq!(chain, ["zh-Hans-CN", "zh-Hans", "zh"]);

        // A bare language is its own whole chain — and in particular the chain
        // never reaches for the server default.
        let chain = Locale::parse("fr").unwrap().fallback_chain();
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].as_str(), "fr");
    }

    #[test]
    fn direction_is_by_script_then_by_language() {
        assert_eq!(
            Locale::parse("ar").unwrap().direction(),
            Direction::RightToLeft
        );
        assert_eq!(
            Locale::parse("ar-EG").unwrap().direction(),
            Direction::RightToLeft
        );
        assert_eq!(
            Locale::parse("he").unwrap().direction(),
            Direction::RightToLeft
        );
        assert_eq!(
            Locale::parse("fr").unwrap().direction(),
            Direction::LeftToRight
        );
        // An explicit script decides, whichever way the language usually goes.
        assert_eq!(
            Locale::parse("ku-Arab").unwrap().direction(),
            Direction::RightToLeft
        );
        assert_eq!(
            Locale::parse("ur-Latn").unwrap().direction(),
            Direction::LeftToRight
        );
        assert_eq!(Direction::RightToLeft.as_str(), "rtl");
    }

    #[test]
    fn a_locale_list_is_parsed_whole_or_not_at_all() {
        let list = parse_locale_list("en, fr, zh-Hans").unwrap();
        let tags: Vec<&str> = list.iter().map(Locale::as_str).collect();
        assert_eq!(tags, ["en", "fr", "zh-Hans"]);

        // Duplicates collapse; one bad tag names itself.
        assert_eq!(parse_locale_list("fr,fr").unwrap().len(), 1);
        let err = parse_locale_list("en, frr-oops-oops")
            .unwrap_err()
            .to_string();
        assert!(err.contains("frr-oops-oops"), "{err}");
    }

    #[test]
    fn the_source_locale_is_english() {
        assert_eq!(Locale::source().as_str(), "en");
    }
}
