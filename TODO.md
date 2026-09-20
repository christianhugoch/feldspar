# Saltcorn v2 — Internationalisation: one catalogue, two runtimes

Ordered, checkable task list for the twenty-eighth milestone after the MVP. Earlier lists are
archived in [docs/TODO-mvp.md](./docs/TODO-mvp.md) (the MVP) and
`docs/TODO-post-mvp-1.md` … [docs/TODO-post-mvp-27.md](./docs/TODO-post-mvp-27.md) (most
recently: streams as an entity, and the coding agent rebuilt for cheap models). Scope and
rationale are in **[docs/I18N.md](./docs/I18N.md)** — the proposal, cited below as *P§n* — and
in [docs/GOALS.md](./docs/GOALS.md).

Every string this product puts in front of a person is English, and there are three populations
of them: **A**, ours, written at release; **B**, the admin's, written while they build an
application; **C**, the end user's, typed into a row. v1 had an answer for each and they were
three unrelated mechanisms. Here A is split across a Rust core and a React SPA that share no
library, and B has moved from a view's configuration JSON — which a server can walk — into
`.tsx` files an agent writes. This milestone is the one facility that covers A and B on both
sides of that split. C is deliberately not in it (P§5).

**Milestone definition of done:** an admin sets the enabled locales to English and French,
picks French in the user menu, and the table editor, the settings screen and a *stream
provider's* configuration form — whose labels are Rust data — are all in French. A React
application whose labels are written `t("Add a task")` lists its strings on a Translations
screen with a coverage figure; **Translate missing** fills the French column through the
configured LLM, rejecting any translation that lost a `{placeholder}`; saving writes
`<project>/locales/fr.json` in the app's file store, and the running application serves French
on the next reload **without a rebuild**. A Saltcorn UI application's list-view header is
translated from the same screen, out of `_fd_translations`. `feldspar i18n lint` is clean for
`ui/admin/src`. The same scenario — with a scripted translator in place of the LLM — passes in
`cargo test`.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

---

# The specification

The argument for each decision is in [docs/I18N.md](./docs/I18N.md); what follows is what the
code has to be. The eleven decisions, in one line each:

| | Decision | P§ |
|---|---|---|
| D1 | The message id **is the English source text** | 4 |
| D2 | Placeholders are `{name}`; a message is **not** a `{{ }}` template | 4 |
| D3 | Plurals are CLDR categories: `Intl.PluralRules` / `icu_plurals` | 4 |
| D4 | One JSON format; domains `core`, `admin`, `builder`, one per application; files for an app with a tree, rows for one without | 4 |
| D5 | **The server translates everything the server says** | 4 |
| D6 | Extraction is a Rust tree-sitter pass, not a step in anybody's build | 4 |
| D7 | An application's catalogue is **served**, not bundled | 4 |
| D8 | One locale per request, negotiated once, **passed explicitly** | 4 |
| D9 | The LLM translates; the machine checks the placeholders | 4 |
| D10 | No new npm dependency — the runtime is generated | 4 |
| D11 | Zero cost for an application with no locales | 4 |

### 1. The catalogue

One file (or one row) per locale per domain, a flat JSON object, key = English source:

```json
{
  "Incorrect password": "Mot de passe incorrect",
  "Delete {name}?": "Supprimer {name} ?",
  "{count} rows": { "one": "{count} ligne", "other": "{count} lignes" },
  "verb\u0004Order": "Commander"
}
```

A value is a string, or an object keyed by CLDR plural category selected on the argument named
`count`. `\u0004` separates a disambiguating context from the source text (gettext's `msgctxt`,
so the file stays flat and hand-editable). There is **no `en.json`** unless English itself needs
plural forms: the key is the English.

### 2. The format, stated once

`{identifier}` is a placeholder; `{{` is a literal `{`; anything else between braces is a
literal run. An identifier that has no argument **renders as written** — a visible `{name}` is a
bug report and an empty string is a mystery. A message is never HTML: it is escaped by whatever
renders it, exactly as any other string is.

