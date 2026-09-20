//! Which locale a request is served in (decision D8).
//!
//! In order: an explicit `?lang=`; the signed-in user's `language` column; the
//! `lang` cookie (how an anonymous visitor to an application chooses);
//! `Accept-Language`, matched against the enabled set with a real fallback chain
//! (`pt-BR` → `pt`); the application's default; the server's `default_locale`.
//! Computed **once**, in the router, and then carried as a value.
//!
//! **A locale is never ambient.** Not a thread-local and not a task-local: this
//! server does work for a person on a task no request owns — a trigger emailing
//! a customer — and an ambient locale is exactly the mechanism that would
//! silently send that mail in the admin's language. The recipient's own
//! `language` is the locale for that message, which is a bug class v1 had and
//! this does not.
//!
//! [`I18nSettings`] is the *installation's* answer to "which locales exist",
//! which is a legitimately process-wide fact and is cached as one
//! ([`active`]/[`set_active`]). The locale of a request is not, and there is no
//! function here that will tell you one without being given the request's
//! inputs.

use std::sync::{Arc, OnceLock, RwLock};

use crate::locale::Locale;

/// What locales this installation serves.
///
/// `default` is the last resort of every negotiation and the language the admin
/// UI is in when nothing else is known; `enabled` is the set a request may be
/// negotiated into. The default is always a member of the enabled set — see
/// [`I18nSettings::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct I18nSettings {
    default: Locale,
    enabled: Vec<Locale>,
}

impl Default for I18nSettings {
    /// English, and English only: what an installation that has never opened the
    /// Localisation section runs as, and the state D11 says must cost nothing.
    fn default() -> I18nSettings {
        I18nSettings::new(Locale::source(), Vec::new())
    }
}

impl I18nSettings {
    /// The settings for a default locale and an enabled set.
    ///
    /// The default is prepended to the enabled set if it is not already in it: a
    /// configuration whose fallback is not a locale it serves has no sound
    /// answer for a request it cannot match, and refusing to boot over it would
    /// be a settings screen that locks an admin out of the settings screen.
    pub fn new(default: Locale, enabled: Vec<Locale>) -> I18nSettings {
        let mut enabled = enabled;
        if !enabled.contains(&default) {
            enabled.insert(0, default.clone());
        }
        I18nSettings { default, enabled }
    }

    /// The locale everything falls back to.
    pub fn default_locale(&self) -> &Locale {
        &self.default
    }

    /// The locales a request may be negotiated into, the default first.
    pub fn enabled(&self) -> &[Locale] {
        &self.enabled
    }

    /// Whether there is anything to negotiate.
    ///
    /// The D11 switch, and the reason it is `pub`: with one enabled locale the
    /// router does not parse `Accept-Language`, does not read the cookie and
    /// does not set `Content-Language` — a monolingual server does no work for a
    /// facility it is not using, and that is asserted rather than hoped.
    pub fn is_multilingual(&self) -> bool {
        self.enabled.len() > 1
    }

    /// Whether `locale` is one this installation serves.
    pub fn is_enabled(&self, locale: &Locale) -> bool {
        self.enabled.contains(locale)
    }

    /// The best enabled locale for one candidate tag, or `None` if the candidate
    /// reaches nothing enabled.
    ///
    /// Matching is by the candidate's fallback chain (`pt-BR` matches an enabled
    /// `pt`), and then — only then — by a *more specific* enabled locale of the
    /// same language (`pt` matches an enabled `pt-BR`, because serving Brazilian
    /// Portuguese to somebody who asked for Portuguese beats serving English).
    /// An unknown tag matches nothing and therefore never escapes the enabled
    /// set.
    pub fn best_match(&self, candidate: &Locale) -> Option<Locale> {
        for step in candidate.fallback_chain() {
            if let Some(found) = self.enabled.iter().find(|l| **l == step) {
                return Some(found.clone());
            }
        }
        self.enabled
            .iter()
            .find(|l| l.language() == candidate.language())
            .cloned()
    }

    /// Negotiate an `Accept-Language` header against the enabled set.
    ///
    /// Quality values are honoured, `q=0` is a refusal rather than a preference,
    /// and `*` means the default. A header that matches nothing — or that does
    /// not parse — yields the default, because the **default is the last resort,
    /// not an error**: a browser sending a tag nobody has heard of still gets a
    /// page.
    pub fn negotiate(&self, accept_language: &str) -> Locale {
        if !self.is_multilingual() {
            return self.default.clone();
        }
        for tag in accept_language_order(accept_language) {
            if tag == "*" {
                return self.default.clone();
            }
            if let Ok(candidate) = Locale::parse(&tag)
                && let Some(found) = self.best_match(&candidate)
            {
                return found;
            }
        }
        self.default.clone()
    }

