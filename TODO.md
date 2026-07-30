# Saltcorn v2 — The File-Store IDE TODO

Ordered, checkable task list for the fifth milestone after the MVP. Earlier lists are archived
in [docs/TODO-mvp.md](./docs/TODO-mvp.md) (the MVP),
[docs/TODO-post-mvp-1.md](./docs/TODO-post-mvp-1.md) (file stores + the React framework),
[docs/TODO-post-mvp-2.md](./docs/TODO-post-mvp-2.md) (the `_sc_tables`/`_sc_fields` overlays,
rich types and File fields), [docs/TODO-post-mvp-3.md](./docs/TODO-post-mvp-3.md) (ownership
formulae, calculated fields and row-level security) and
[docs/TODO-post-mvp-4.md](./docs/TODO-post-mvp-4.md) (actions and triggers); scope and rationale
remain in [docs/GOALS.md](./docs/GOALS.md) and
[docs/TECHNICAL_DESIGN.md](./docs/TECHNICAL_DESIGN.md) (**§12.1**, written for this milestone).

**Milestone definition of done:** an admin opens a file store from the admin UI and gets the
**VS Code workbench** in the browser — project tree on the left, tabs of editors in the main
area, command palette, find-in-files, keybindings — editing the real store. In it they format a
file with **prettier**, see **TypeScript errors** for the project as `tsc` would report them, and
**build the application** whose source that store holds, with a failed build's diagnostics landing
in the Problems panel by file and line.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

---

## Decisions taken up front

1. **Embed VS Code, do not rebuild it.** `@codingame/monaco-vscode-api`'s
   `workbench-service-override`, not a Monaco editor component with a hand-written tree and tab
   bar. The tree, tabs, palette, search and settings editor are the deliverable and they already
   exist. §12.1 records the two rejected candidates (`vscode-web`, dead since 2024 and archived;
   `@typefox/monaco-editor-react`, one editor and one language client per component) and why.
2. **No extension is packaged.** The workbench runs in `ui/ide`'s own JavaScript context, so the
   build command and the formatter are `registerExtension(<manifest object>)` plus ordinary
   `vscode.*` calls. If this milestone ever produces a `.vsix`, something has gone wrong.
3. **`ui/ide` is a separate page, not a screen in the SPA.** VS Code initializes once per page and
   cannot be unloaded; the bundle is far larger than the SPA's; the workbench owns the viewport.
   Served under `/ide/`, opened as `/ide/?store=<name>`.
4. **The IDE's filesystem is the file manager's API.** No new file endpoints: a
   `FileSystemProvider` over `browseFiles`/`readFile`/`writeFile`/`makeDirectory`/`deleteFile`/
   `renameFile`, so every backend is editable, and the access rules of §9 apply unchanged.
5. **TypeScript semantics come from a real language server on the server**, not tsserver in a web
   worker — the project's `node_modules` stays where it is. This is the one capability gated on
   `FileStore::local_path`, exactly as a framework's build step is; a store without one is *told*
   it has no semantics rather than silently getting worse ones.
6. **Prettier runs in the browser**, from `prettier/standalone` with the project's `.prettierrc`
   read through the filesystem provider. Formatting must not be the feature that stops working on
   a store with no `node_modules`.
7. **Tests split by side.** Rust tests for the server's additions (the route, its CSP, the
   WebSocket's auth, the language-server process). `vitest` for `ui/ide`'s own logic — the
   filesystem provider against a stubbed client, prettier config resolution, the store→application
   match — plus `tsc --noEmit && vite build` in CI, as `ui/admin` already has.

---

## Phase 1 — `ui/ide`: the workbench, served and authenticated ✅

- [x] New Vite + TypeScript project `ui/ide` (no React), with `base: "/ide/"` and the CSS-as-string
      resolver plugin `@codingame/monaco-vscode-api` requires for its stylesheets.
