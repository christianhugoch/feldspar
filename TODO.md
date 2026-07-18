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

**Placement (resolved):** the value type goes in `sc-files`, the persistence in `sc-catalog`.
`sc-files` cannot hold the persistence — it has no `Catalog`, and it cannot gain one because
`sc-catalog` already depends on *it*. `sc-catalog` is the lowest crate holding both the
`FileStore` trait and a `Catalog`, and it already owns the connected-store registry, so the
definition and the instance it produces live one crate apart at most.

- [x] `FileStoreDef` value type: id, name (the catalog key), backend name, backend config (`Attrs`), optional `min_role`, description. The *definition* of a store, distinct from the connected `dyn FileStore` instance it produces. `FileStoreDefId` (UUID) is the row identity, separate from the *name* `FileStoreId` references, so a store can be renamed without its row changing identity
- [x] `_sc_file_stores` table + bootstrap, following `_sc_applications` (`crates/sc-app/src/applications.rs`): name `UNIQUE` (it is the lookup key, as a subdomain is for an app), JSON `config`
- [x] `save_file_store` / `load_file_store` / `load_file_store_by_name` / `list_file_stores` / `delete_file_store`, following `crates/sc-app/src/store.rs` — including its strictness rule: a missing or ill-typed column is an `Error::invalid` naming the store and the column, not a silent default
- [x] Decide what a delete means. **Resolved: the row only, never the bytes** — a definition is a connection to data that exists independently of Saltcorn (often an admin's own directory, possibly a git working tree), so disconnecting is not consent to destroy. A delete is refused if anything still references the store, naming the referents. The check is **split across crates by necessity**: `sc-catalog` scans its own tables (`file_store_field_references`), and application references live in `sc-app` above it (`applications_using_file_store`, matching the store subset, static dirs and a framework's `store` setting), passed down as `extra_referents`. §1.4's delete endpoint is the one place that must compose both
- [x] Integration tests against a real Postgres: round-trip a definition, reject a duplicate name, reject a delete that would orphan a reference

**Gap found while doing this — affects §1.4/§1.5, and §1.6 more than expected.**
`DataFieldKind::File` is modelled (§6.2) but persisted **nowhere**: `create_table` reloads the
catalog from introspection and `Table::from_physical` derives only `Plain` or `Key` (from a
foreign key), so a column cannot say "I am a path in store `uploads`". That needs the
`_sc_fields` overlay, which §9 puts out of MVP scope. Consequences to carry forward:

- `file_store_field_references` is correct but **inert** today; only the application-level check
  actually protects a store. A passing delete is not proof nothing points at the store, and the
  admin UI must not claim otherwise in §1.5
- `file_field_references_are_inert_until_the_fields_overlay_exists` is written as a tripwire: it
  asserts the current (wrong-in-the-long-run) behaviour so that landing the overlay fails the
  test and forces the delete path to be revisited
- [ ] Decide whether a minimal `_sc_fields` overlay — enough to persist a `File` field's store,
  folder and MIME restrictions — belongs in this milestone. It is out of MVP scope by §9, but a
  `File` field is unusable without it, and file stores being editable makes that more visible

### 1.2 Backend registry (`sc-files`)

The same "settings as data" move that made "the admin picks a framework" work (§13.3) — the
admin UI must render a form for a backend it knows nothing about, including one arriving later
through `sc-code`, with no per-backend special case.

- [x] Backend registry keyed by backend name, mirroring `registered_frameworks` / `framework_config_spec`: `registered_backends()`, `backend_config_spec(name) -> Result<Vec<FormField>>` (`crates/sc-files/src/backend.rs`). **No `FileStoreBackend` trait**: a backend is a name, a spec and a constructor, all three of which the registry functions already provide — a trait would need an instance to ask for a spec that must be available *before* any instance exists, which is the same reason `code_config_spec` is a free function
- [x] `local` backend declares its settings: `path` (required) and `create` (default false — creating a directory on the server's filesystem is a side effect to opt into, and silently creating one turns a typo'd path into a new empty store instead of an error)
- [x] `validate_file_store_config` called **on save**, exactly as `validate_framework_config` is
- [x] Construct a connected `Arc<dyn FileStore>` from a `FileStoreDef` (`connect_from_def`) — the one place a definition becomes an instance. `sc-cli`'s `--file-store` flag now goes through it too, rather than building a `LocalFileStore` itself, so a flag-connected store cannot drift from a stored one
- [x] Unit tests (11 in `backend.rs`) + an integration test that a mis-configured store is rejected on save

**Deviation from this section's original wording, deliberate.** The item above said "an
unreadable path *or* a missing setting" should be rejected on save. Only the second is. The two
are different kinds of wrong and conflating them breaks §1.1's model:

