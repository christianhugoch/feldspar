//! The catalogue: one JSON object per locale per domain, keyed by English source
//! (decisions D1 and D4).
//!
//! ```json
//! {
//!   "Incorrect password": "Mot de passe incorrect",
//!   "Delete {name}?": "Supprimer {name} ?",
//!   "{count} rows": { "one": "{count} ligne", "other": "{count} lignes" },
//!   "verb\u0004Order": "Commander"
//! }
//! ```
//!
//! A value is a string, or an object keyed by CLDR plural category selected on
//! the argument named `count`. `\u{4}` separates a disambiguating context from
//! the source text (gettext's `msgctxt`, so the file stays flat and
//! hand-editable — see [`context_key`]). There is **no `en.json`** unless English
//! itself needs plural forms: the key *is* the English.
//!
//! Two types, because there are two questions:
//!
//! - [`Catalog`] is one locale's messages — what a file holds, what a
//!   `_fd_translations` row holds, what the LLM fills in.
//! - [`Catalogs`] is a **domain's** set of them, and is what a lookup goes
//!   through: it walks the requested locale's fallback chain (`pt-BR` → `pt`),
//!   so a partially translated regional catalogue reads its parent's answer
//!   rather than falling all the way back to English.

use std::collections::BTreeMap;

use sc_error::{Error, Result};
use serde_json::{Map, Value as Json};

use crate::format::{Args, format};
use crate::locale::Locale;
use crate::plural::{PluralCategory, category_for};

/// The character separating a disambiguating context from the source text in a
/// key — gettext's `msgctxt` separator, `\u{4}` (`EOT`).
///
/// A separator inside the key rather than a nested object keeps the file flat:
/// one object, one line per message, which is what a translator can open and
/// what an LLM reliably returns.
pub const CONTEXT_SEPARATOR: char = '\u{4}';

/// The key for a message disambiguated by a context: `tc!(loc, "verb", "Order")`
/// is filed under `verb\u{4}Order`.
pub fn context_key(context: &str, text: &str) -> String {
    format!("{context}{CONTEXT_SEPARATOR}{text}")
}

/// The source text a key carries, with any context stripped.
///
/// What renders when there is no translation (D1's whole point: a missing
/// translation is correct English, not a message id) and what a placeholder
/// check reads.
pub fn source_text(key: &str) -> &str {
    match key.split_once(CONTEXT_SEPARATOR) {
        Some((_, text)) => text,
        None => key,
    }
}

/// The context a key carries, if it has one.
pub fn key_context(key: &str) -> Option<&str> {
    key.split_once(CONTEXT_SEPARATOR)
        .map(|(context, _)| context)
}

/// One catalogue entry: a translation, or a translation per plural category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// One form, used whatever the count.
    Simple(String),
    /// One form per CLDR plural category, selected on `count`.
    Plural(BTreeMap<PluralCategory, String>),
}

impl Message {
    /// The form this message uses for `count` in `locale`.
    ///
    /// A [`Simple`](Message::Simple) message is used as written — *a locale with
    /// one form is a locale with one form*, and a translator who wrote one
    /// string for Japanese was right. A plural message with no variant for the
    /// selected category falls back to `other`, then to any variant it has:
    /// something in the reader's language beats English, and beats nothing.
    pub fn form(&self, locale: &Locale, count: Option<i64>) -> &str {
        match self {
            Message::Simple(text) => text,
            Message::Plural(forms) => {
                let selected = count.map(|n| category_for(locale, n));
                selected
                    .and_then(|c| forms.get(&c))
                    .or_else(|| forms.get(&PluralCategory::Other))
                    .or_else(|| forms.values().next())
                    .map(String::as_str)
                    // An empty plural object cannot be built by `from_json`
                    // (it is refused there), so this is unreachable rather than
                    // a case with a meaning.
                    .unwrap_or("")
            }
        }
    }

    /// Every form this message has, in category order — what a placeholder check
    /// iterates.
    pub fn forms(&self) -> Vec<&str> {
        match self {
            Message::Simple(text) => vec![text.as_str()],
            Message::Plural(forms) => forms.values().map(String::as_str).collect(),
        }
    }

    /// The plural categories this message supplies, or `None` if it is a plain
    /// string.
    pub fn categories(&self) -> Option<Vec<PluralCategory>> {
        match self {
            Message::Simple(_) => None,
            Message::Plural(forms) => Some(forms.keys().copied().collect()),
        }
    }

