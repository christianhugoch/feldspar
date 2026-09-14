# `ui/builder/vendor/` — Saltcorn 1's builder, vendored

The builder is v1's own `@saltcorn/builder`: the Craft.js canvas Saltcorn 1 edits view and
page layouts with, copied here and bundled into `dist/builder.js` by `../build.mjs` (TODO "The
builder" §1). It is copied rather than rewritten because the builder and the renderers form
one contract. A layout is right when `storage.js` writes it and `show.ts`, `filter.ts` or
`renderLayout` reads it the same way, and both halves are v1's. It is copied rather than
depended on because `@saltcorn/builder` is published only as v1's webpack bundle, which fetches
v1's server routes directly.

Taken at **`@saltcorn/builder` 1.7.0-alpha.1, saltcorn/saltcorn `0508c45ac2`**, **the same commit
as `ui/saltcorn-ui/vendor/`**. `refresh.sh` refuses a checkout at any other commit. Every text
file carries a two-line header naming its upstream path and that version. **Do not edit a
vendored file**: an edit is a fork, and a fork is what a refresh silently undoes. Behaviour this
server needs to differ goes in `../src/` (see *The line* below).

**GOALS says "Use TypeScript for the React code", and this directory is the stated exception**
(§1): it is v1's JSX, unchanged. `../src/` is TypeScript, and every call to this server goes
through the generated typed client (`../src/client.ts`).

## What is here

| Path | Upstream | What |
|---|---|---|
| `saltcorn-builder/` | `packages/saltcorn-builder/src/` | all of it: `index.js` (`renderBuilder`), `components/` (`Builder.js`, `Toolbox.js`, `Library.js`, `storage.js`, …) and the thirty-odd `components/elements/` |

And beside it, `../public/`: the browser assets v1's builder page links
(`saltcorn-markup/builder.ts`), staged into `dist/` by the build:

| File | Upstream |
|---|---|
| `saltcorn-builder.css`, `fonticonpicker.react.css` | `packages/server/public/`, bundled into `dist/builder.css` |
| `assets/fontIconPicker.{svg,ttf,woff}` | `packages/server/public/assets/`: the icon picker's font, named by relative URL from `fonticonpicker.react.css`. A binary (or an SVG font, whose XML declaration must come first) cannot carry a header, so this row is their record |
| `ckeditor/` (CKEditor 4.16.2, whole) | `packages/server/public/ckeditor/`. It loads its plugins, skins and languages from beside `ckeditor.js` at run time, so it is copied to `dist/ckeditor/` rather than bundled. Its `.js` and `.css` files carry the header, and the rest (images, `.md`, `.txt`) are this row's record |

**CKEditor 4 is end-of-life as open source.** It is kept because v1's Text element is written
against it. Replacing it is a change to v1's builder and belongs upstream first (*Explicitly OUT*).

## The line

What is vendored is the whole builder. What sits across the line is v1's **server** and v1's
**page**, and each crossing is answered in `../src/`:

- **v1's server** (§3). The builder reaches about twenty URL shapes on v1's server, as `fetch`
  calls and as rendered hrefs. `../src/routes.ts` puts each in exactly one column:
  - *mapped*: a typed-client call, or a URL on this server;
  - *refused*: a sentence naming the feature;
  - *unreachable*: the builder cannot reach it with what this server sends, and the reason
    is written beside it.

  `routes.test.ts` walks every URL-shaped literal in this directory and fails on one that is
  in no column. The build rewrites the free `fetch` of **every file in this directory, and no
  other**, to `builderFetch` (`../src/builder-fetch.ts`), which answers from the table. An
  unknown URL is refused naming it, never passed through. Hrefs go through one delegated
  click listener (`../src/links.ts`) over the same table.
- **v1's page** (§4). The builder calls globals v1's page defines. `../src/globals.ts` lists
  each with its origin. Some come from the Saltcorn UI scripts the host document loads (jQuery,
  Bootstrap's bundle and `saltcorn-common.js`, for `notifyAlert`, `validate_expression_elem`
  and `apply_showif`). The rest are defined there: the stubs v1's `builder.ts` installs, and
  `ajax_modal`, refused. `globals.test.ts` holds every global this directory reaches to one of
  the two lists.
- **Other origins** (7.3). Two packages the builder imports would load code from a CDN, so both
  are shimmed for the vendored importers only (`../build.mjs`'s `VENDOR_SHIMS`):
  - `@monaco-editor/react` normally loads Monaco from a CDN, or from v1's `/monaco`, through
    its AMD loader. `../src/shims/monaco-editor-react.tsx` hands it the ESM `monaco-editor`
    this bundle carries, as a lazily loaded chunk with same-origin workers.
  - `ckeditor4-react` normally loads CKEditor from `cdn.ckeditor.com`.
    `../src/shims/ckeditor4-react.tsx` points it at `dist/ckeditor/`.
- **Other v1 packages.** `@saltcorn/common-code` (the relation finder) is the copy already in
  `ui/saltcorn-ui/vendor/common-code/`, which is why the two directories must come from one
  commit.
- **npm packages** are pinned in `../package.json` to the versions v1's lock file resolved at
  that commit. The `overrides` are v1's root `package.json`'s, for the packages here:
  - `react`, `react-dom` and `react-transition-group` resolve every peer to the one version.
    `ckeditor4-react` and `@fonticonpicker/react-fonticonpicker` declare React 16 or 17 peers
    and have run on React 18 in v1 for years.
  - `immer` is pinned the same way.
  - `@monaco-editor/loader` is pinned at the version v1's lock file resolved.

  The overrides are used rather than `--legacy-peer-deps`, which would hide every other
  conflict too. v1's webpack, babel and `process` polyfill are not here. esbuild compiles the JSX, and
  `process.env.NODE_ENV` is a `define`.

## Refreshing

```sh
# ui/saltcorn-ui/vendor/refresh.sh first, from the same checkout (see its README)
ui/builder/vendor/refresh.sh ~/saltcorn
cd ui/builder && npm test
```

`refresh.sh` replaces `saltcorn-builder/` and `../public/` and rewrites the headers. It refuses
a checkout whose commit is not the one `ui/saltcorn-ui/vendor/` was taken at, and one with
uncommitted changes in the files it copies. Then:

1. **Update the version line at the top of this file.**
2. Build. A new bare import in a vendored file fails the build naming the specifier: add it to
   `../package.json` at the version v1's lock file resolved, or shim it in `../src/shims/`
   with the reason.
3. Run the tests.
   - `routes.test.ts` fails on a URL the refresh added, until it is put in a column.
   - `globals.test.ts` fails on a v1 global the refresh added, until it is listed.
   - The jsdom mount and the Craft round trip (`builder.test.ts`) fail if the refresh changed
     what a layout saves as.
4. Update the pinned npm versions in `../package.json` if v1's lock file moved them.