This is implemented twice — `sc_i18n::format` and the generated `messages.ts` — and the two are
held to each other by `crates/sc-i18n/fixtures/format.json`, a corpus of (message, args,
expected) triples that a Rust test and a vitest test both run. Two implementations of one thing
disagree by the third bug fixed in one of them (§13.3's rule); a shared fixture is what makes
this instance affordable.

### 3. Where everything lives

**`sc-i18n`, a new crate at layer 0** — `sc-error` and nothing else — because `sc-auth` and
`sc-types` have to be able to call `t!` and the dependency cannot point the other way. It holds
the `Locale`, the negotiation, the `Catalog`, `format`, plural selection, the `t!`/`tc!` macros,
`translate_spec`, and the `Translator` seam. The extractor is behind a **`extract` feature**
(tree-sitter; `sc-repomap`'s `grammars` arrangement, for `sc-repomap`'s reason).

Three seams, each inverted the way its neighbours' already are:

| Seam | Declared in | Implemented in | Installed by |
| --- | --- | --- | --- |
| `Translator` — how missing messages get filled | `sc-i18n` | `sc-server`, `sc-cli` (over `sc-llm`) | the caller |
| `CatalogStore` — where an application's catalogue is | `sc-app::i18n` | files (`sc-files`) · rows (`_fd_translations`) | `sc-app`, by whether the framework has a tree |
| `ViewRuntime::strings_for_i18n` — a view's own B strings | `sc-viewpattern` | `sc-module::ModuleViewRuntime` | already mounted |

### 4. `_fd_translations`

Per application, exactly as `_fd_views`/`_fd_pages`/`_fd_library` are (§9): `name` is the locale
tag, unique per application; `messages` is the JSON object of §1; `application` is by value, so
deleting an application deletes its translations. It is the home for the catalogue of an
application that has **no project tree** — a Saltcorn UI app's definition is rows, so its
translations are rows. A code application's are files in its repository, because that is where
its definition is (P§4, D4).

### 5. The locale on a request

`?lang=` → the user's `language` column → the `lang` cookie → `Accept-Language` → the
application's default → `default_locale`. Matched against `enabled_locales` with a real fallback
chain (`pt-BR` → `pt` → default). Negotiated once in the router; carried on `AppRequest`,
`ViewRequest` and the admin handler context; answered with `Content-Language` and
`Vary: Accept-Language, Cookie`. **Never ambient** (D8): a trigger emailing a customer
translates against *that customer's* `language`, which is a bug class v1 had.

---

## Phase 1 — `sc-i18n`: the kernel

- [x] 1.1 The crate at layer 0: `Locale` (BCP-47 parse over `icu_locale_core`, the fallback
      chain, `direction()`), `negotiate(accept_language, enabled, default)` with quality values,
      and the tests that `pt-BR` falls back to `pt`, that an unknown tag never escapes the
      enabled set, and that the default is the last resort rather than an error.
- [x] 1.2 `Catalog`: parse a domain's JSON (a value that is neither a string nor an object of
      known plural categories is an error naming the key), lookup with the fallback chain, and
      plural selection with `icu_plurals` (`compiled_data`). A plain string where plurals were
      expected is used as written.
- [x] 1.3 `format(message, args) -> String` per §2, and `crates/sc-i18n/fixtures/format.json` —
      the corpus, with the literal-brace, missing-argument, unknown-identifier and
      `{{`-escape cases in it.
- [x] 1.4 `t!(loc, "…")` / `t!(loc, "…", name = v)` / `tc!(loc, "ctx", "…")`, and
      `translate_spec(&mut Vec<FormField>, loc)` — labels, `sublabel`s and option labels, with a
      test that a spec with no catalogue entry comes back untouched (D5, D11).
      **Deviation:** `translate_spec` lives in `sc-types`, not `sc-i18n` — a function that walks a
      `FormField` cannot live in the crate `sc-types` depends on. It translates the **label** and
      nothing else: `FormField` has no `sublabel` (the settings screen's help text hangs off
      `sc_config::ConfigDef`, translated at the API edge in 3.2), and an option is a *value*
      whose translation would fail its own validation.
- [x] 1.5 The `Translator` seam and `translate_missing(catalog, source, translator, keys)` — the
      target locale is the catalogue's own, and the keys to fill are passed alongside, since a
      catalogue holds translations and not the set of messages that want one:
      batching, and the **validation** — a returned message whose placeholder set or plural
      categories differ from the source's is rejected and left untranslated, with the key in the
      warning. Tested against a scripted translator that mangles one of each (D9).
- [x] 1.6 The two config keys (`default_locale`, `enabled_locales`) in a new `sc-config`
      section, the nullable `language` column on `users` with its place on the user form, and
      negotiation wired into `sc-server`'s router with the response headers. A server with one
      enabled locale does no work (D11) — asserted, not hoped.