- [x] Boot the workbench: `initialize` with `workbench-service-override` plus the service overrides
      §12.1 names (files, quickaccess, search, keybindings, configuration, storage, textmate,
      themes, languages, extensions), into a full-viewport container.
- [x] `typescript-basics` and the other grammar/default extensions needed for a React project
      (`javascript`, `json`, `css`, `html`, `markdown`) — grammars now; semantics in phase 4.
- [x] Generate the typed admin client into `ui/ide/src/client.ts` from the same
      `emit_admin_client` example `ui/admin` uses, and add the `gen-client` script.
- [x] `sc-server`: serve `ui/ide/dist` under `/ide/`, admin-only through the existing session
      middleware, with its own bootstrap document and its own relaxed CSP (inline styles,
      `worker-src blob:`) — leaving the SPA's strict policy untouched.
- [x] Tests: the route serves the IDE document only to an admin, and its CSP header is the relaxed
      one while `/` still gets the strict one.
- [x] **Done when** `/ide/?store=<name>` shows the workbench — tree, tabs, palette — over an empty
      in-memory workspace.

## Phase 2 — The store as a filesystem ✅

- [x] A `FileSystemProvider` over the file endpoints (`stat`, `readDirectory`, `readFile`,
      `writeFile`, `createDirectory`, `delete`, `rename`), registered with
      `registerFileSystemOverlay`, with the workspace folder derived from `?store=`.
- [x] Handle what the API does and does not give: no watch, so `onDidChangeFile` is driven by the
      IDE's own writes plus an explicit refresh; base64 for non-UTF-8 bytes; a store that is
      defined but not connected surfaces its reason (§9's `connected`/`why`) as a dialog, not a
      stack trace.
- [x] Link into the IDE from the admin SPA: from `FileStores` (per store) and from an application's
      derived `source.store` (§13.3), the same places the file manager is reached from.
- [x] Tests: `vitest` over the provider against a stubbed client — read/write round-trip, directory
      listing to `FileType`, delete/rename, and the error mapping for a missing path.
- [x] **Done when** an admin edits and saves a scaffolded React app's `src/App.tsx` in the
      workbench and the file manager shows the new contents.

## Phase 3 — Prettier, and the build button ✅

- [x] Prettier in the browser: `prettier/standalone` with the estree/typescript/babel/postcss/html/
      markdown plugins, registered as a `DocumentFormattingEditProvider` for the languages a React
      project contains, so format-on-save and the format command both work.
- [x] Resolve the project's own configuration through the provider — `.prettierrc*` or
      `package.json`'s `prettier` key, nearest ancestor wins — and pass it as options. A `.js`,
      YAML or TOML configuration is *found* and reported as unusable rather than stepped over.
- [x] The in-page extension: `registerExtension(manifest).setAsDefaultApi()`, contributing a
      **Build** command with a palette entry and a visible button, which calls `buildApplication`
      for the application whose `source.store` is this store (client-side match over
      `listApplications`; no new endpoint), and reports success as a notification.
- [x] A failed build's diagnostics — an Application error carrying `tsc`'s and the bundler's output
      (§16) — parsed into a `DiagnosticCollection` so the Problems panel names file and line.
      Required fixing `run_build`, which quoted stderr in preference to stdout and so dropped
      `tsc`'s diagnostics whenever npm said anything; both streams are now carried.
- [x] A build writes into the source tree (the generated client) and into `dist/`, which is a
      change made outside the editor by an action taken inside it: the build command drops what
      the filesystem remembers (`StoreFiles::forgetEverything`) rather than waiting for the
      listing lifetime to expire.
- [x] Tests: `vitest` for the config resolution and the store→application match; a Rust test that a
      failing build's error text carries the file-and-line diagnostics the parser relies on.
- [x] **Done when** an admin formats a file with prettier's own configuration, presses Build, and a
      deliberate type error appears in the Problems panel at the right line.

## Phase 4 — TypeScript semantics: the language server ✅

- [x] `sc-server`: a WebSocket route for a store's language server (axum's `ws` feature), admin-only
      through the same session middleware, refusing a store with no `local_path` with the reason
      spelled out. The reason rides the **close frame**, not an HTTP status: a browser cannot read
      the body of a failed handshake, so only the admin check — which needs no explanation — is
      answered before the upgrade.
