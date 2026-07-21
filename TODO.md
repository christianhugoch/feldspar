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
- [x] Decide whether a minimal `_sc_fields` overlay — enough to persist a `File` field's store,
  folder and MIME restrictions — belongs in this milestone. **Decided: no.** The deciding fact
  is that the overlay would be *necessary but not sufficient*: `createField` accepts only
  `name`/`sql_type`/`nullable`, so a field's **kind cannot be declared at all**, through the API
  or the UI. Storing a `File` kind would therefore still leave `File` fields unreachable, and
  making them reachable means field-kind editing in the API and the table editor, and then §6.3's
  fieldviews to render one. That is a feature area, not a Phase 1 loose end. It would also cost
  more than it looks: the overlay overturns the catalog's stated invariant — "no stored metadata
  beyond `information_schema`" — which is load-bearing for the zero-setup promise, and brings
  merge/precedence semantics, behaviour when a column is dropped underneath a row, and cache
  invalidation with it. Nothing in this milestone's definition of done needs it: the file-store
  screens and the file manager are complete without a single `File` field. Tracked below instead

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

- [x] **Renaming** a store must disconnect the *old* name — done in §1.4a's `updateFileStore`,
  which loads the previous row and disconnects its name when it differs
- [x] **Deleting** must disconnect after removing the row — done in §1.4a's `deleteFileStore`

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

- [x] §1.5 renders a null-id store as read-only (no edit/delete offered, labelled "from --file-store")

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

**Not actionable in this milestone** — it needs a caller that does not exist yet. Tracked in
"Carried past this milestone" below rather than left open here, since nothing in Phase 1 can
close it.

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

- [x] Audit the other admin-facing error paths for the same truncation. **One central site, now
  fixed**: `error_response` in `sc-server/src/router.rs` sent `err.to_string()` for *every*
  endpoint error, so the truncation was never specific to file stores. It could not simply be
  switched to the causal chain, because the same function serves the admin API **and** every
  application's API, and those have different readers — so it now takes an explicit `Audience`:
  an admin gets `err.causes()`, an application's own users get the top-level message, and the log
  keeps getting everything either way. Making it an argument rather than a default means a new
  route has to answer the question. No other production site formats an error into a response
  body

### 1.6 Follow-on: the framework `store` setting becomes a pick-list ✅

- [x] `OptionsSource` in `sc-types`: `None | Static | ServerQuery`. `FormField.options` became `options_source`, with `static_options()` / `query()` accessors. **A query is a name, not an expression** — settings-as-data only works if the data stays inert, and an embedded query language would make a spec executable, which is not something to run on behalf of a guest-language plugin
- [x] `ClientCode` deliberately **not** implemented: it is for options depending on other form values (a dependent dropdown), which cannot be pre-resolved and needs the form runtime. Nothing needs it, and this module's convention is to leave out what has no consumer
- [x] The `store` setting declares `ServerQuery(QUERY_FILE_STORES)`; `sc_catalog::resolve_options` answers it. §13.3 updated
- [x] **Resolved: the server evaluates, not the client.** The spec is resolved before it leaves the server, so the admin UI receives a concrete list and needs no evaluator — which is why this did not have to wait for `ui/form-runtime`, and why no evaluator gets built twice. The UI renders a select purely because `options` is non-empty; there is no store-specific code in any screen
- [x] Tests: an unknown store is rejected on save; a defined-but-unconnected store is still choosable; the pick-list arrives resolved over HTTP; unit tests for the spec and for unresolved queries not restricting

**Two design points the work forced, both worth remembering:**

- **Validation split in two.** `validate_framework_config(catalog, fw)` resolves queries then
  validates (save time); `validate_framework_config_structure(fw)` checks shape only (build
  time). Re-asking "does this store exist?" at build would make a build fail for a reason
  unrelated to building, and the honest error there is the one the build already gives. This
  falls out of the model rather than being bolted on: an unresolved query has no static options,
  and `validate_attrs` only checks membership against options it has
- **The pick-list is defined stores ∪ connected stores, not connected only.** If it were
  connected-only, an unmounted disk would block editing every application using that store —
  including to repair it. A defined-but-unreachable store stays a valid choice, which is the same
  principle §1.2 established for saving one

