//! Internationalisation: one catalogue, two runtimes (design §16.x, proposal
//! [`docs/I18N.md`]).
//!
//! Every string this product puts in front of a person is English, and there are
//! three populations of them: **A**, ours, written at release; **B**, the
//! admin's, written while they build an application; **C**, the end user's,
//! typed into a row. This crate is the kernel that covers A and B on both sides
//! of the Rust/TypeScript split. C is deliberately not in it.
//!
//! # Layer 0, and why
//!
//! `sc-error` and nothing else above it, because `sc-auth` and `sc-types` have
//! to be able to translate a sentence they are about to hand to a person and the
//! dependency cannot point the other way. Everything that needs a database, a
//! file store or an LLM is *above* this crate, behind a trait declared *in* it —
//! [`Translator`] here, `CatalogStore` in `sc-app`, exactly the arrangement
//! `StreamProvider`, `Mailer` and `JsEvaluator` already have.
//!
//! # The shape of it
//!
//! | Piece | Module |
//! |---|---|
//! | [`Locale`], its fallback chain and its [`Direction`] | [`locale`] |
//! | [`I18nSettings`], negotiation, the order of a request's sources | [`negotiate`] |
//! | [`Catalog`] (one locale) and [`Catalogs`] (a domain) | [`catalog`] |
//! | [`format`](format()) and [`Arg`] — the message format, stated once | [`format`] |
//! | [`PluralCategory`] over `icu_plurals` | [`plural`] |
//! | [`t!`] / [`tc!`] and the embedded `core` catalogues | [`macros`], [`core`] |
//! | [`Translator`] and [`translate_missing`] | [`translate`] |
//!
//! # The three rules worth knowing before writing a call site
//!
//! 1. **The message id is the English source text.** `t!(loc, "Incorrect
//!    password")`, not `t!(loc, "auth.bad_password")`. A missing translation
//!    renders correct English, which for a facility whose normal state is "60%
//!    translated" is the design rather than the failure.
//! 2. **Placeholders are `{name}`, and a message is not a template.** No
//!    expressions, no member access, no filters — see [`format`].
//! 3. **The locale is a value, passed explicitly.** There is no ambient locale
//!    to reach for, on purpose: see [`negotiate`].
//!
//! ```
//! use sc_i18n::{Locale, t};
//!
//! let loc = Locale::parse("fr")?;
//! // No `fr` catalogue entry yet: the English renders, with its argument.
//! assert_eq!(t!(loc, "Delete {name}?", name = "Tasks"), "Delete Tasks?");
//! # Ok::<(), sc_error::Error>(())
//! ```
//!
//! [`docs/I18N.md`]: https://github.com/saltcorn/feldspar/blob/main/docs/I18N.md

pub mod catalog;
pub mod core;
pub mod format;
pub mod locale;
mod macros;
pub mod negotiate;
pub mod plural;
pub mod translate;

pub use catalog::{
    CONTEXT_SEPARATOR, Catalog, Catalogs, Message, context_key, key_context, source_text,
};
pub use format::{Arg, Args, format, placeholders};
pub use locale::{Direction, Locale, parse_locale_list};
pub use macros::locale_ref;
pub use negotiate::{I18nSettings, RequestLocale, active, set_active};
pub use plural::{PluralCategory, categories_for, category_for};
pub use translate::{BATCH, Rejection, TranslateReport, Translator, check, translate_missing};
