//! Negotiating a request's locale, and saying which one was served
//! (design §16.x, decision D8).
//!
//! The router is the one place a locale is decided. Everything below it — a
//! handler, a framework, a view runtime, a trigger — is *given* one, because a
//! locale is a value and never an ambient: this server does work for a person on
//! a task no request owns, and an ambient locale is the mechanism that would
//! silently send that person's mail in the admin's language.
//!
//! # D11, in the one place it is visible
//!
//! [`negotiate`] answers `None` on a monolingual installation, and `None` is the
//! whole optimisation: no `Accept-Language` parse, no cookie read, no
//! `Content-Language` and no `Vary` on the way out. A server that has never
//! opened the Localisation section does exactly what it did before this module
//! existed, and the tests at the bottom of this file — with
//! `tests/locale_negotiation.rs` over real HTTP — assert that rather than
//! hoping it.

use axum::http::{HeaderMap, HeaderValue, Uri, header};
use axum::response::Response;
use axum_extra::extract::CookieJar;
use sc_auth::User;
use sc_i18n::{I18nSettings, Locale, RequestLocale};

/// The cookie an anonymous visitor's language choice is remembered in.
///
/// Named `lang` because that is what it is, and because an application's own
/// pages set it: the locale picker on a public site has no user row to write to.
pub const LANG_COOKIE: &str = "lang";

/// The query parameter that names a locale outright — the loudest signal there
/// is, and the one a link can carry.
pub const LANG_QUERY: &str = "lang";

/// Negotiate the locale for a request, or `None` when there is nothing to
/// negotiate.
///
/// `None` means *this installation serves one locale*, which is both the common
/// case and the one D11 promises costs nothing. A caller that needs a locale
/// regardless — to hand to `t!` — takes the installation default with
/// [`locale_or_default`].
pub fn negotiate(
    settings: &I18nSettings,
    uri: &Uri,
    headers: &HeaderMap,
    jar: &CookieJar,
    user: Option<&User>,
) -> Option<Locale> {
    if !settings.is_multilingual() {
        return None;
    }
    let query = query_lang(uri);
    let accept = headers
        .get(header::ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok());
    let cookie = jar.get(LANG_COOKIE).map(|c| c.value());
    Some(settings.resolve(&RequestLocale {
        query: query.as_deref(),
        user: user.and_then(User::language),
        cookie,
        accept_language: accept,
    }))
}

/// The locale a request is served in: what [`negotiate`] found, or the
/// installation's default.
pub fn locale_or_default(settings: &I18nSettings, negotiated: Option<&Locale>) -> Locale {
    match negotiated {
        Some(locale) => locale.clone(),
        None => settings.default_locale().clone(),
    }
}

/// The locale for a sentence decided *before* — or *outside* — a request's full
/// context: an authorization refusal, which is settled from the user alone.
///
/// Still not ambient (D8): the user is the request's user, passed in. What it
/// gives up is the `?lang=`, the cookie and `Accept-Language`, which is the
/// right trade for a refusal — the one signal that matters for "you may not do
/// that" is the language the person who is signed in reads, and a refusal is
/// decided in half a dozen places that have a `User` and no `Uri`.
///
/// A monolingual installation gets the default with nothing parsed (D11).
pub fn locale_for_user(user: Option<&User>) -> Locale {
    let settings = sc_i18n::active();
    if !settings.is_multilingual() {
        return settings.default_locale().clone();
    }
    settings.resolve(&RequestLocale {
        user: user.and_then(User::language),
        ..RequestLocale::default()
    })
}

/// Say which language this response is in, and what the answer depended on.
///
/// `Content-Language` is the locale that was actually negotiated — not the one
/// asked for — and `Vary: Accept-Language, Cookie` is what stops a shared cache
/// from serving one visitor's French to the next visitor. Both are skipped
/// entirely when nothing was negotiated (D11): a monolingual server has no
/// variation to declare, and declaring one would cost every cache in front of it
/// a dimension.
pub fn with_language(mut response: Response, negotiated: Option<&Locale>) -> Response {
    let Some(locale) = negotiated else {
        return response;
    };
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(locale.as_str()) {
        headers.insert(header::CONTENT_LANGUAGE, value);
    }
    headers.insert(
        header::VARY,
        HeaderValue::from_static("Accept-Language, Cookie"),
    );
    response
}

/// The `?lang=` of a URI, if it has one.
///
/// A hand-rolled scan rather than a query parser because this runs on every
/// request of a multilingual server and the answer is almost always "there is no
/// query string at all".
fn query_lang(uri: &Uri) -> Option<String> {
    let query = uri.query()?;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        if key == LANG_QUERY {
            return Some(percent_decode(value));
        }
    }
    None
}