**A coupling caught and removed.** `resolve_options` reaching for `_sc_file_stores`
unconditionally made `listFrameworks` return 500 when that table was absent — an endpoint
failing over a table it has no visible relationship to. `choosable_file_stores` now asks the
catalog whether the table exists (a cache lookup) and treats its absence as "no stored
definitions", which is what absence *means*, while a genuine database error still propagates
rather than being reported as "no stores exist".

---

## Phase 2 — An opinionated React framework

`CodeFramework` is the right shape for "any bundler, any layout" and the wrong shape for the
common case: five settings, all required to be mutually consistent, plus a hand-scaffolded
project the admin must create over SSH before the settings mean anything. The `react` framework
inverts that — conventions instead of settings, and the server creates the project.

### 2.1 Decide the opinions ✅

These are the choices that stop being the admin's. Each needed deciding **before** 2.2, since the
scaffold hard-codes them; recording the reasoning here matters as much as the choice, because
"opinionated" only pays off if the opinions are defensible and stable. **This item produced no
code** — it is the decision record 2.2 and 2.3 implement against, so it has no test of its own;
the tests it implies are the ones listed under those items.

- [x] **Build tooling: Vite + React + TypeScript, no SSR.** The presumptive answer survived: it
  is what the tutorial and `ui/admin` already use, `npm run build` → `dist` is the convention
  every derived setting is read off, and its output is exactly what the serving path wants — real
  `<script type="module" src=…>` and `<link rel=stylesheet>` files, no inline script, so a strict
  CSP needs no exception. SSR is ruled out for the same reason and not merely unchosen: §13.3
  serves *bundled assets*, and server-rendering would put a Node process in the request path of
  every application, which is a different serving model, not a different setting
- [x] **Routing: `react-router` with the routes declared as data in one `src/routes.tsx`** — not
  the file-system convention. File-system routing needs a build-time plugin that scans directories
  and generates the route module; that is a second convention the scaffold must own, that the
  admin must debug through, and it earns nothing here because the scaffold *generates the route
  list anyway* — it knows the app's tables. A plain array is inspectable, editable, diffable, and
  is where the per-route `public` flag (below) can live. Deep links keep working as they already
  do, through `CodeFramework`'s SPA fallback
- [x] **Data layer: the generated client *plus* generated typed hooks over it, and no
  data-fetching dependency.** The tutorial's step 5 — `useEffect` + `useState` + a hand-written
  `refresh()` — is precisely the boilerplate this framework exists to delete, so `useRows`,
  `useRow`, `useCreate`, `useUpdate`, `useDelete` ship. They are **generated alongside the client
  from the same `EndpointSet`**, so they are typed per table (`useRows.tasks()`), not
  stringly-typed over a generic one. TanStack Query was considered and rejected: it would add a
  dependency and a second mental model (query keys, invalidation strategy) to describe a cache
  whose keys the framework already knows exactly — one entry per table, invalidated by table name
  on mutation. An app that wants it can drop the hooks and use the client, which is unchanged
- [x] **Auth: authenticated by default; `public: true` is one word per route.** The scaffold ships
  an `<AuthProvider>`, a `useUser()` hook and a login screen against the app's own `/api/login` /
  `/api/logout` / `/api/whoami` (session cookie; the endpoints already exist per app). Routes
  require a user unless their entry in the route list says otherwise. Defaulting the other way
  would make *forgetting* to mark a route produce an open door instead of a locked one, and it
  would mostly produce blank screens anyway, since table access is role-gated server-side.
  **The client's auth state is a UI convenience and never the enforcement point** — every request
  is authorized again by §7 — which is what makes a wrong `public` flag a cosmetic bug rather than
  a hole
- [x] **Styling: plain CSS with custom properties for the few tokens the scaffold uses, wholly
  replaceable.** No CSS framework, no CSS-in-JS. The scaffold writes `src/app.css` once and never
  regenerates it, and nothing in the runtime imports it, so deleting it and installing Tailwind is
  an ordinary thing to do rather than a fight with the generator. Two reasons beyond taste: a CSS
  framework is a large dependency on its own version treadmill and usually its own build plugin,
  and the value on offer here is the data/auth/build path, not the look; and CSS-in-JS injects
  `<style>` at runtime, which would force `style-src 'unsafe-inline'` into the default CSP of
  every scaffolded app