    /// This message as it is stored in a catalogue file.
    pub fn to_json(&self) -> Json {
        match self {
            Message::Simple(text) => Json::String(text.clone()),
            Message::Plural(forms) => Json::Object(
                forms
                    .iter()
                    .map(|(c, text)| (c.as_str().to_owned(), Json::String(text.clone())))
                    .collect(),
            ),
        }
    }

    /// Read one catalogue value, naming `key` in every refusal.
    ///
    /// The error names the key because that is the only thing that identifies
    /// the line in a flat JSON object, and a catalogue with one bad entry is a
    /// file somebody has to go and fix.
    pub fn from_json(key: &str, value: &Json) -> Result<Message> {
        match value {
            Json::String(text) => Ok(Message::Simple(text.clone())),
            Json::Object(map) => {
                let mut forms = BTreeMap::new();
                for (name, form) in map {
                    let category = PluralCategory::parse(name).ok_or_else(|| {
                        Error::invalid(format!(
                            "`{key}`: `{name}` is not a plural category (one of {})",
                            PluralCategory::ALL.map(PluralCategory::as_str).join(", ")
                        ))
                    })?;
                    let Json::String(text) = form else {
                        return Err(Error::invalid(format!(
                            "`{key}`: the `{name}` form should be a string, got {form}"
                        )));
                    };
                    forms.insert(category, text.clone());
                }
                if forms.is_empty() {
                    return Err(Error::invalid(format!(
                        "`{key}`: a plural entry needs at least one category"
                    )));
                }
                Ok(Message::Plural(forms))
            }
            other => Err(Error::invalid(format!(
                "`{key}`: a translation is a string or an object of plural forms, got {other}"
            ))),
        }
    }
}

/// One locale's messages: what a `locales/{locale}.json` file holds, and what a
/// `_fd_translations` row's `messages` column holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    locale: Locale,
    messages: BTreeMap<String, Message>,
}

impl Catalog {
    /// An empty catalogue for a locale — what a newly enabled locale starts as.
    pub fn new(locale: Locale) -> Catalog {
        Catalog {
            locale,
            messages: BTreeMap::new(),
        }
    }

    /// Parse a catalogue file.
    pub fn parse(locale: Locale, json: &str) -> Result<Catalog> {
        let value: Json = serde_json::from_str(json).map_err(|e| {
            Error::invalid(format!(
                "the `{}` catalogue is not valid JSON: {e}",
                locale.as_str()
            ))
        })?;
        Catalog::from_json(locale, &value)
    }

    /// Read a catalogue out of a parsed JSON object.
    pub fn from_json(locale: Locale, value: &Json) -> Result<Catalog> {
        let Json::Object(map) = value else {
            return Err(Error::invalid(format!(
                "the `{}` catalogue should be a JSON object of source text to \
                 translation, got {}",
                locale.as_str(),
                json_kind(value)
            )));
        };
        let mut messages = BTreeMap::new();
        for (key, entry) in map {
            messages.insert(key.clone(), Message::from_json(key, entry)?);
        }
        Ok(Catalog { locale, messages })
    }

    /// The catalogue as it is stored.
    pub fn to_json(&self) -> Json {
        let map: Map<String, Json> = self
            .messages
            .iter()
            .map(|(key, message)| (key.clone(), message.to_json()))
            .collect();
        Json::Object(map)
    }

    /// The locale this catalogue is for.
    pub fn locale(&self) -> &Locale {
        &self.locale
    }

    /// Every message, ordered by key.
    pub fn messages(&self) -> &BTreeMap<String, Message> {
        &self.messages
    }

    /// The translation of `key`, if this catalogue has one.
    pub fn get(&self, key: &str) -> Option<&Message> {
        self.messages.get(key)
    }

    /// How many messages this catalogue holds.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Whether this catalogue holds nothing.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Record a translation, replacing any previous one.
    pub fn insert(&mut self, key: impl Into<String>, message: Message) {
        self.messages.insert(key.into(), message);
    }

    /// Forget a translation — used when a key's English changed and the admin
    /// clears the orphan, never automatically (D1: orphans are shown, not
    /// deleted).
    pub fn remove(&mut self, key: &str) -> Option<Message> {
        self.messages.remove(key)
    }

    /// The keys of `wanted` this catalogue has no translation for, in order.
    pub fn missing<'a>(&self, wanted: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        wanted
            .into_iter()
            .filter(|key| !self.messages.contains_key(*key))
            .map(str::to_owned)
            .collect()
    }

    /// The keys this catalogue holds that `wanted` does not use any more — the
    /// **orphans** of D1, shown on the Translations screen and never deleted by
    /// the machine.
    pub fn orphans<'a>(&self, wanted: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        let wanted: std::collections::BTreeSet<&str> = wanted.into_iter().collect();
        self.messages
            .keys()
            .filter(|key| !wanted.contains(key.as_str()))
            .cloned()
            .collect()
    }

    /// Render `key` from this catalogue, falling back to the key's own English.
    pub fn render(&self, key: &str, args: &Args<'_>) -> String {
        match self.get(key) {
            Some(message) => render_message(message, &self.locale, args),
            None => format(source_text(key), args),
        }
    }
}

