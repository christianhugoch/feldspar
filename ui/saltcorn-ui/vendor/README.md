# `ui/saltcorn-ui/vendor/` — Saltcorn 1's rendering code, vendored

Saltcorn UI renders v1's views with **v1's own source** (TODO §2): the six view patterns,
the fieldviews and `@saltcorn/markup`, copied here and bundled into
`dist/view-runtime.js` by `../build.mjs`. They are copied rather than depended on because
`@saltcorn/data` as an npm dependency *is* v1's server — its `models/table.js` opens its own
`pg` pool — and the whole point is to keep six view files and leave that behind.

Taken at **`@saltcorn/data` 1.7.0-alpha.1, saltcorn/saltcorn `0508c45ac2`**. Every text file
carries a two-line header naming its upstream path and that version. **Do not edit a vendored
file**: an edit is a fork, and a fork is what a refresh silently undoes. Behaviour this server
needs to differ goes in `../src/` (see *The line* below).

## What is here

| Directory | Upstream | Files |
|---|---|---|
| `saltcorn-data/base-plugin/viewtemplates/` | `packages/saltcorn-data/base-plugin/viewtemplates/` | `list.ts`, `show.ts`, `edit.ts`, `feed.ts`, `filter.ts`, `listshowlist.ts`, and the back-compat `viewable_fields.ts` re-export. **Not** `room.ts` or `workflow-room.ts` (socket views, out of scope). |
| `saltcorn-data/base-plugin/` | same | `types.ts`, `fieldviews.ts`, `fileviews.ts` |
| `saltcorn-data/` | `packages/saltcorn-data/` | `plugin-helper.ts`, `viewable_fields.ts`, `utils.ts`, `evaluator.ts` (the host builds `getState().evaluator` from it) |
| `saltcorn-data/models/` | same | `form.ts`, `fieldrepeat.ts`, `expression.ts`, `layout.ts`, `library.ts` (`resolveSegment` and `suitableFor`; its `db` is `../src/shims/library-db.ts`) |
| `saltcorn-data/diagram/` | same | `node_extract_utils.ts` and `nodes/*` — what a pattern's `connectedObjects` is built from |
| `saltcorn-data/tests/mocks.ts` | same | `fieldviews.ts` renders with its `mockReqRes` |
| `saltcorn-data/mobile-mocks/` | same | `saltcorn/plugin-testing.ts`, `npm/dockerode.ts`, `npm/xml2js.ts`, `node/fs-extra.ts` — v1's own mocks for running `@saltcorn/data` without its server |
| `saltcorn-markup/` | `packages/saltcorn-markup/` | every non-test `.ts` file |
| `common-code/` | `packages/common-code/` | `index.ts`, `relations/*` |
| `db-common/` | `packages/db-common/` | `internal.ts`, `dbtypes.ts` (the pure `sqlsanitize`/`sqlFun` helpers) |
| `saltcorn-types/` | `packages/saltcorn-types/` | `common_types.ts`, `generators.ts` — the two with runtime code the above calls |
| `plain-date/` | `packages/plain-date/` | `index.ts` |
| `v1-exports.json` | *generated* | the export names each library specifier has under a v1 `require`, recorded from the checkout by `refresh.sh` |

And beside it, `../public/` — the browser assets (§9), staged into `dist/public/`:

| File | Upstream |
|---|---|
| `jquery-3.6.0.min.js`, `saltcorn-common.js`, `saltcorn.js`, `saltcorn.css` | `packages/server/public/` |
| `bootstrap.bundle.min.js` (Bootstrap 5.3.3) | `packages/saltcorn-sbadmin2/public/` |
| `fontawesome-free/css/all.min.css`, `fontawesome-free/webfonts/*`, `fontawesome-free/LICENSE.txt` (Font Awesome Free 5.15.3) | `packages/saltcorn-sbadmin2/public/fontawesome-free/` — the font binaries and the licence cannot carry a header, so this row is their record |
| `bootstrap.min.css` (Bootstrap 5.3.3) | `saltcorn/any-bootstrap-theme` `public/bootstrap.min.css` — the monorepo ships Bootstrap's css only compiled into sb-admin-2's theme, and the one layout here (§9) wants it plain |

## The line

What is vendored is drawn along one line: **everything that renders, and nothing that reaches a
database, a tenant or a socket** (TODO §2). What sits across the line is answered by the host,
through the one `require` the bundle imports (`createRequire` from `node:module`), which in the
module worker is `module-host.mjs`'s patched `Module._load`:

- **Host-supplied `@saltcorn/data` modules** (`build.mjs`'s `HOST_DATA_MODULES`):
  `models/table`, `field`, `view`, `page`, `trigger`, `file`, `user`, `crash`, `page_group`,
  `workflow`, `tag`, `config`, `discovery`, and `db/state`, `db/index`. A vendored file's
  relative import of any of them becomes `require("@saltcorn/data/<path>")`.
- **Node's built-ins** (`vm`, `path`, `crypto`, `fs`, …): the worker's own.
- **`plugin-helper.ts`**, per export: `../src/plugin-helper.ts` is the partition, and every
  import of plugin-helper — the vendored patterns' included — reaches it. Kept, refused (fatal on
  call, naming the export) or absent (`undefined` to a plugin, which feature-detects it). The
  `bundle_shape` test holds every upstream export to exactly one of the three.
- **Shims in `../src/shims/`**, each saying why: `module` (utils' lazy `require` of `db/*`),
  `os` (utils asks for the home directory at load), `library-db` (`models/library.ts`'s `db`:
  its two reads answered from `getState().library`, the application's snapshot, and every write
  refused by name — the admin API writes the library, never the worker), `vm2` (formulas run
  with `vm.runInNewContext`, the branch v1 takes off Node — the isolation boundary is the
  worker's empty permission set) and `https-proxy-agent` (views make no outbound requests).
  `module`, `os` and `library-db` are **keyed on their importer** (`build.mjs`'s `KEYED_SHIMS`):
  only that file's import reaches them. The build records who actually imported each shim in
  `dist/view-runtime.importers.json`, and `bundle_shape` holds it to that table.
- **npm packages** the vendored files import (`moment`, `underscore`, `xss`, …) are bundled,
  pinned in `../package.json` to the versions v1's lock file resolved.

## Refreshing

```sh
# a built Saltcorn 1 checkout (npm install && npm run tsc), and any-bootstrap-theme
ui/saltcorn-ui/vendor/refresh.sh ~/saltcorn ~/any-bootstrap-theme
cd ui/saltcorn-ui && npm run build
cargo test -p sc-viewpattern bundle_shape
```

`refresh.sh` replaces every vendored directory and `../public/`, rewrites the headers with the
new version, and regenerates `v1-exports.json` by requiring each library specifier from the
checkout. Then:

1. **Update the version line at the top of this file.**
2. Build. A new import in a vendored file fails the build naming the specifier: vendor it (add it
   to `refresh.sh`'s lists) if it renders, add it to `HOST_DATA_MODULES` if it is a model the host
   answers, or shim it in `src/shims/` with the reason.
3. Run `bundle_shape`. A new plugin-helper export fails it until it is put on one side of the
   partition; an export v1's `require` gained fails it until the library answers it.
4. Update the pinned npm versions in `../package.json` if v1's lock file moved them.