    /// Resolve the locale of one request from every source, in D8's order.
    ///
    /// The single place that order is written down. Each source is checked
    /// against the enabled set: a `?lang=` naming a locale this installation does
    /// not serve falls through to the next source rather than being honoured,
    /// which is what keeps a query parameter from being a way to ask for a
    /// catalogue that does not exist.
    pub fn resolve(&self, sources: &RequestLocale<'_>) -> Locale {
        // D11: nothing to negotiate, nothing to parse. Deliberately before every
        // source, including the explicit `?lang=` — on a monolingual server
        // there is no second locale for it to name.
        if !self.is_multilingual() {
            return self.default.clone();
        }
        for explicit in [sources.query, sources.user, sources.cookie] {
            if let Some(tag) = explicit
                && let Ok(candidate) = Locale::parse(tag)
                && let Some(found) = self.best_match(&candidate)
            {
                return found;
            }
        }
        if let Some(header) = sources.accept_language {
            return self.negotiate(header);
        }
        self.default.clone()
    }
}

/// The four places a request's locale can come from, in D8's order.
///
/// A struct of borrowed strings rather than four arguments, so a caller that
/// forgets one gets a field it did not fill rather than two `Option<&str>`s in
/// the wrong order.
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestLocale<'a> {
    /// An explicit `?lang=` — the loudest signal there is, and the one a link
    /// can carry.
    pub query: Option<&'a str>,
    /// The signed-in user's `language` column.
    pub user: Option<&'a str>,
    /// The `lang` cookie: how an anonymous visitor to an application chooses.
    pub cookie: Option<&'a str>,
    /// The `Accept-Language` header.
    pub accept_language: Option<&'a str>,
}

/// The tags of an `Accept-Language` header, best first.
///
/// Sorted by quality value descending, stably — two tags at the same `q` keep
/// the order the browser wrote them in, which is the order it meant. `q=0` is
/// dropped: RFC 9110 says it means "not acceptable", and treating it as a weak
/// preference is how a user who explicitly refused a language gets served it.
fn accept_language_order(header: &str) -> Vec<String> {
    let mut entries: Vec<(f32, usize, String)> = Vec::new();
    for (index, part) in header.split(',').enumerate() {
        let mut pieces = part.split(';');
        let Some(tag) = pieces.next().map(str::trim) else {
            continue;
        };
        if tag.is_empty() {
            continue;
        }
        let mut quality = 1.0f32;
        for param in pieces {
            let param = param.trim();
            if let Some(value) = param.strip_prefix("q=") {
                // A `q` that is not a number is a malformed header, and the
                // charitable reading of a malformed header is that the tag was
                // still meant — so the tag keeps its default weight.
                quality = value.trim().parse::<f32>().unwrap_or(1.0);
            }
        }
        if quality <= 0.0 {
            continue;
        }
        entries.push((quality, index, tag.to_owned()));
    }
    entries.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });
    entries.into_iter().map(|(_, _, tag)| tag).collect()
}

/// The installation's settings, as this process currently has them.
static ACTIVE: OnceLock<RwLock<Arc<I18nSettings>>> = OnceLock::new();

fn slot() -> &'static RwLock<Arc<I18nSettings>> {
    ACTIVE.get_or_init(|| RwLock::new(Arc::new(I18nSettings::default())))
}

/// Make `settings` the ones this process negotiates against.
///
/// Called at boot and again when the Localisation section is saved — enabling a
/// locale takes effect on the next request rather than at the next restart, for
/// the reason the two logging switches do: the moment you turn it on is the
/// moment you want to go and look.
pub fn set_active(settings: I18nSettings) {
    if let Ok(mut guard) = slot().write() {
        *guard = Arc::new(settings);
    }
}