/// A **domain's** catalogues — `core`, `admin`, `builder`, or one application's —
/// keyed by locale tag.
///
/// This is what a lookup goes through, because a lookup is not "the fr
/// catalogue's answer": it is the fallback chain's answer. A `pt-BR` request
/// against a domain holding `pt-BR` and `pt` reads the regional file first and
/// the language file for whatever the regional one has not got.
#[derive(Debug, Clone, Default)]
pub struct Catalogs {
    by_tag: BTreeMap<String, Catalog>,
}

impl Catalogs {
    /// A domain with no catalogues: the state of every application that has not
    /// turned i18n on, and the state of this server before a locale is enabled.
    pub fn new() -> Catalogs {
        Catalogs::default()
    }

    /// Add or replace a locale's catalogue.
    pub fn insert(&mut self, catalog: Catalog) {
        self.by_tag
            .insert(catalog.locale().as_str().to_owned(), catalog);
    }

    /// The catalogue filed exactly under this locale, if any.
    pub fn exact(&self, locale: &Locale) -> Option<&Catalog> {
        self.by_tag.get(locale.as_str())
    }

    /// Every locale this domain has a catalogue for.
    pub fn locales(&self) -> Vec<&Locale> {
        self.by_tag.values().map(Catalog::locale).collect()
    }

    /// Whether this domain has no catalogues at all — the zero-cost case (D11),
    /// checked first on every lookup.
    pub fn is_empty(&self) -> bool {
        self.by_tag.is_empty()
    }

    /// Look `key` up along `locale`'s fallback chain.
    pub fn get(&self, locale: &Locale, key: &str) -> Option<(&Catalog, &Message)> {
        if self.by_tag.is_empty() {
            return None;
        }
        for step in locale.fallback_chain() {
            if let Some(catalog) = self.by_tag.get(step.as_str())
                && let Some(message) = catalog.get(key)
            {
                return Some((catalog, message));
            }
        }
        None
    }

    /// Translate and format `key` for `locale`.
    ///
    /// The whole of what `t!` does, and the whole of D11: a domain with no
    /// catalogues formats the key's own English and returns, having allocated
    /// once and hashed nothing.
    pub fn translate(&self, locale: &Locale, key: &str, args: &Args<'_>) -> String {
        match self.get(locale, key) {
            Some((catalog, message)) => render_message(message, catalog.locale(), args),
            None => format(source_text(key), args),
        }
    }
}

/// Render a looked-up message: select the plural form against the locale the
/// *catalogue* is in, then substitute.
///
/// The selecting locale is the catalogue's, not the request's: a `pt-BR` request
/// answered out of the `pt` catalogue has Portuguese forms in hand, and asking
/// `pt-BR`'s rules about them would be asking the wrong question only when the
/// two differ — but they are the same rules, and being explicit about which one
/// is used costs nothing.
fn render_message(message: &Message, locale: &Locale, args: &Args<'_>) -> String {
    let count = args
        .iter()
        .find(|(name, _)| *name == "count")
        .and_then(|(_, value)| value.plural_count());
    format(message.form(locale, count), args)
}