- [x] 1.7 `crates/sc-i18n/locales/` exists with `README.md` saying what these files are, who
      writes them and what `feldspar i18n translate` does to them.

## Phase 2 — Extraction, the lint, and the CLI

- [ ] 2.1 `sc_i18n::extract` (feature `extract`): tree-sitter queries over `.ts`/`.tsx`/`.js`/
      `.jsx` finding `t(…)`, `tc(…)` and `<T text="…">`, each yielding key, file and line. A
      call whose first argument is not a string literal (or a substitution-free template
      literal) is an **error** naming file and line — silently skipping it is how an app ends up
      half-translated with nobody knowing.
- [ ] 2.2 The lint: the same parse, reporting JSX **text nodes** and `label` / `title` /
      `placeholder` / `aria-label` attributes holding a bare English literal that no `t` wraps.
      Tested on a fixture file with one of each and on one that is clean.
- [ ] 2.3 The Rust side: a scanner over `t!(`/`tc!(` call sites in `crates/**/*.rs`, and the
      test that every key it finds is one the shipped `core` catalogues can be checked against.
- [ ] 2.4 `feldspar i18n extract|lint|check|translate` in `sc-cli`, with `--domain` and
      `--locale`, and `Translator` implemented over `sc-llm`'s configured provider. `check`
      reports coverage per locale and **fails** on exactly one thing: a placeholder or plural
      mismatch between a translation and its key. Coverage is a number, not a gate — a new
      English string must not break the build.
- [ ] 2.5 `whale-ci.yml` runs `feldspar i18n check` beside `fmt` and `clippy`.

## Phase 3 — Type A: the product's own strings

- [ ] 3.1 Rust user-facing messages wrapped in `t!`, locale plumbed to them: authentication and
      sign-up, validation messages that reach a form, the application-facing 4xx sentences, and
      the admin API's refusals. **System errors are not wrapped** (§16) — they go to the log and
      to an admin reading a stack, and a translated one loses the string you would search for.
- [ ] 3.2 `translate_spec` applied at the admin API edge (D5): the settings sections, every
      extension point's `config_spec` (actions, agents and their traits, file stores, LLM
      providers, model providers, **stream providers**, table providers, frameworks), and the
      `FrameworkInfo`/provider descriptions. One test walks every declared spec in a non-English
      locale and asserts the labels moved.
- [ ] 3.3 The admin SPA runtime: `ui/admin/src/i18n.tsx` (`I18nProvider`, `useT`, `<T>`) over a
      lazily `import()`-ed `src/locales/{locale}.json`, the locale picker in the user menu
      writing the user's `language`, `<html lang>`/`<html dir>`, and Bootstrap's
      `bootstrap.rtl.min.css` swapped in for an RTL locale.
- [ ] 3.4 The sweep: every literal in `ui/admin/src` wrapped, `feldspar i18n lint
      ui/admin/src` clean, and a vitest that runs `format` against
      `crates/sc-i18n/fixtures/format.json` (§2).
- [ ] 3.5 `ui/builder`: fill v1's `translations` map in the builder options
      (`builder-routes.ts`, `index.ts`) from the `builder` domain. The vendored
      `useTranslation` is *already* a lookup keyed by the English phrase — what is empty is the
      map — so this is the item carried since the builder milestone, and it is two call sites.
- [ ] 3.6 Ship the first locales — `fr`, `de`, `es`, `zh-Hans`, `ar` — generated by
      `feldspar i18n translate` for the `core`, `admin` and `builder` domains and committed.
      Arabic is in the set on purpose: it is what makes the `dir` work visible.

## Phase 4 — Type B: applications

- [ ] 4.1 `sc-app::i18n`: the `CatalogStore` trait with both implementations — `<project>/locales/
      {locale}.json` through the app's file store, and `_fd_translations` (§4) with its
      bootstrap, save, list and delete-with-the-application. The app's `locales` and
      `default_locale` go in `Application.attributes` (§9's sparse rule: most apps have none).
- [ ] 4.2 `GET {mount}/i18n/{locale}.json`, mounted **beside** the endpoint sets as the observe
      socket is (§13.2), with an ETag, a per-mount cache, and re-read on save and on `SIGHUP` —
      so a translation is live without a bundler (D7).
- [ ] 4.3 The generated runtime: `messages.ts` in `common_runtime_files` (the format, the
      catalogue fetch, the negotiation — framework-neutral, so a module's framework gets it
      unchanged) and `i18n.tsx` in React's `runtime_files` (`I18nProvider`, `useT`, `t`, `<T>`
      with element values). The scaffold wires the provider into `main.tsx`, uses `t()` in the
      pages it writes, and `AGENTS.md`, `SKILL.md` and the runtime `README.md` say that
      user-visible text goes through `t()`.
