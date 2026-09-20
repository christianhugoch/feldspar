# `core` catalogues

One file per locale, named by its BCP-47 tag: `fr.json`, `de.json`, `zh-Hans.json`, `ar.json`.
Together they are the **`core` domain** — the product's own Rust-side strings, population **A**
of [docs/I18N.md](../../../docs/I18N.md).

## What is in a file

A flat JSON object whose **key is the English source text** (decision D1) and whose value is the
translation:

```json
{
  "Incorrect password": "Mot de passe incorrect",
  "Delete {name}?": "Supprimer {name} ?",
  "{count} rows": { "one": "{count} ligne", "other": "{count} lignes" },
  "verb\u0004Order": "Commander"
}
```

- A value is a **string**, or an **object keyed by CLDR plural category** (`zero`, `one`, `two`,
  `few`, `many`, `other`) selected on the argument named `count`. Which categories a locale needs
  is CLDR's answer, not a choice: French needs `one` and `other`, Russian needs `one`, `few`,
  `many` and `other`, Japanese needs only `other`.
- `\u0004` separates a **disambiguating context** from the source text (gettext's `msgctxt`), so
  `tc!(loc, "verb", "Order")` is filed under `verb\u0004Order` and the file stays flat.
- `{name}` is a **placeholder**. `{{` is a literal `{`. Anything else between braces is literal
  text. A translation **must use exactly the placeholders its key uses** — this is checked, and a
  translation that renames one is refused rather than shipped.
- A message is never HTML. Whatever renders it escapes it.

**There is no `en.json`**, and there should not be one unless English itself needs plural forms:
the key *is* the English, so a missing entry renders correct English rather than a message id.

## Who writes these files

Nobody, by hand, as a first move. They are generated:

```
feldspar i18n translate --domain core --locale fr
```

which extracts every `t!` / `tc!` call site in `crates/**/*.rs`, asks the configured LLM provider
for the keys this locale has no entry for, **checks each answer's placeholders and plural
categories**, keeps what passes, and writes the file back sorted. What it refuses it leaves
untranslated and names in a warning, because correct English beats a French sentence with a
literal `{nombre}` in it.

Already-translated entries are never re-translated and never overwritten, so a correction made by
hand survives the next run. Editing a file directly is fine and expected — that is why the format
is one flat object of readable sentences.

To see where each locale stands, and to fail a build on a placeholder mismatch:

```
feldspar i18n check
```

Coverage is a number, not a gate: a new English string must not break the build.

## How they are loaded

`build.rs` scans this directory and emits an `include_str!` for every `*.json` it finds, so a
catalogue is **embedded in the binary** — a server with no database still has its own messages.
Adding a locale is adding a file here; there is no list to update. A file that does not parse is
a panic at first use, on purpose: these are ours, CI checks them, and a shipped catalogue with a
broken entry is a build that should not have been made.

## What does *not* belong here

- **The admin SPA's strings** — `ui/admin/src/locales/{locale}.json`, the `admin` domain.
- **The builder's** — `ui/builder/src/locales/{locale}.json`, the `builder` domain.
- **An application's** — `<project>/locales/{locale}.json` in the app's own repository, or
  `_fd_translations` rows for a Saltcorn UI application. An application's translations live
  wherever that application's definition lives.
- **System errors.** They go to the log and to an admin reading a stack; a translated one loses
  the string you would have searched for.
- **What a v1 module declares.** A plugin's field labels are its own strings in its own package,
  and this server translating them would be it claiming authorship of text it did not write.
