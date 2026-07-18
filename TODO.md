# Saltcorn v2 — Post-MVP Implementation TODO

Ordered, checkable task list for the first milestone after the MVP. The MVP's own list is
archived in [docs/TODO-mvp.md](./docs/TODO-mvp.md); scope and rationale remain in
[docs/GOALS.md](./docs/GOALS.md) and [docs/TECHNICAL_DESIGN.md](./docs/TECHNICAL_DESIGN.md).

**Milestone definition of done:** two gaps the MVP left open are closed.

1. **File stores are first-class, admin-editable objects.** Today a store exists only as a
   `--file-store NAME=PATH` process argument held in an in-memory catalog registry: it cannot be
   created, edited or removed without editing the command line and restarting, and the admin SPA
   has no file screen at all despite the browse/read/write endpoints existing. After this
   milestone an admin creates, edits and deletes file stores in the admin UI, they persist across
   restarts, they connect and disconnect live, and the file manager those endpoints were built
   for is actually reachable.
2. **An opinionated `react` framework exists alongside the general `code` one.** Today the only
   framework is `CodeFramework`, which asks the admin for a store, a source dir, an output dir, a
   build command and a client path, and expects them to have already scaffolded a Vite project by
   hand over SSH (tutorial steps 2–3). That generality is right for the escape hatch and wrong for
   the common case. After this milestone an admin picks **React**, names the app, picks a store,
   and gets a working, scaffolded, building, authenticating React app with no shell access and no
   choices to make. `CodeFramework` stays, unchanged, as the generic option.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

---

## Phase 1 — File stores as editable, persisted objects

### 1.1 Model & persistence (`sc-files`, `sc-catalog`)

Mirrors how an application is modelled (§9/§13.2): the object exists as its stored row, one
column per field every store has, sparse values in `attributes`, strict reads.

- [ ] `FileStoreDef` value type: id, name (the catalog key), backend name, backend config (`Attrs`), optional `min_role`, description. The *definition* of a store, distinct from the connected `dyn FileStore` instance it produces
- [ ] `_sc_file_stores` table + bootstrap, following `_sc_applications` (`crates/sc-app/src/applications.rs`): name `UNIQUE` (it is the lookup key, as a subdomain is for an app), JSON `config`
- [ ] `save_file_store` / `load_file_store` / `list_file_stores` / `delete_file_store`, following `crates/sc-app/src/store.rs` — including its strictness rule: a missing or ill-typed column is an `Error::invalid` naming the store and the column, not a silent default
- [ ] Decide what a delete means: the row only, never the bytes. A store whose definition is removed must leave its directory untouched, and deleting one still referenced by an application's `file_stores` (or a `File` field's `FileStoreId`) must be refused with an error naming the referent
- [ ] Integration tests against a real Postgres: round-trip a definition, reject a duplicate name, reject a delete that would orphan a reference

### 1.2 Backend registry (`sc-files`)

The same "settings as data" move that made "the admin picks a framework" work (§13.3) — the
admin UI must render a form for a backend it knows nothing about, including one arriving later
through `sc-code`, with no per-backend special case.

- [ ] `FileStoreBackend` registry keyed by backend name, mirroring `registered_frameworks` / `framework_config_spec`: `registered_backends()`, `backend_config_spec(name) -> Result<Vec<FormField>>`
- [ ] `local` backend declares its settings (`path`, and whether to create the directory if absent). It is the only backend this milestone registers — S3 and git-remote backends stay out of scope, but the registry is the thing that makes adding them not a UI change
- [ ] `validate_file_store_config` called **on save**, exactly as `validate_framework_config` is: an unreadable path or a missing setting is the admin's to fix while they are standing in front of the form, not a 500 the first time someone browses the store
- [ ] Construct a connected `Arc<dyn FileStore>` from a `FileStoreDef` (`connect_from_def`) — the one place a definition becomes an instance
- [ ] Unit tests: spec names/labels/required-ness; unknown backend is a config error naming what is available; a bad `path` is rejected on save

### 1.3 Live connect / disconnect (`sc-catalog`, `sc-server`)

The MVP already established the pattern for applications ("the mount registry is live", §13.2);
stores get the same treatment, because editing a store must not need a restart either.