- [ ] 4.4 The admin API and the Translations screen for an application: the extracted keys with
      their coverage per locale, the grid, **Translate missing**, the unwrapped literals the
      lint found, and the orphans (a key in the catalogue that the source no longer uses —
      shown, never deleted). Saving writes through the `CatalogStore` and re-reads the mount.
- [ ] 4.5 Saltcorn UI: `ViewRuntime::strings_for_i18n` over the vendored `getStringsForI18n` (it
      is intact on every pattern), `translate` in `module-host.mjs` becoming a catalogue lookup
      that keeps v1's positional `%s`, and `getLocale()` answering the request's locale instead
      of `"en"`. Same screen, same button, rows instead of files.
- [ ] 4.6 The end-to-end test, against a scripted translator: scaffold an app, extract its
      strings, fill French, read `{mount}/i18n/fr.json` back, and assert a request with
      `Accept-Language: fr` gets `Content-Language: fr` — plus the Saltcorn UI half, where a
      list view's header comes back translated.

## Phase 5 — Documentation

- [ ] 5.1 `docs/TECHNICAL_DESIGN.md`: a new **§16.x Internationalisation** (the three
      populations, the catalogue, the format, the domains, negotiation, the seams and what is
      *not* translated), `sc-i18n` in §2's crate tree and §3's layers, `Translator` and
      `CatalogStore` in §2.1's extension-point table, `_fd_translations` in §9's table
      catalogue and §9.2's relationships, the catalogue route in §13.2, and the `language`
      column in §7.1.
- [ ] 5.2 `docs/tutorial-i18n.md`: turn on two locales, see the admin UI in French, translate a
      React application end to end (including what the coding agent should be told), then the
      same for a Saltcorn UI application.
- [ ] 5.3 The CHANGELOG entry for the milestone.

---

## Explicitly OUT of scope for this milestone

- **Type C — the user's own data.** The brief says so and the read path says so: v1's
  `localizes_field` becomes a field attribute whose column the row layer projects in place of
  the base one, and that belongs with the projection work rather than bolted onto it (P§5).
- **MessageFormat 2.** Right answer, wrong year: the spec is stable, the Rust implementation is
  not. The flat-JSON catalogue is convertible to it the day that changes (P§7).
- **XLIFF/PO export and a TMS integration** (Weblate, Crowdin). A converter over a flat JSON
  object is a script; wiring a translation-management system is a product decision.
- **Number, date and currency formatting in Rust.** The browser has `Intl`; the server has
  little reason to format a number for a human, and Saltcorn UI's date fieldviews keep doing
  what they do.
- **An RTL layout audit.** `dir` and Bootstrap's RTL stylesheet, and Arabic in the shipped set
  so the remaining gaps are visible rather than theoretical.
- **`ui/ide`.** The VS Code workbench brings its own localisation machinery and its own language
  packs; wrapping our shell around it is a different job.
- **Translating what a module declares.** A v1 plugin's field labels are its own strings in its
  own package; the server translating them would be the server claiming authorship of text it
  did not write. They pass through.
- **Per-locale URLs** (`/fr/tasks`). The locale is negotiated, not routed.

## Carried past this milestone

- From this milestone: nothing yet.
- From TODO-post-mvp-27: the live-broker half of the streams definition of done (10.3). It needs
  a real MQTT broker, which this machine has not got; `docs/tutorial-streams.md` is the script.
- From TODO-post-mvp-26: running the agent eval against a real provider (11.4) and walking the
  agent milestone's definition of done by hand (12.3). Both need an API key and spend money;
  `docs/AGENT_EVAL.md` has the command and the heading the numbers go under.
- From TODO-post-mvp-25: page groups, HTML-file pages, copilot layout generation, uploading from
  the builder, v1's help topics, formula-editor completions, replacing CKEditor 4, a menu editor,
  cloning pages and views, sharing library items, collaborative editing, and the builder in a
  plugin pattern's mode.
- From TODO-post-mvp-24: `room`/`workflow-room` and realtime, tags, file upload from an Edit
  view, themes as plugins, a v1 `db` module for plugins, and externalising inline handlers to
  drop `'unsafe-inline'` from Saltcorn UI's CSP.