- [x] **Where the runtime lives: generated into the project at `src/saltcorn/`, not an npm
  package.** This was billed as the irreversible decision, and the argument that settles it is not
  upgradability — it is that **the runtime is app-shaped**. The hooks worth having are typed per
  table, which means generated from this app's `EndpointSet`, exactly as the client already is. A
  registry package cannot contain them; it could only offer generic untyped hooks, discarding the
  one property that motivated a hooks layer at all. So the "vendored source is instantly stale"
  objection dissolves — this is not a vendored snapshot but generated output, refreshed on every
  build like `client.ts`, and a server upgrade cannot leave a client runtime pinned behind it. The
  cost is accepted honestly: `src/saltcorn/**` is **generated and overwritten**, carries a header
  saying so, and is therefore not hackable in place. Everything outside it is the admin's and is
  never touched. The escape hatch from a runtime you dislike is to stop importing it, not to edit
  it
- [x] **Naming conventions** (what 2.2 derives instead of asking): an app named `todo` has source
  `todo/`, output `todo/dist`, build command `npm run build`, generated client and runtime under
  `todo/src/saltcorn/`. Five settings collapse to two — the store and the name
- [x] Recorded in `docs/TECHNICAL_DESIGN.md` §13.3, next to the code-framework description, as
  "the two frameworks and why there are two"

**The through-line, worth keeping when these are revisited:** every one of these decisions is
either *derivable from the app's own schema* (routes, hooks, client) or *a dependency not taken*
(router aside: no data library, no CSS framework, no CSS-in-JS, no SSR runtime). What is generated
can be regenerated and needs no version negotiation with the server; what is not depended on
cannot go stale. That is what makes the opinions safe to hard-code, and it is the test to apply to
the next one: if an opinion can only be honoured by a package the admin must keep in step with the
server, it is the wrong opinion.

### 2.2 The framework (`sc-app`) ✅

- [x] Registered in `registered_frameworks` / `framework_config_spec`. **The list is ordered,
  and that is the only editorial statement the registry makes**: `react` first because it is
  the path an admin should take, `code` second as the escape hatch. §2.4 renders that; the
  registry is where it starts. A test asserts the order and that every listed name resolves to
  a labelled spec, so the list and the lookup cannot drift
- [x] `react_config_spec`: `store` (the same server-resolved pick-list `code` uses) and
  `project`. Nothing else. `project` is the framework's setting rather than the app's `name`
  because it is a directory on disk while `name` is a renameable display string — and because
  it is the only thing `app_source_from_config` is handed
- [x] Conventions in `sc-app/src/react.rs`, deliberately **pure functions of the project
  name** with no I/O, so §2.3's scaffold reads its paths from here rather than restating them:
  source `todo`, output `todo/dist`, `npm run build`, client `todo/src/saltcorn/client.ts`.
  The client is not optional as it is for `code` — the scaffold imports it, so an app that did
  not emit one would not compile
- [x] `app_source_from_config` now dispatches on the framework name and both arms produce the
  same `AppSource`; a test asserts a `react` config and the equivalent hand-written `code`
  config resolve to an identical `BuildSpec`. **That is why `build_app`, `build_application`,
  `build_and_mount` and the mount registry needed no change at all**
- [x] Serving reused, not reimplemented. The one thing this exposed: `CodeFramework::config_spec`
  hard-coded `code_config_spec()`, so a mounted `react` app would have reported `code`'s five
  settings. It now looks the spec up by its own name, and the instance agrees with the registry
  for both
- [x] CSP: `react_csp()`, and `framework_default_csp(name)` applied where an app states no
  policy of its own. Strict baseline plus exactly what a Vite bundle needs (`data:` images and
  fonts, because Vite inlines small assets), the app's own origin for `connect-src`, and
  `object-src`/`base-uri`/`frame-ancestors` tightened. **No `unsafe-inline` or `unsafe-eval`
  anywhere** — which is not luck but the 2.1 tooling and styling decisions paying off, and a
  test asserts it so a future dependency cannot quietly need one
- [x] Tests: the spec is two labelled required settings; every path derives from the project
  name and stays inside the project directory; a project name is a plain identifier (traversal,
  separators, spaces, leading dots rejected); a `react` config resolves to the derived
  `BuildSpec` and matches the `code` equivalent; a stated `code` setting on a `react` app is
  refused rather than ignored; the conventional output directory serves with SPA deep links;
  the default CSP carries no unsafe source; and over HTTP — React offered first, two settings
  with the store pre-resolved, the react CSP applied when none is stated and overridden when
  one is, a traversal project name refused on save naming the setting