- [ ] `Catalog::disconnect_file_store` and a replace path, so an edited store rebinds in place — today `connect_file_store` rejects a name that is already connected, which makes an edit impossible
- [ ] Boot: load every `_sc_file_stores` row and connect each. One store that fails to connect (a path that has since gone missing) must not stop the server or the other stores — the same rule the MVP applied to a single app that fails to build
- [ ] Report a store that failed to connect to the admin rather than dropping it silently: a store present in the list but not connected is a state the UI has to be able to show
- [ ] Decide how `--file-store NAME=PATH` and persisted stores coexist. Proposal: the flag stays as an ephemeral, unpersisted store, connected after the stored ones, and a name clash is a startup error rather than a silent override — it is a dev-and-test convenience, and the tests use it heavily
- [ ] Integration tests: create a store at runtime → it browses without a restart; edit its path → the new path serves; delete → it stops resolving; a store with a bad path leaves the others up

### 1.4 Admin API (`sc-api`, `sc-server`)

Extends the existing `file-stores` endpoint group, which today is read/browse/write only.

- [ ] Endpoints: create / update / delete file store
- [ ] Endpoint: list registered backends with their `config_spec`, so the UI renders a settings form for a backend it knows nothing about (mirrors `listFrameworks`)
- [ ] Extend `listFileStores`' schema beyond `{name, is_git_repo}`: backend, config, `min_role`, whether it is currently connected, and the connection error if not
- [ ] File-manager endpoints the file screen needs that do not exist yet: `mkdir`, `deleteFile`, `renameFile`/`move`, and multipart or chunked upload for files too large to base64 into a JSON body
- [ ] Endpoints for `FileMeta`: get/set `min_role` and attributes on a path. `FileMeta` is modelled and xattr-backed (`crates/sc-files/src/store.rs`) with no way to reach it — the path-cumulative access rule it documents is unenforceable and untestable from the UI until there is
- [ ] Enforce the path-cumulative `min_role` rule on read/browse/write, not just store it
- [ ] Integration tests: drive each endpoint end-to-end; non-admins are rejected; path traversal (`..`, absolute paths) is rejected at the API edge as well as in the driver

### 1.5 Admin SPA (`ui/admin`)