- **Structurally wrong** (missing/ill-typed/unknown setting, unknown backend) is the admin's
  typo, knowable without touching the filesystem, and the same answer on every machine. It
  blocks the save
- **Currently unreachable** (directory unmounted, renamed, not yet created) is often not the
  admin's fault, can become true *after* a successful save, and can stop being true with no
  edit. It is reported by `connect_from_def`, never enforced at save

Enforcing reachability at save would make a store whose disk was unmounted **uneditable** —
the admin could not open it to fix the path, which is the one thing they need to do. So §1.4's
save endpoint should connect after saving and return the reachability result alongside the
saved store, giving immediate feedback without blocking the save; §1.5 shows it as the
"connected-or-error" state the store list already needs.

### 1.3 Live connect / disconnect (`sc-catalog`, `sc-server`)

The MVP already established the pattern for applications ("the mount registry is live", §13.2);
stores get the same treatment, because editing a store must not need a restart either.

- [x] `Catalog::disconnect_file_store`, so a deleted or renamed store stops resolving. `connect_file_store` already replaces on a repeated name, so re-pointing an edited store works today; removing one does not
- [x] Boot: load every `_sc_file_stores` row and connect each (`connect_all_file_stores`, called from the CLI boot path via `connect_stored_file_stores`, which logs each outcome). One store that fails is logged and skipped, never fatal — the same rule `mount_all` applies to an app whose build fails. Only failing to *read the table* is an error, since that means the metadata itself is unreachable. `connect_catalog` also bootstraps `_sc_file_stores` alongside `users` and `_sc_applications`
- [x] Report a store that failed to connect rather than dropping it silently: the catalog records the reason (`record_file_store_error` / `file_store_error`), cleared whenever the store connects or is disconnected so a stale reason is never shown as current. `FileStoreConnections { connected, failed }` is the boot-time report
- [x] Decide how `--file-store NAME=PATH` and persisted stores coexist. **Resolved as proposed**: ephemeral and unpersisted, connected *after* the stored ones, and a name clash is a startup error. Refusing is the point — `connect_file_store` replaces on a repeated name, so a clash would otherwise let the flag silently shadow a store configured in the UI, and the admin would edit a store, see it save, and watch the server keep serving a different directory with nothing saying why
- [x] Integration tests: `crates/sc-catalog/tests/file_store_live.rs` (5) — boot connects stored stores, one bad store leaves the others up and records why, an edit repoints without a restart, disconnect stops resolution while leaving the bytes alone, a repaired store loses its stale error; plus a CLI boot test for the bootstrap and the flag-clash rule

**Carried into §1.4:** two composition steps live above this layer and must not be forgotten,
because nothing at this layer can enforce them:

- [ ] **Renaming** a store must disconnect the *old* name. `save_file_store` deliberately does
  not touch the registry, so a rename otherwise leaves the old handle connected and serving. The
  update endpoint must load the previous row, and disconnect its name when it differs
- [ ] **Deleting** must disconnect after removing the row — proven separate by
  `disconnecting_stops_a_store_resolving`, where the row is gone and the handle still browses

### 1.4a Admin API — store configuration (`sc-api`, `sc-server`) ✅

Split from §1.4b so the security-sensitive access check does not land in the same review as
routine CRUD. This half is everything the §1.5 **file stores screen** needs.

- [x] Endpoints: create / update / delete file store, addressed by **id** (the row identity, which survives a rename) while the file manager stays addressed by **name** (what the admin picked and what everything else references)
- [x] Endpoint `listFileStoreBackends`, mirroring `listFrameworks`
- [x] Extend `listFileStores` beyond `{name, is_git_repo}`: id, description, backend, config, `min_role`, `connected`, `error`, `is_git_repo`
- [x] **Both composition steps carried from §1.3**: a rename disconnects the old name (otherwise the old handle keeps serving under a name with no definition), and a delete disconnects after removing the row. Create and update also connect immediately, so an admin learns now — not at next boot — whether the directory is reachable
- [x] Delete composes both halves of the reference check: `sc-catalog` sees `File` fields, this layer adds `applications_using_file_store`
- [x] Regenerate `ui/admin/src/client.ts` (`cargo run -p sc-api --example emit_admin_client -- ui/admin/src/client.ts`)
- [x] Integration tests: `crates/sc-server/tests/file_store_admin_api.rs` (8), end to end through the assembled router

**A design change forced by the work.** `listFileStores` was going to be driven by the stored
definitions, which would have made a `--file-store` store **invisible** — it is connected and
browsable but has no row, so a developer running with the flag would see an empty screen and no
way to reach the store they had just connected. The listing is now the **union** of defined and
connected stores, and `id` is nullable: a null id is exactly what tells the UI a store cannot be
edited or deleted, because there is no row behind it.