**Where the project name is checked, and why it moved.** The first cut checked it in
`react_source_from_config`, at build. That is too late by §1.6's own argument: the check now
lives in `validate_framework_config`, so an unusable name is refused **on save**, where the
admin is still looking at the form. It could not be expressed in the spec itself — §6.2 states
presence, type and membership, not patterns — and growing the vocabulary for one setting would
oblige every guest-language framework to be understood by it, so the framework checks its own
(`framework_specific_checks`). The build path inherits it through the structural validation it
already ran.

**One test is honestly weaker than it looks.** "A `react` app serves its bundle and resolves
deep links" runs against the *derived* source and output directories but with `npm` stubbed by
a shell script, because a Node toolchain in the Rust test suite would make every build test
need one. The real `npm run build` is exercised by §2.3's scaffold → build → serve integration
test, which is the first point there is a project to run it on.

### 2.3 Scaffolding (`sc-app`) ✅

The step that removes the SSH requirement, and the reason this is more than a settings preset.

- [x] Generated **on first save** (the create handler), not behind an explicit action: the
  admin filled in two fields in a browser and a complete Vite project exists on the server.
  14 files for a one-table app — `package.json`, `vite.config.ts`, `tsconfig.json`,
  `index.html`, `.gitignore`, entry point, app shell, login screen, `auth.tsx`, the route
  list, a stylesheet, one page per table, and the two-file runtime. **Scaffolding does not
  fail the create**: the row is already saved and valid, and an occupied directory is
  something the admin fixes and re-tries, not a reason to lose the app they just configured —
  so the response carries `scaffolded` or `scaffold_error` alongside the created application
- [x] Generated against the app's *actual* tables: a page per table with its real columns, a
  create form (minus the key), and hooks typed from the schema (`TasksRow`, `done?: boolean |
  null`). The generator (`scaffold/files.rs`) is **pure** — tables and an `EndpointSet` in,
  file contents out — so all of this is assertable without a store or a database
- [x] The client lands at the conventional path the scaffold's own imports point at, so the
  two halves of the runtime meet with no setting to get wrong
- [x] `git init` when the store is **not** already a repo; when it is, the project is inside
  it and a nested repo would be worse than none. A missing or failing `git` is reported, not
  fatal — refusing to keep a project that was written correctly because version control was
  unavailable is the wrong trade
- [x] Never overwrites: refused before a byte is written, naming the directory. Tested by
  putting a file in the way and asserting it is byte-identical afterwards. Re-scaffolding does
  not exist, deliberately
- [x] `npm install` runs as part of the build when `node_modules` is absent. Carried on the
  `BuildSpec` as an `InstallSpec { command, args, marker }` rather than assumed by the build
  step, so `run_build` stays framework-agnostic and a `code` app — whose dependencies are the
  admin's business — is unaffected. The marker is **checked on disk**, not remembered, so a
  store restored from a backup installs again
- [x] Install and scaffold failures are Application errors carrying the tool's own output
  (§16). The install log is surfaced separately from the bundler's in the build banner,
  because the first build of a scaffolded app is mostly the install
- [x] Integration tests (`sc-app/tests/scaffold_app.rs`): the project is written against real
  tables with git initialised; an occupied directory is refused and nothing is touched; a
  git-repo store gets no nested repo; the runtime is regenerated when a table is added while
  the admin's own files are left alone. Over HTTP: creating a React app scaffolds it, and a
  second app pointed at the same directory is created but reports why nothing was generated

**The end-to-end test is real, and opt-in.** `SC_TEST_NPM=1` runs scaffold → `npm install` →
`tsc --noEmit && vite build` → serve, against a real Node toolchain. It is the only thing that
proves a scaffolded project *builds*, and because the build script type-checks, it is also
what closes "a scaffolded app's generated client compiles against its own endpoints" — a drift
between what `sc-api` emits and what the generated hooks call fails there. It is gated because
it needs npm and a network, which the rest of the Rust suite deliberately does not; everything
else in that file runs unconditionally. **Verified passing locally** (react 19, vite 8,
react-router 7).

**One coupling removed rather than duplicated.** The hooks call `listTasks`/`createTasks`/… by
name, and those names come from `sc-api`'s `op_name`, which was private. Recomputing the
convention in the scaffold would have been a second copy of it, free to drift from the
endpoints the client is generated with; `op_name` is now public and the scaffold uses it.

### 2.4 Admin SPA (`ui/admin`) ✅