/// The installation's current settings.
///
/// An `Arc` clone, so a request negotiating against them does not hold a lock
/// while it serves, and a save during that request does not change the answer
/// halfway through.
pub fn active() -> Arc<I18nSettings> {
    match slot().read() {
        Ok(guard) => guard.clone(),
        // A poisoned lock means a writer panicked mid-swap. The honest answer is
        // still a usable set of settings rather than a panic in every handler.
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(tag: &str) -> Locale {
        Locale::parse(tag).unwrap()
    }

    fn settings(default: &str, enabled: &[&str]) -> I18nSettings {
        I18nSettings::new(loc(default), enabled.iter().map(|t| loc(t)).collect())
    }

    #[test]
    fn pt_br_falls_back_to_pt() {
        let s = settings("en", &["en", "pt", "fr"]);
        assert_eq!(s.negotiate("pt-BR").as_str(), "pt");
        // And an enabled regional catalogue is preferred when there is one.
        let s = settings("en", &["en", "pt-BR", "pt"]);
        assert_eq!(s.negotiate("pt-BR").as_str(), "pt-BR");
    }

    #[test]
    fn a_request_for_a_language_reaches_its_one_region() {
        // Asked for `pt`, and the only Portuguese served is Brazilian: that beats
        // English.
        let s = settings("en", &["en", "pt-BR"]);
        assert_eq!(s.negotiate("pt").as_str(), "pt-BR");
    }

    #[test]
    fn an_unknown_tag_never_escapes_the_enabled_set() {
        let s = settings("en", &["en", "fr"]);
        assert_eq!(s.negotiate("de").as_str(), "en");
        assert_eq!(s.negotiate("xx-YY").as_str(), "en");
        assert_eq!(s.negotiate("not a header at all").as_str(), "en");
        assert_eq!(s.negotiate("").as_str(), "en");
    }

    #[test]
    fn the_default_is_the_last_resort_rather_than_an_error() {
        let s = settings("fr", &["fr", "en"]);
        // No source says anything: the default, not a failure.
        assert_eq!(s.resolve(&RequestLocale::default()).as_str(), "fr");
        assert_eq!(s.negotiate("ja, ko;q=0.8").as_str(), "fr");
    }

    #[test]
    fn quality_values_order_the_candidates() {
        let s = settings("en", &["en", "fr", "de"]);
        assert_eq!(s.negotiate("de;q=0.5, fr;q=0.9").as_str(), "fr");
        assert_eq!(s.negotiate("fr;q=0.2, de;q=0.3").as_str(), "de");
        // Equal quality keeps the browser's own order.
        assert_eq!(s.negotiate("de, fr").as_str(), "de");
        // `q=0` is a refusal, not a weak preference.
        assert_eq!(s.negotiate("fr;q=0, de").as_str(), "de");
        // `*` means "whatever you have", which is the default.
        assert_eq!(s.negotiate("*").as_str(), "en");
    }

    #[test]
    fn the_sources_are_tried_in_order() {
        let s = settings("en", &["en", "fr", "de"]);
        let all = RequestLocale {
            query: Some("de"),
            user: Some("fr"),
            cookie: Some("fr"),
            accept_language: Some("fr"),
        };
        assert_eq!(s.resolve(&all).as_str(), "de");

        let no_query = RequestLocale { query: None, ..all };
        assert_eq!(s.resolve(&no_query).as_str(), "fr");

        let header_only = RequestLocale {
            accept_language: Some("de;q=0.9, fr;q=0.1"),
            ..RequestLocale::default()
        };
        assert_eq!(s.resolve(&header_only).as_str(), "de");
    }

    #[test]
    fn a_source_naming_a_locale_we_do_not_serve_falls_through() {
        let s = settings("en", &["en", "fr"]);
        let sources = RequestLocale {
            query: Some("ja"),
            user: Some("nonsense"),
            cookie: Some("fr"),
            ..RequestLocale::default()
        };
        assert_eq!(s.resolve(&sources).as_str(), "fr");
    }

    #[test]
    fn one_enabled_locale_is_no_negotiation_at_all() {
        // D11, asserted: every source says French, the server serves English
        // only, and nothing about the request is even looked at.
        let s = settings("en", &[]);
        assert!(!s.is_multilingual());
        let sources = RequestLocale {
            query: Some("fr"),
            user: Some("fr"),
            cookie: Some("fr"),
            accept_language: Some("fr"),
        };
        assert_eq!(s.resolve(&sources).as_str(), "en");
        assert_eq!(s.negotiate("fr;q=1.0").as_str(), "en");
    }

    #[test]
    fn the_default_joins_the_enabled_set() {
        let s = I18nSettings::new(loc("fr"), vec![loc("en")]);
        assert_eq!(
            s.enabled().iter().map(Locale::as_str).collect::<Vec<_>>(),
            ["fr", "en"]
        );
        assert!(s.is_enabled(&loc("fr")));
        assert!(!s.is_enabled(&loc("de")));
    }

    #[test]
    fn the_active_settings_start_english_only_and_can_be_replaced() {
        // The only test in this crate that touches the process-wide slot, so it
        // owns both halves: a process that has configured nothing is English and
        // monolingual (D11), and a save takes effect without a restart.
        let before = active();
        assert_eq!(before.default_locale().as_str(), "en");
        assert!(!before.is_multilingual());

        set_active(settings("fr", &["fr", "en"]));
        let after = active();
        assert_eq!(after.default_locale().as_str(), "fr");
        assert!(after.is_multilingual());

        set_active(I18nSettings::default());
    }
}
