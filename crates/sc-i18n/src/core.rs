//! The `core` domain: the product's own Rust-side strings (decision D4).
//!
//! `crates/sc-i18n/locales/{locale}.json`, embedded with `include_str!` by
//! `build.rs`, parsed once on first use. Embedded rather than stored because a
//! server with no database still has to be able to say "could not connect to the
//! database" — and in the admin's language, if that is configured.
//!
//! This is what [`t!`](crate::t) and [`tc!`](crate::tc) look in. Everything else
//! — the admin SPA's own literals, the builder's, an application's — is a
//! different domain with a different home, reached through [`Catalogs`] directly.
//!
//! **A catalogue that does not parse is a crash at first use**, and that is the
//! right failure: these files are ours, they are checked by
//! `feldspar i18n check` in CI, and a shipped catalogue with a broken entry is a
//! build that should never have been made. The alternative — silently serving
//! English — is the failure nobody notices.
//!
//! [`Catalogs`]: crate::Catalogs

use std::sync::OnceLock;

use crate::catalog::{Catalog, Catalogs};
use crate::format::Args;
use crate::locale::Locale;

include!(concat!(env!("OUT_DIR"), "/core_catalogues.rs"));

/// The parsed `core` catalogues, built once.
fn catalogs() -> &'static Catalogs {
    static CATALOGS: OnceLock<Catalogs> = OnceLock::new();
    CATALOGS.get_or_init(|| {
        let mut domain = Catalogs::new();
        for (tag, json) in CORE_CATALOGUES {
            let locale = match Locale::parse(tag) {
                Ok(locale) => locale,
                Err(e) => panic!("crates/sc-i18n/locales/{tag}.json: {e}"),
            };
            match Catalog::parse(locale, json) {
                Ok(catalog) => domain.insert(catalog),
                Err(e) => panic!("crates/sc-i18n/locales/{tag}.json: {e}"),
            }
        }
        domain
    })
}

/// Translate a `core` message for `locale`, substituting `args`.
///
/// What [`t!`](crate::t) expands to. Public because a call site that builds its
/// arguments in a loop cannot use the macro, not because anybody should prefer
/// it.
pub fn translate(locale: &Locale, key: &str, args: &Args<'_>) -> String {
    catalogs().translate(locale, key, args)
}

/// The locales the shipped `core` domain has catalogues for.
///
/// Empty in a build with no `locales/*.json`, which is the state this crate
/// ships in until the first locales land (task 3.6).
pub fn locales() -> Vec<&'static Locale> {
    catalogs().locales()
}

/// The `core` catalogues, for the CLI's `check` and `translate` commands.
pub fn domain() -> &'static Catalogs {
    catalogs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_untranslated_message_renders_its_english() {
        // D1: a key no catalogue has is already a correct answer, in whatever
        // locale it is asked for.
        let fr = Locale::parse("fr").unwrap();
        let key = "A message no shipped catalogue will ever hold {name}";
        assert_eq!(
            translate(&fr, key, &[("name", crate::Arg::from("x"))]),
            "A message no shipped catalogue will ever hold x"
        );
    }

    #[test]
    fn every_shipped_catalogue_parses() {
        // The assertion is that building the domain does not panic; the count is
        // whatever `locales/` holds.
        assert_eq!(locales().len(), CORE_CATALOGUES.len());
    }
}