- [x] The picker is a radio card per framework, each with a name and a sentence, in registry
  order. **The editorial content comes from the server** (`FrameworkInfo { name, label,
  description }`, surfaced by `listFrameworks`), which is what makes this possible *without*
  the screen knowing which framework is which — the alternative, special-casing the name
  `react` in the SPA, would undo §13.3's arrangement the moment a third framework or a
  guest-language one arrived. A new application starts on the first framework offered, so
  "React is the default path" is the registry's ordering rather than a default in the form
- [x] Short form vs five settings needed **no code at all**: the spec is the branch. A test
  asserts the two specs differ in length and that both arrive labelled
- [x] Scaffold outcome surfaced in the existing build banner. The scaffold happens on the
  *form*, which navigates away immediately, so a one-shot `notice.ts` hands the message to the
  list — one banner for both, because to an admin a scaffold and a build are the same kind of
  news about the same app. A refused scaffold is shown as a failure on an application that
  was nonetheless created, which is exactly what the server reports
- [x] The app row links into the file manager at the app's source directory. **Where that is
  comes from the server** as a derived `source: { store, path }` on the application JSON: a
  `code` app states it in five settings and a `react` app derives it from one, and the screen
  should know neither. The files route grew an optional directory (`/files/<store>/<dir>`)

**One thing that had to change to make the framework CSP default reachable.** The form
pre-filled the CSP box with `default-src: 'self'` and always sent it — so every app, React
included, overrode its framework's default policy with the baseline (§2.2) and no one would
have noticed. The box now starts empty, means "no opinion", and is omitted from the request
when blank. Verified against a running server: a React app created from the form comes back
with the full react policy.

**Verified live, not only in tests.** Against a real server: two fields in the create form →
14 files scaffolded → Build → `npm install` + `tsc` + `vite build` in ~3s → the app serving on
`todo.localhost` with its React CSP, deep links resolving, and its API refusing an anonymous
caller. That is the §2.5 tutorial's whole path, minus the SSH session.

### 2.5 Documentation

- [ ] Rewrite `docs/tutorial-react-todo.md` for the React framework: steps 2 and 3 (SSH in, `npm create vite`, fill in five paths) collapse to "pick React, name it, pick a store". Keep the `code`-framework path documented separately for the generic case
- [ ] Update `docs/TECHNICAL_DESIGN.md` §13.3 to describe both frameworks and why there are two
- [ ] CHANGELOG entries as each phase lands

---

## Carried past this milestone

Decided, not forgotten. Each of these came out of Phase 1 with a reason it cannot or should not
close here, and each has something in the tree that will make it noisy again when its time comes.

- **A `_sc_fields` overlay, so `DataFieldKind::File` can actually be stored.** Decided against for
  this milestone (§1.1): a field's kind cannot be declared through the API at all, so the overlay
  alone would not make `File` fields reachable, and the overlay overturns the catalog's
  "no stored metadata beyond `information_schema`" invariant. Whoever picks this up should do it
  as the front of the fields/fieldviews work (§6.2/§6.3), not as a storage patch. The tripwire
  test `file_field_references_are_inert_until_the_fields_overlay_exists` fails the moment the
  overlay lands, which is the signal to re-enable the catalog-level half of the file-store delete
  check (`file_store_field_references`, correct but inert today).
- **Path-cumulative `min_role` enforcement doing observable work.** Implemented and unit-tested in
  `sc-files`, and wired into every file endpoint — but every one of those is admin-only, so the
  caller's role is always `1` and clears every rule (§1.4b). The first real exercise is
  application-facing file access, which does not exist yet. Nothing in this milestone can close it;
  it is here so it is not mistaken for finished when that caller arrives.
- **`OptionsSource::ClientCode`** — options that depend on other values in the form, which cannot
  be pre-resolved server-side (§1.6). It arrives with `ui/form-runtime` (§12), which is the only
  thing that could evaluate it.

## Explicitly OUT of scope for this milestone

- S3 / object-store and git-remote file-store backends — the registry makes them additive; only `local` is registered now
- In-browser VS Code for the Web (§13.3's "ideally") — the file manager's text editor is a plain editor this milestone
- Next.js / SvelteKit / React Native frameworks — `react` is the one opinionated framework; the registry pattern makes the others additive
- `ui/form-runtime` (§12) proper — §1.6 resolved server queries server-side instead, so no evaluator was needed
- Everything still listed as out of scope in [docs/TODO-mvp.md](./docs/TODO-mvp.md)