- [ ] **File stores screen**: list (name, backend, path/summary, connected-or-error), create, edit, delete. Nav entry alongside Tables / Users / Applications
- [ ] Create/edit form: pick a backend → render its `config_spec` settings as a plain form, no backend-specific code in the screen (the same rendering `ApplicationForm` already does for a framework's spec — factor the shared "render a `FormField[]`" piece out rather than copying it)
- [ ] **File manager screen**: browse a store's tree, upload, download, create directory, rename, delete, and edit a text file in place. The MVP built `browseFiles`/`readFile`/`writeFile` — including the UTF-8 `text` shortcut explicitly "for the text editor" — and then shipped no screen that calls them
- [ ] Surface and let an admin edit a file's `min_role`
- [ ] A store that failed to connect is shown as such, with its error, and is still editable — that is precisely when an admin needs to fix its path

### 1.6 Follow-on: the framework `store` setting becomes a pick-list

Now that stores are a listable, persisted set, the compromise §13.3 documents can be retired.

- [ ] Implement `OptionsSource` in `sc-types` (§6.2: static | server query | client code). `FormField` currently carries only a static options list; `field.rs:92` already flags this as the sketch to fill in
- [ ] The `store` setting on both frameworks becomes a `ServerQuery` pick-list of connected stores instead of validated free text. Update §13.3's note, which currently explains at length why it is free text in the MVP
- [ ] Resolve whether the admin UI evaluates a `ServerQuery` generically now or whether that waits for `ui/form-runtime` (§12). A generic evaluator here is most of the runtime's option handling and should not be built twice
- [ ] Tests: an unknown store name is now rejected on save rather than at build time

---

## Phase 2 — An opinionated React framework

`CodeFramework` is the right shape for "any bundler, any layout" and the wrong shape for the
common case: five settings, all required to be mutually consistent, plus a hand-scaffolded
project the admin must create over SSH before the settings mean anything. The `react` framework
inverts that — conventions instead of settings, and the server creates the project.

### 2.1 Decide the opinions

These are the choices that stop being the admin's. Each needs deciding **before** 2.2, since the
scaffold hard-codes them; recording the reasoning here matters as much as the choice, because
"opinionated" only pays off if the opinions are defensible and stable.

- [ ] Build tooling: Vite + React + TypeScript is the presumptive answer (it is what the MVP tutorial and `ui/admin` already use)
- [ ] Routing: a router, or the file-system convention over one? Deep links already work — `CodeFramework`'s SPA fallback resolves them — so this is about what the scaffold ships wired up
- [ ] Data layer: the generated typed client alone, or the client plus a hooks layer (`useRows`, `useRow`, mutations) over it. The tutorial's step 5 is entirely hand-rolled `useEffect` + `useState` around the client; that is the boilerplate an opinionated framework should delete
- [ ] Auth: the scaffold ships a login screen, session handling and a "current user" hook against the app's own `/api/login` / `/api/logout` / `/api/whoami`. Decide whether an unauthenticated route is possible and what the default is
- [ ] Styling: what ships, and whether it is replaceable
- [ ] Where the shared runtime lives: an npm package the scaffold depends on, or vendored source in the generated project. A package is upgradable and not hackable in place; vendored source is hackable and instantly stale. This is the main irreversible decision in this phase
- [ ] Record the decisions and their rationale in `docs/TECHNICAL_DESIGN.md` §13.3, next to the code-framework description

### 2.2 The framework (`sc-app`)

- [ ] Register `react` alongside `code` in `registered_frameworks` / `framework_config_spec`
- [ ] `react_config_spec`: the file store, the app's name/sub-directory, and as close to nothing else as the decisions in 2.1 allow. Source dir, output dir, build command and client path all become conventions derived from the name, not settings
- [ ] Derive a `BuildSpec` from those conventions, so `app_source_from_config` keeps working unchanged for both frameworks
- [ ] Reuse `CodeFramework`'s serving path rather than reimplementing it — a built React app is a static bundle with an SPA fallback, which is exactly what `CodeFramework::serve` already does. The difference is configuration and scaffolding, not serving
- [ ] Sensible CSP default for the scaffolded app, since the admin is no longer making that choice either
- [ ] Unit tests: the spec is minimal and labelled; conventions resolve to the right `BuildSpec`; an app configured for `react` serves its bundle and resolves deep links

### 2.3 Scaffolding (`sc-app`)

The step that removes the SSH requirement, and the reason this is more than a settings preset.

- [ ] Generate a complete project into the store on first save (or an explicit "Scaffold" action): `package.json`, Vite config, `tsconfig.json`, `index.html`, entry point, app shell, login screen, router wiring, `.gitignore`
- [ ] Generate against the app's *actual* tables — the scaffold should come up showing real data, not a placeholder counter. The endpoint set is already derived per-app (`app_endpoints`), so the shape is known
- [ ] Wire the generated typed client in at the conventional path, so the existing client-generation step lands where the scaffold already imports from
- [ ] `git init` the project if the store is a git repo and the sub-directory is not already tracked — §13.3 expects an app's source to be a git repo, and the MVP left that to the admin's shell
- [ ] Never overwrite: scaffolding into a non-empty directory must refuse with a clear error, not clobber an admin's work. Re-scaffolding an existing app is a separate, explicit, destructive action if it exists at all
- [ ] Run `npm install` when `node_modules` is absent, as part of the build. The tutorial currently makes the admin do this by hand, and an admin with no shell cannot
- [ ] Surface install and scaffold failures as **Application** errors carrying the tool's own output (§16), the same as build failures
- [ ] Integration tests: scaffold → build → serve, end to end, with no shell step; scaffolding into an occupied directory is refused; a scaffolded app's generated client compiles against its own endpoints

### 2.4 Admin SPA (`ui/admin`)

- [ ] Framework picker distinguishes the two meaningfully — React as the default path, `code` presented as the generic escape hatch — rather than as two equal names in a dropdown
- [ ] Picking React shows the short form (store + name); picking `code` shows today's five settings. Both still render from `config_spec` with no framework-specific code in the screen
- [ ] Scaffold/build outcome surfaced with its log, reusing the existing build banner
- [ ] From the app row, a link into the file manager at the app's source directory — the loop an admin actually works in is edit-file → build → view

### 2.5 Documentation

- [ ] Rewrite `docs/tutorial-react-todo.md` for the React framework: steps 2 and 3 (SSH in, `npm create vite`, fill in five paths) collapse to "pick React, name it, pick a store". Keep the `code`-framework path documented separately for the generic case
- [ ] Update `docs/TECHNICAL_DESIGN.md` §13.3 to describe both frameworks and why there are two
- [ ] CHANGELOG entries as each phase lands

---

## Explicitly OUT of scope for this milestone

- S3 / object-store and git-remote file-store backends — the registry makes them additive; only `local` is registered now
- In-browser VS Code for the Web (§13.3's "ideally") — the file manager's text editor is a plain editor this milestone
- Next.js / SvelteKit / React Native frameworks — `react` is the one opinionated framework; the registry pattern makes the others additive
- `ui/form-runtime` (§12) proper — 1.6 may need a generic `ServerQuery` evaluator, but not the whole runtime
- Everything still listed as out of scope in [docs/TODO-mvp.md](./docs/TODO-mvp.md)