- [x] Spawn `typescript-language-server --stdio` with the store's directory as its working
      directory and pipe it over the socket; one process per connection, killed when the socket
      closes, with a bound on how many may run at once (`MAX_LANGUAGE_SERVERS`).
- [x] Client side: `vscode-languageclient` + `vscode-ws-jsonrpc` against that route for TypeScript
      and JavaScript documents. **Two deviations, both argued in the CHANGELOG.**
      `monaco-languageclient` is not used: it pins `@codingame/monaco-vscode-api` at `^25`
      (released) / `^35` (unreleased) against this workbench's `36`, and two copies of that package
      are two service registries — the same argument §12.1 used to reject
      `@typefox/monaco-editor-react`. What it adds beyond that is a twenty-line
      `BaseLanguageClient` subclass, which is now in `languageClient.ts`. And the workspace folder
      URI deliberately does *not* match the server's root: the **bridge** translates between
      `/<store>` and the store's real directory, structurally over each JSON message, so the
      browser is never told the server's directory layout and the workspace folder stays the same
      for every backend.
- [x] Tell the admin when there are no semantics: a store with no local path, or a project with no
      `node_modules` yet, says so once rather than reporting thousands of phantom errors.
- [x] Tests: the route rejects a non-admin and an object-store-backed store; a spawned server
      completes an initialize handshake over the socket and reports a diagnostic for a file with a
      deliberate type error.
- [x] **Done when** typing a type error in `src/App.tsx` underlines it, with completions and
      go-to-definition working across the project's own files and its installed dependencies.

## Phase 5 — Documentation

- [ ] `docs/tutorial-ide.md`: open a store, edit the React app from the
      [React tutorial](./docs/tutorial-react-todo.md), format, fix a type error, build.
- [ ] §12.1 revised to describe what was built where it deviates from what was planned.
- [ ] CHANGELOG entries as each phase lands.

---

## Carried past this milestone

- **A server-side search endpoint.** Find-in-files walks the tree through the filesystem provider,
  which is correct but costs one request per directory; a store-side search would be one request.
- **The SCM panel.** A git store already has pull/push/commit as backend operations (§14.1) driven
  from the SPA; surfacing them as VS Code's own source-control view is a later, separate job.
- **A terminal.** It implies handing an admin a shell on the server, which is a security decision
  of its own and not one this milestone needs to take.
- **`npm install` from the IDE.** The build already installs when `node_modules` is missing
  (§13.3), which covers the case that matters.
- **Non-admin access.** §12 anticipates granting restricted, app-development-only access to
  selected non-admins; the IDE is exactly such a surface, but it ships admin-only.

## Explicitly OUT of scope for this milestone

- **Extension installation** — no marketplace, no Open VSX, no `.vsix` loading.
- **Debugging** (breakpoints, a debug adapter) and **testing views**.
- **Editing two stores in one page**, or reloading the workbench without a page load — precluded by
  VS Code's initialize-once design (decision 3).
- **Language servers other than TypeScript.**
- Everything still listed as out of scope in [docs/TODO-mvp.md](./docs/TODO-mvp.md),
  [docs/TODO-post-mvp-1.md](./docs/TODO-post-mvp-1.md),
  [docs/TODO-post-mvp-2.md](./docs/TODO-post-mvp-2.md),
  [docs/TODO-post-mvp-3.md](./docs/TODO-post-mvp-3.md) and
  [docs/TODO-post-mvp-4.md](./docs/TODO-post-mvp-4.md)