fn json_kind(value: &Json) -> &'static str {
    match value {
        Json::Null => "null",
        Json::Bool(_) => "a boolean",
        Json::Number(_) => "a number",
        Json::String(_) => "a string",
        Json::Array(_) => "an array",
        Json::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::format::Arg;

    fn loc(tag: &str) -> Locale {
        Locale::parse(tag).unwrap()
    }

    fn french() -> Catalog {
        Catalog::parse(
            loc("fr"),
            r#"{
                "Incorrect password": "Mot de passe incorrect",
                "Delete {name}?": "Supprimer {name} ?",
                "{count} rows": { "one": "{count} ligne", "other": "{count} lignes" },
                "verb\u0004Order": "Commander"
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn a_catalogue_parses_strings_plurals_and_contexts() {
        let catalog = french();
        assert_eq!(catalog.len(), 4);
        assert_eq!(
            catalog.render("Incorrect password", &[]),
            "Mot de passe incorrect"
        );
        assert_eq!(
            catalog.render("Delete {name}?", &[("name", Arg::from("Tâches"))]),
            "Supprimer Tâches ?"
        );
        assert_eq!(
            catalog.render(&context_key("verb", "Order"), &[]),
            "Commander"
        );
    }

    #[test]
    fn plurals_are_selected_by_the_locales_rules() {
        let catalog = french();
        // French puts 0 in `one`, which is the reason this is not `count == 1`.
        assert_eq!(
            catalog.render("{count} rows", &[("count", Arg::from(0))]),
            "0 ligne"
        );
        assert_eq!(
            catalog.render("{count} rows", &[("count", Arg::from(1))]),
            "1 ligne"
        );
        assert_eq!(
            catalog.render("{count} rows", &[("count", Arg::from(4))]),
            "4 lignes"
        );
    }

    #[test]
    fn a_missing_key_renders_its_own_english() {
        let catalog = french();
        assert_eq!(
            catalog.render("Not translated yet", &[]),
            "Not translated yet"
        );
        // Including the context form: the reader gets the word, not the key.
        assert_eq!(catalog.render(&context_key("noun", "Order"), &[]), "Order");
    }

    #[test]
    fn a_plain_string_where_plurals_were_expected_is_used_as_written() {
        let catalog = Catalog::parse(loc("ja"), r#"{ "{count} rows": "{count} 行" }"#).unwrap();
        assert_eq!(
            catalog.render("{count} rows", &[("count", Arg::from(5))]),
            "5 行"
        );
    }

    #[test]
    fn a_bad_value_is_an_error_naming_the_key() {
        let err = Catalog::parse(loc("fr"), r#"{ "Hello": 7 }"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("Hello"), "{err}");

        let err = Catalog::parse(loc("fr"), r#"{ "{count} rows": { "plural": "x" } }"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("{count} rows"), "{err}");
        assert!(err.contains("plural"), "{err}");

        let err = Catalog::parse(loc("fr"), r#"{ "{count} rows": {} }"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("at least one category"), "{err}");

        assert!(Catalog::parse(loc("fr"), "[]").is_err());
        assert!(Catalog::parse(loc("fr"), "not json").is_err());
    }

    #[test]
    fn a_catalogue_round_trips_through_json() {
        let catalog = french();
        let round = Catalog::from_json(loc("fr"), &catalog.to_json()).unwrap();
        assert_eq!(round, catalog);
        // And the stored shape is the one the proposal prints.
        assert_eq!(
            catalog.to_json()["{count} rows"],
            json!({ "one": "{count} ligne", "other": "{count} lignes" })
        );
    }

    #[test]
    fn a_domain_walks_the_fallback_chain() {
        let mut domain = Catalogs::new();
        domain.insert(
            Catalog::parse(loc("pt"), r#"{ "Save": "Guardar", "Delete": "Eliminar" }"#).unwrap(),
        );
        domain.insert(Catalog::parse(loc("pt-BR"), r#"{ "Delete": "Excluir" }"#).unwrap());

        let br = loc("pt-BR");
        // The regional catalogue wins where it has an answer…
        assert_eq!(domain.translate(&br, "Delete", &[]), "Excluir");
        // …and the language catalogue answers where it does not.
        assert_eq!(domain.translate(&br, "Save", &[]), "Guardar");
        // A locale with no catalogue anywhere in its chain gets its English.
        assert_eq!(domain.translate(&loc("de"), "Save", &[]), "Save");
    }

    #[test]
    fn an_empty_domain_costs_a_format() {
        let domain = Catalogs::new();
        assert!(domain.is_empty());
        assert_eq!(
            domain.translate(
                &loc("fr"),
                "Delete {name}?",
                &[("name", Arg::from("Tasks"))]
            ),
            "Delete Tasks?"
        );
        assert!(domain.get(&loc("fr"), "anything").is_none());
    }

    #[test]
    fn missing_and_orphans_are_two_sides_of_the_same_comparison() {
        let catalog = french();
        let used = ["Incorrect password", "Something new"];
        assert_eq!(catalog.missing(used), ["Something new"]);
        let orphans = catalog.orphans(used);
        assert!(orphans.contains(&"Delete {name}?".to_owned()));
        assert!(!orphans.contains(&"Incorrect password".to_owned()));
    }

    #[test]
    fn a_context_key_carries_its_two_halves() {
        let key = context_key("verb", "Order");
        assert_eq!(source_text(&key), "Order");
        assert_eq!(key_context(&key), Some("verb"));
        assert_eq!(key_context("Order"), None);
        assert_eq!(source_text("Order"), "Order");
    }
}