- [ ] §1.5 must render a null-id store as read-only, and not offer edit/delete for it

### 1.4b Admin API — file operations & access control (`sc-api`, `sc-server`) ✅

What the §1.5 **file manager screen** needs.

- [x] Extend the `FileStore` trait with `mkdir`, `delete`, `rename` plus the `LocalFileStore` implementations, then the `makeDirectory` / `deleteFile` / `renameFile` endpoints over them. Semantics chosen deliberately: `mkdir` is idempotent but refuses to shadow a file; `delete` reports whether anything was there (so a caller need not race an existence check) and refuses the store root; `rename` never overwrites — a silent replace on a file manager's drag-and-drop is a lost file with no undo
- [x] **Binary upload route** at `POST /upload/{store}/{*path}`, outside the typed `EndpointSet`. It dispatches *through the handler registry* (`uploadFile`) rather than owning its own catalog handle, so routing around the endpoint set does not also route around the catalog or the access checks. Body capped at 256 MB; `writeFile`'s base64 path stays for small files
- [x] `getFileMeta` / `setFileMeta`. They report `effective_min_role` (what actually applies, given the store floor and every parent) alongside `min_role` (what is set on this entry) — showing only the latter would let an admin believe a file is reachable when its folder has locked it
- [x] Enforce the path-cumulative `min_role` rule on read/browse/write/mkdir/delete/rename/meta/upload (`sc-files/src/access.rs`). A browse is *filtered* rather than refused, since listing names the caller cannot open leaks what the rule was set to hide. Nested rules only ever tighten, and the store-wide floor (§1.1) is simply the outermost entry on the path
- [x] Integration tests: `crates/sc-server/tests/file_operations_api.rs` (5) + 8 unit tests in `sc-files`

**Known gap, deliberately not papered over.** Every file endpoint is `AuthRequirement::admin()`,
so `caller_role` is always `1` and an admin clears every rule — the enforcement is therefore
**inert through the admin API today**. It is wired in because it is correct by construction and
starts working the moment a non-admin can reach a store, and because enforcement living only in
a future caller is the same mistake as the rule living only in a doc comment (which is what
`FileMeta::min_role` was until now). The rule itself is tested directly at the `sc-files` level,
where roles other than admin can be exercised.

- [ ] Revisit when application-facing file access exists: that is the first caller with a real
  non-admin role, and the first place this enforcement does observable work

### 1.5 Admin SPA (`ui/admin`) ✅

- [x] **File stores screen** (`screens/FileStores.tsx`): list with name, backend, a generic settings summary, connected-or-error status, and create/edit/browse/remove. Nav entry "Files" alongside Tables / Applications / Users
- [x] Create/edit form (`screens/FileStoreForm.tsx`): pick a backend → render its `config_spec`, no backend-specific code. The shared renderer was **factored out** into `src/settings.tsx` and `ApplicationForm` now uses it too, so the one settings vocabulary has one rendering
- [x] **File manager screen** (`screens/FileManager.tsx`): breadcrumb navigation, upload, download, new folder, rename, delete, and editing a text file in place — which is what `readFile`'s UTF-8 `text` shortcut was built for and nothing had ever called
- [x] Surface and let an admin edit a file's `min_role`, showing the **effective** role beside it, since a parent folder can be stricter and an admin reading only the entry's own rule would draw the wrong conclusion
- [x] A store that failed to connect is shown as such, with its error, and is still editable
- [x] Null-id (`--file-store`) stores render read-only, per the §1.4a note
- [x] Upload is the one hand-written client call (`api.ts`), since the route is outside the typed `EndpointSet`

**Verified by running the real server**, not only by tests: created a store through the API the
form posts to, drove mkdir/upload/write/browse/rename/delete/meta, confirmed uploaded bytes match
by sha256, saw all three list states (stored+connected, stored+unconnected with its reason, and
flag-connected with a null id), and completed the repair flow — editing a broken store's path
until it connects, then renaming it and watching the old name disconnect.

**A bug that found:** a failed connection reported only `connecting file store \`gone\``, with no
cause — `Display` on a context error shows just the outermost context. For a screen whose entire
purpose in that state is telling the admin what to fix, that is useless. Added
`sc_error::format_causes` (the whole chain on one line, no code locations — the user-facing
counterpart to `format_chain`, which is for logs) and used it where connection failures are
recorded and reported. The message now reads
`connecting file store \`gone\`: file store root /definitely/not/here: No such file or directory`.

- [ ] Consider auditing other admin-facing error paths for the same truncation — anywhere
  `e.to_string()` on a context error reaches the UI has this problem, and it is invisible until
  someone reads the message and finds it says nothing

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