/// The minimum of percent-decoding a language tag can need: `%2D` for a hyphen,
/// `+` for a space. A tag is `[A-Za-z0-9-]`, so nothing here has to be
/// general — and a value that decodes to nonsense is refused by the parse, not
/// by this.
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes.get(i) {
            Some(b'%') => {
                let hex = raw
                    .get(i + 1..i + 3)
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(byte) => {
                        out.push(char::from(byte));
                        i += 3;
                    }
                    None => {
                        out.push('%');
                        i += 1;
                    }
                }
            }
            Some(b'+') => {
                out.push(' ');
                i += 1;
            }
            Some(byte) => {
                out.push(char::from(*byte));
                i += 1;
            }
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(default: &str, enabled: &[&str]) -> I18nSettings {
        I18nSettings::new(
            Locale::parse(default).unwrap(),
            enabled.iter().map(|t| Locale::parse(t).unwrap()).collect(),
        )
    }

    fn headers(accept: Option<&str>) -> HeaderMap {
        let mut map = HeaderMap::new();
        if let Some(accept) = accept {
            map.insert(
                header::ACCEPT_LANGUAGE,
                HeaderValue::from_str(accept).unwrap(),
            );
        }
        map
    }

    #[test]
    fn a_monolingual_server_negotiates_nothing() {
        // D11, asserted at the router's edge: a French browser, a French
        // cookie, a French `?lang=`, and still no work and no headers.
        let settings = settings("en", &[]);
        let jar = CookieJar::new().add(axum_extra::extract::cookie::Cookie::new(LANG_COOKIE, "fr"));
        let uri: Uri = "/api/tables?lang=fr".parse().unwrap();
        let negotiated = negotiate(&settings, &uri, &headers(Some("fr")), &jar, None);
        assert!(negotiated.is_none());

        let response = with_language(
            axum::response::Response::new(axum::body::Body::empty()),
            None,
        );
        assert!(response.headers().get(header::CONTENT_LANGUAGE).is_none());
        assert!(response.headers().get(header::VARY).is_none());
    }

    #[test]
    fn the_query_parameter_wins_and_the_header_is_the_fallback() {
        let settings = settings("en", &["en", "fr", "de"]);
        let jar = CookieJar::new();

        let uri: Uri = "/api/tables?lang=de&page=2".parse().unwrap();
        let found = negotiate(&settings, &uri, &headers(Some("fr")), &jar, None);
        assert_eq!(found.as_ref().map(Locale::as_str), Some("de"));

        let uri: Uri = "/api/tables".parse().unwrap();
        let found = negotiate(
            &settings,
            &uri,
            &headers(Some("fr;q=0.9, de;q=0.1")),
            &jar,
            None,
        );
        assert_eq!(found.as_ref().map(Locale::as_str), Some("fr"));

        // Nothing at all: the installation default.
        let found = negotiate(&settings, &uri, &headers(None), &jar, None);
        assert_eq!(found.as_ref().map(Locale::as_str), Some("en"));
    }

    #[test]
    fn the_cookie_answers_for_an_anonymous_visitor() {
        let settings = settings("en", &["en", "fr"]);
        let jar = CookieJar::new().add(axum_extra::extract::cookie::Cookie::new(LANG_COOKIE, "fr"));
        let uri: Uri = "/".parse().unwrap();
        let found = negotiate(&settings, &uri, &headers(Some("de")), &jar, None);
        assert_eq!(found.as_ref().map(Locale::as_str), Some("fr"));
    }

    #[test]
    fn a_response_says_what_it_is_and_what_it_varied_on() {
        let fr = Locale::parse("fr").unwrap();
        let response = with_language(
            axum::response::Response::new(axum::body::Body::empty()),
            Some(&fr),
        );
        assert_eq!(
            response.headers().get(header::CONTENT_LANGUAGE).unwrap(),
            "fr"
        );
        assert_eq!(
            response.headers().get(header::VARY).unwrap(),
            "Accept-Language, Cookie"
        );
    }

    #[test]
    fn a_percent_encoded_tag_is_read() {
        let settings = settings("en", &["en", "pt-BR"]);
        let uri: Uri = "/?lang=pt%2DBR".parse().unwrap();
        let found = negotiate(&settings, &uri, &headers(None), &CookieJar::new(), None);
        assert_eq!(found.as_ref().map(Locale::as_str), Some("pt-BR"));
    }

    #[test]
    fn the_default_stands_in_when_nothing_was_negotiated() {
        let settings = settings("de", &[]);
        assert_eq!(locale_or_default(&settings, None).as_str(), "de");
        let fr = Locale::parse("fr").unwrap();
        assert_eq!(locale_or_default(&settings, Some(&fr)).as_str(), "fr");
    }
}
