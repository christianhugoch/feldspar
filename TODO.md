# Saltcorn v2 — Saltcorn UI: the v1 views, running

Ordered, checkable task list for the twenty-fourth milestone after the MVP. Earlier lists are
archived in [docs/TODO-mvp.md](./docs/TODO-mvp.md) (the MVP),
[docs/TODO-post-mvp-1.md](./docs/TODO-post-mvp-1.md) (file stores + the React framework),
[docs/TODO-post-mvp-2.md](./docs/TODO-post-mvp-2.md) (the `_sc_tables`/`_sc_fields` overlays,
rich types and File fields), [docs/TODO-post-mvp-3.md](./docs/TODO-post-mvp-3.md) (ownership
formulae, calculated fields and row-level security),
[docs/TODO-post-mvp-4.md](./docs/TODO-post-mvp-4.md) (actions and triggers),
[docs/TODO-post-mvp-5.md](./docs/TODO-post-mvp-5.md) (the file-store IDE),
[docs/TODO-post-mvp-6.md](./docs/TODO-post-mvp-6.md) (agents),
[docs/TODO-post-mvp-7.md](./docs/TODO-post-mvp-7.md) (the GraphQL provider),
[docs/TODO-post-mvp-8.md](./docs/TODO-post-mvp-8.md) (REST queries, custom SQL and the
generated client), [docs/TODO-post-mvp-9.md](./docs/TODO-post-mvp-9.md) (table constraints
and indexes), [docs/TODO-post-mvp-10.md](./docs/TODO-post-mvp-10.md) (email),
[docs/TODO-post-mvp-11.md](./docs/TODO-post-mvp-11.md) (tables in code),
[docs/TODO-post-mvp-12.md](./docs/TODO-post-mvp-12.md) (concurrent code bodies),
[docs/TODO-post-mvp-13.md](./docs/TODO-post-mvp-13.md) (modules),
[docs/TODO-post-mvp-14.md](./docs/TODO-post-mvp-14.md) (SQLite),
[docs/TODO-post-mvp-15.md](./docs/TODO-post-mvp-15.md) (modules in-process),
[docs/TODO-post-mvp-16.md](./docs/TODO-post-mvp-16.md) (table providers),
[docs/TODO-post-mvp-17.md](./docs/TODO-post-mvp-17.md) (writable table providers),
[docs/TODO-post-mvp-18.md](./docs/TODO-post-mvp-18.md) (workflows),
[docs/TODO-post-mvp-19.md](./docs/TODO-post-mvp-19.md) (the Python code adapter),
[docs/TODO-post-mvp-20.md](./docs/TODO-post-mvp-20.md) (the administration MCP server),
[docs/TODO-post-mvp-21.md](./docs/TODO-post-mvp-21.md) (bundled modules),
[docs/TODO-post-mvp-22.md](./docs/TODO-post-mvp-22.md) (predictive models) and
[docs/TODO-post-mvp-23.md](./docs/TODO-post-mvp-23.md) (the v1 `Table` API).
Scope and rationale remain in [docs/GOALS.md](./docs/GOALS.md) and
[docs/TECHNICAL_DESIGN.md](./docs/TECHNICAL_DESIGN.md) (**§13.3**'s third framework — "the
drag-and-drop views/pages experience … using `sc-viewpattern` + `ui/builder`" — and **§18.5**,
the open question this milestone answers).

Every application this server can serve today is one somebody **writes**: a React project, a
Vue project, a tree of source in a file store with a build step. GOALS asks for the other
one — "a drag and drop building experience like saltcorn v1", "applications can also be built
in the saltcorn1 experience which will continue to improve" — and nothing of it exists here.
A restored Saltcorn 1 backup makes the gap concrete and unambiguous: the tables arrive, the
rows arrive, the triggers arrive, and then seven views and a page are dropped on the floor
with a line in the report saying Saltcorn 1 describes them against a UI this system does not
have.

This milestone gives it that UI. **Saltcorn UI** is a fourth framework beside `react`, `code`
and `vue`. An application whose framework is `saltcorn-ui` owns a set of **views** — each a
view pattern (v1 calls them view templates) configured over one table — and a set of
**pages**, and the server renders them. The view patterns are **v1's own source**: `list.ts`,
`show.ts`, `edit.ts`, `feed.ts`, `filter.ts` and `listshowlist.ts` out of
`saltcorn-data/base-plugin/viewtemplates`, with `viewable_fields.ts`, the fieldviews in
`base-plugin/types.ts`, the key fieldviews in `base-plugin/fieldviews.ts`, the file
fieldviews in `base-plugin/fileviews.ts` and `@saltcorn/markup`, vendored and run on the
Deno worker this process already has. Eight years of view behaviour is not something to
reimplement from the outside; it is something to **host**.

**Milestone definition of done:** an admin restores `saltcorn-v1-BooksDB.zip`. The restore
report now says seven views and one page were imported, and the Applications list has a new
application — *BooksDB*, framework **Saltcorn UI** — with the four imported tables in its
subset and Views and Pages tabs listing what came in. They open its subdomain, are asked to
sign in because every imported view is `min_role` 1, sign in, and land on
`/page/BooksOverview`: the *Filter books* view, with a publisher dropdown, a page-count range
slider, and *List Books* under it showing five books with their authors joined in. Changing
the dropdown re-runs the list. *Show* and *Edit* links work; Edit saves and comes back;
*Delete* deletes; *Add row* creates. Nothing was built, no bundler ran, and the HTML came out
of v1's `list.ts` unmodified.

**Not in this milestone:** the **builder**. Creating a view and editing the parts of its
configuration that are *not* a layout — the table, the pattern, the columns, the options, the
default state — is here (Phase 10), because those are `configuration_workflow` steps and this
server already renders a `FormField` list. The Craft.js drag-and-drop canvas that edits the
*layout* step is `ui/builder`, and it is the next milestone; until it lands a layout is shown
as read-only JSON and an imported view keeps the layout it came with. Also out: the `room`
and `workflow-room` patterns and everything realtime, page groups, the library, tags, file
**upload** from an Edit view, themes as plugins, and i18n beyond the identity translation.
Each is named under *Explicitly OUT* with what it would take.

**View patterns from an installed plugin are *in*** (§6, Phase 11), and they are in because
leaving them out shapes the wrong thing: `@saltcorn/kanban` requires `@saltcorn/markup/tags`
and `@saltcorn/data/plugin-helper` the way the six built-in patterns do, so a vendored bundle
that only the built-ins can see would have to be taken apart again the first time anybody
installed one. Kanban installing and rendering is the test that what was built is a facility
rather than six hard-coded views. `@saltcorn/mind-map` is the honest counter-example and
stays broken on purpose: it writes raw SQL through v1's `db`, which this server does not
have (§6).

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

---

# The specification

### 1. A view belongs to an application, which is the one real departure from v1

In Saltcorn 1 a view is global: one tenant, one view list, one menu. Here the unit of
multi-tenancy is the **application** (§13.2) — several share one data layer, each seeing only
its declared subset of tables and file stores — so a view that were global would be a view
reachable from every app's subdomain, on tables half of them cannot see.

So `_fd_views.application` and `_fd_pages.application` are columns, name is unique **per
application**, and a view may only name a table in its application's subset. Everything else
about a view is v1's: the pattern name, the `min_role`, the slug, and a `configuration` JSON
blob that is **v1-shaped and deliberately untouched** — it is what v1's own `list.ts` reads,
and translating it on the way in would be this server inventing a second configuration format
it would then have to keep in step with a file it does not own.

Two consequences worth stating now because they shape the rest. A Saltcorn UI application has
**no build step**: `Framework::build` answers `None`, saving a view is the whole deployment,
and the app's "source" is rows rather than a file store. And two Saltcorn UI applications on
one server may hold two views with the same name over the same table with different
configurations, which is the point of the column.

### 2. The view patterns are v1's source, vendored — not reimplemented, not depended on

`list.ts` is 2 170 lines, `edit.ts` 2 670, `viewable_fields.ts` 2 917. That is not volume for
its own sake; it is where a decade of "what should a list do when the join field is null and
the column is a formula and the user's role cannot write" lives. Reimplementing it in Rust
would produce a Saltcorn that renders *nearly* the same, and every difference would be a bug
report from somebody whose app used to work.

**Vendored, not a dependency.** The files are copied into `ui/saltcorn-ui/vendor/` with a
header per file naming the upstream path and the v1 version they were taken at, and a README
stating how to refresh them. Taking `@saltcorn/data` as an npm dependency instead is not an
option and it is worth being precise about why: `saltcorn-data`'s `models/table.js` *is* v1's
data layer — it opens its own `pg` pool, reads `_sc_tables`, and knows about tenants — and it
is exactly the file this server replaces with `sc-expr`'s `v1_api.js`. A dependency would drag
the whole of v1's server in behind six view files.

What is vendored is therefore drawn along one line: **everything that renders**, and nothing
that reaches a database, a tenant or a socket. The six patterns, `plugin-helper.ts`,
`viewable_fields.ts`, `base-plugin/types.ts` / `fieldviews.ts` / `fileviews.ts`,
`models/form.ts`, `models/fieldrepeat.ts`, `models/expression.ts`, and the whole of
`@saltcorn/markup`. Everything on the other side of the line is a **host shim** (§5).

`plugin-helper.ts` is the awkward one and the important one: 3 655 lines, imported by every
single base pattern *and* by every third-party pattern (§6), and not cleanly on one side of
the line — `stateFieldsToWhere`, `readState`, `calcfldViewOptions` and `link_view` render,
while `generate_joined_query`, `json_list_to_external_table` and `build_schema_data` reach
v1's `db`. It is vendored **per export**: what renders is kept, what queries is replaced by
the refusal that names it, and the split is a list in one file with a test that walks the
module's exports and asserts every one of them is on exactly one side of it.

### 3. Where it runs: the worker this process already has

`sc-module` runs v1 plugins on a `deno_runtime` worker in this process, with a `Module._load`
patch that answers `@saltcorn/*` itself, a call table, a timeout, a heap cap and a permission
set. The view runtime needs every one of those and needs nothing else, so it gets them by
being **a module** — a reserved built-in one, `@feldspar/saltcorn-ui`, loaded from the
`ui/saltcorn-ui` bundle directory instead of from the modules root, with an **empty**
permission set (no net, no fs, no env: it reaches data only through the host surfaces) and a
name no installed module may claim.

Not a second runtime, and not a Rust renderer, for the reason §1 of the previous milestone
gave about `Table`: two implementations of one thing disagree by the third bug fixed in one of
them. It is also the answer to the obvious alternative of shelling out to Node — this server
stopped requiring a `node` binary at run time in milestone 15, and a framework that brought
the requirement back would undo that for every deployment that serves one app.

**An embedded view does not cross the seam.** v1 views embed views (a Filter embeds a List; a
page embeds a Filter), and the worker holds the whole view set (§4), so `view.run()` inside
`filter.ts` recurses **inside the worker**. Only data crosses. A depth cap (16) makes a view
that embeds itself an error that names the cycle instead of a worker that stops answering.

### 4. The snapshot rule, extended from metadata to views

The previous milestone's central decision was that v1's `Table.findOne` is **synchronous**, so
metadata is a snapshot handed to the guest and data is a host call. v1's `View.findOne` is
synchronous too, and `getState().getConfig(...)` and `getState().types` are properties. The
same rule extends:

- **`SchemaSnapshot`** (already built, `sc_api::code_host::schema`) answers `Table` and
  `Field`.
- **`ViewSnapshot`** (new) answers `View`, `Page` and `getState()`: the application's id, name
  and base URL; its menu; its role list; its config values; and every view and page in it with
  its pattern, table, configuration, `min_role`, slug and attributes.

Both are cached on the worker behind a **generation** stamp and re-sent only when it moves —
one integer comparison per render, not a megabyte of JSON. The view snapshot's generation is
bumped by a view or page write, by an application save, and by `SIGHUP`.

The registries a `getState()` exposes — `types`, `keyFieldviews`, `fileviews`, `viewtemplates`
— are **not** in the snapshot: they are the vendored bundle's own objects, they never change
while a worker lives, and serialising a fieldview's `run` function is not a thing that can be
done.

### 5. The bundle is a library, not a private bundle

The rule is the one §13.3 already states for frameworks a module declares — *a module does
not write what this server has an answer for* — applied one level down:

| v1 import | Answered by | Why |
|---|---|---|
| `@saltcorn/markup`, `markup/tags`, `layout`, `helpers`, `layout_utils`, `form` | the bundle | pure rendering, no server state |
| `models/form.js`, `models/fieldrepeat.js`, `models/expression.js` | the bundle | pure; `Form` **replaces** `sc-module`'s stub, so there is one `Form` |
| `plugin-helper.js` | the bundle, per export (§2) | the rendering half kept, the querying half refused by name |
| `viewable_fields.js`, `base-plugin/types.js`, `fieldviews.js`, `fileviews.js` | the bundle | the fieldview set is what this milestone is importing |
| `models/table.js`, `models/field.js` | the host | `sc_expr::V1_API_JS`, already built |
| `models/view.js`, `models/page.js` | the host | the snapshot, and this server's view store |
| `db/state.js` (`getState`) | the host | the application, not a tenant (§7) |
| `models/trigger.js` | the host | this server's triggers, over the existing trigger surface |
| `models/file.js`, `models/user.js` | the host | the file stores and `sc-auth` |
| `models/crash.js` | the host | this server's error log |
| `db/index.js` | the host | its pure helpers (`sqlsanitize`, `slugify`) and a `withTransaction` that opens none (Phase 6); every query refused by name — there is no v1 `db` here (milestone 23) |
| `models/library.js`, `page_group.js`, `workflow.js` | the host | inert stubs; see *Explicitly OUT* |

**Both columns are reachable by `require`, from any module on the worker.** The bundle is
esbuilt with the host specifiers external, and `module-host.mjs`'s existing `Module._load`
patch — which already answers `@saltcorn/data/models/table` with `v1_api.js`'s `Table` —
resolves the left column into the bundle's exports instead of into a stub namespace. This is
what makes §6 possible, and it is worth stating as the design rather than as a side effect:
the vendored code is **the v1 compatibility library this server ships**, one copy, reached
the way v1 reaches it, and the Saltcorn UI patterns are simply its first consumer. A private
bundle that only the six built-in patterns could see would have to be un-privatised the first
time anybody installed a plugin, and the un-privatising is the whole of the work.

A shim the host supplies and a member it does not implement go on **one refusal list**, the
one `v1_api.js` already owns: reachable as a property, fatal on call, naming the path. A view
pattern that reaches for something this server has not got fails with a sentence an admin can
read, not with `undefined` rendered into a page.

**With one exception, and it is not a small one: the absent tier.** v1 plugins feature-detect,
because they are written against eight years of v1 versions:

```js
const public_user_role = features?.public_user_role || 10;     // @saltcorn/kanban
const results = runCollabEvents ? await runCollabEvents(…) : []; // @saltcorn/kanban
```

Against a throwing stub both of these are *worse* than against nothing: the stub is truthy,
so `public_user_role` becomes a function and `runCollabEvents` is called and throws. A
refusal that a plugin cannot detect is a refusal that turns a graceful degradation into a
crash. So the compatibility library has a **third** tier beside implemented and refused —
**absent**: a named list of v1 exports that answer `undefined`, because `undefined` is the
answer the plugin is testing for. It is a list, not a default: an unknown name still refuses,
and a name moves onto the absent list only with the feature-detection idiom that justifies it
written beside it.

### 6. A view pattern may come from an installed plugin, and that is the test of §5

`@saltcorn/kanban` and `@saltcorn/mind-map` are ordinary v1 npm plugins whose whole content
is `viewtemplates: [ … ]`. They are the reason §5 is written the way it is: a Kanban view
pattern is a view pattern by exactly the same definition the six built-in ones satisfy, and
if installing one does not work then what this milestone built is not a view-pattern
facility, it is six hard-coded views with a registry-shaped comment over them.

So `viewtemplates` becomes a **module facility key**, beside `actions`, `table_providers`,
`modelproviders` and `frameworks`, with the same rules: unqualified names in one namespace, a
name another module (or a built-in pattern) already has loses *that pattern* with the reason
on the module's card and keeps the rest, and the registry is installed whole on every module
change. Reading the two plugins tells us precisely what else it takes, and each item is a
thing the built-in patterns would have wanted anyway:

- **The compatibility library has to be a library** (§5). Both plugins open with
  `require("@saltcorn/markup/tags")`, `require("@saltcorn/data/plugin-helper")` and
  `require("@saltcorn/data/models/fieldrepeat")`. Today those resolve to throwing stub
  namespaces, so `div(…)` throws on the first render. They must resolve into the vendored
  bundle's real exports. This is the single largest reason not to esbuild the vendored code
  into a private bundle that only the six built-ins can see.
- **The absent tier** (§5). `features?.public_user_role || 10` and
  `runCollabEvents ? await runCollabEvents(…) : []` are both from `@saltcorn/kanban`, and
  both are *feature detection*. A throwing stub is truthy and turns each into a crash.
- **A plugin ships assets, and declares headers.** Both export
  `headers: [{ script: "/plugins/public/<name>@<version>/dragula.min.js", onlyViews: [...] }]`
  and a `public/` directory in the package. So: `headers` is a manifest key that crosses at
  load (it is data), the app serves `/plugins/public/<name>@<version>/*` out of the installed
  package's `public/`, and the document builder (§9) injects the headers of every module
  whose `onlyViews` matches the pattern being rendered, or that has no `onlyViews`.
- **A pattern's configuration steps are a call, not load-time data.** `sc-module`'s existing
  `workflowFields` flattens a `configuration_workflow` by evaluating each step's `form({})`
  once, at load. That is right for a module's own settings and wrong here: Kanban's first
  step does `Table.findOne({ id: context.table_id })` and
  `View.find_table_views_where(context.table_id, …)`, so there is no form at all without a
  context. `ViewRuntime::config_steps` is therefore a **per-step call carrying the
  accumulated context** — which is what v1's `Workflow` genuinely is, a wizard — and the
  built-in patterns need exactly the same thing for exactly the same reason.
- **`FieldRepeat` has to be real**, because both plugins build repeated configuration
  sections with it. It is 146 pure lines; it is vendored.

Two things these plugins reach for that this milestone does **not** give them, stated here
rather than discovered later:

- **`virtual_triggers`** — Kanban declares them to emit realtime events. They are only
  produced when its `real_time_updates` setting is on, and with it off the array is empty, so
  Kanban works without them. The key is read and reported as unsupported, the way a module's
  other unsupported keys already are.
- **Raw SQL.** `@saltcorn/mind-map` builds a recursive CTE with `db.query`,
  `db.getTenantSchemaPrefix()` and `db.sqlsanitize`. Milestone 23 left the v1 `db` module out
  deliberately, and a `db.query` here would be arbitrary SQL from a plugin, bypassing the plan
  seam, the ownership rule and the row cap. **Kanban is therefore the milestone's plugin test
  and mind-map is not**: it installs, its pattern registers, and the view fails at run time
  with `db.query` named. Whether this server ever offers a v1 `db` — as the server's own role,
  read-only, or not at all — is a decision of its own and is listed under *Carried past*.

### 7. `getState()` is the application, not the tenant

v1's `getState()` is a per-tenant singleton holding config, plugins, types, the menu and the
i18n catalogue. Here it is built **per application**, from the application row and the view
snapshot, because that is the scope the same questions have: `getConfig("site_name")` is the
app's name, `base_url` is the app's subdomain, the menu is the app's, and `roles` is the
server's role table.

Its `getConfig` answers a **fixed, declared** set of keys — the ones the vendored patterns
actually read — and an unknown key answers the supplied default rather than throwing, because
that is v1's contract and a pattern asking for `pagination_size` is not an error. The set is
one list in one file, and a key added to it is a key the framework's `config_spec` offers an
admin.

### 8. The request has to grow, and the response has to be able to sign somebody in

`AppRequest` is today a method and a path, and `AppResponse` a status, a content type and
bytes — which is everything a static bundle needs and about a third of what a server-rendered
view needs. They grow:

```rust
pub struct AppRequest {
    pub method: Method,
    pub path: String,
    pub query: BTreeMap<String, String>,      // v1's req.query
    pub body: RequestBody,                    // Form(pairs) | Json(Value) | Empty
    pub headers: BTreeMap<String, String>,    // the few v1 reads: referer, x-requested-with…
    pub user: Option<User>,                   // v1's req.user
    pub base_url: String,                     // v1's req.get_base_url()
}

pub struct AppResponse {
    pub status: u16,
    pub content_type: String,
    pub body: Bytes,
    pub headers: Vec<(String, String)>,       // Location, and nothing exotic
    pub session: Option<SessionAction>,       // so the app's own login works
}
```

`session` is the one that matters structurally: an application's REST provider can already log
somebody in (`ApiResponse::session`, applied by `apply_response`), and a Saltcorn UI app must
be able to do the same from a **rendered form** rather than a JSON call. Reusing the field and
the code that applies it means the app's session cookie is the same cookie, set the same way,
with the same CSRF double-submit — there is no second session story.

`CodeFramework` ignores every new field, and a test asserts it still serves the same bytes.

### 9. One layout, built in, and the client assets that go with it

v1's page chrome comes from a **theme plugin** (`sbadmin2`), and a plugin system for themes is
not something to build in order to render a list. Saltcorn UI ships **one layout**: v1's
`emergency_layout` — `navbar(brand, menu, currentUrl)` plus `renderLayout` of the body —
which is plain Bootstrap 5 and is the thing v1 itself falls back to. The document around it
(doctype, head, the asset tags, the alerts) is Saltcorn UI's own, ported from v1's
`wrapper.js`.

The browser assets are vendored beside the runtime and served by the framework at
`/static_assets/<version>/…`: Bootstrap 5.3, jQuery, `saltcorn-common.js`, `saltcorn.js` and
`saltcorn.css` from v1's `packages/server/public`. They are what makes a v1 view *work* rather
than merely appear — the ajax view reload behind a Filter, the form submit, the modal popup, the
`view_link` in a `<td>` — and they are jQuery, which is a fact about the HTML `list.ts` emits
and not a choice being made here.

`ui/saltcorn-ui` is therefore the **third** bundle in `crates/sc-cli/build.rs`, beside
`ui/admin` and `ui/ide`, recorded as `SC_SALTCORN_UI_BUNDLE_DIR`. A `--no-ui` build has no
Saltcorn UI, exactly as it has no admin UI, and an application whose framework is
`saltcorn-ui` says so on mount rather than failing per request.

### 10. §18.5, answered: the framework's own CSP, and what that costs

The open question is how server-rendered v1 views live under a strict CSP. The honest answer
for this milestone is that **they do not**, and the machinery to say so already exists:
`framework_default_csp` lets a framework supply the policy for an app that does not state one,
and Saltcorn UI's widens `script-src` with `'unsafe-inline'`.

That is not a shrug. v1's markup puts JavaScript in `onclick`, `onchange` and `href="javascript:…"`
attributes in dozens of places across `viewable_fields.ts` and the six patterns, and inline
event handlers cannot be nonced — only `'unsafe-hashes'` with a hash per distinct handler, or
`'unsafe-inline'`, permits them. Externalising them is a real and desirable piece of work
(delegated listeners over `data-` attributes, in the vendored copy) and it is a milestone of
its own, not a prerequisite for rendering anything.

What does **not** move: `default-src 'self'`, no `eval`, no `blob:`, no third-party origins.
The admin UI's policy is untouched, and the existing character-for-character assertion on it
stays. The relaxation is per-framework, visible on the application's own screen, and an admin
who states a policy still wins.

### 11. Roles, and whose authority a view renders under

Two checks, and they are not the same one:

- **The view's own `min_role`**, checked by the framework *before* the pattern runs, against
  the viewer's role. This is v1's rule and it governs whether the view is reachable at all.
- **The row rule** (§7.3), checked by the row layer for every read and write the pattern makes,
  because `v1_api.js` already lowers `Table.getRows(where, { forUser })` and
  `insertRow(row, user)` to `Authority::User(id)` and through the same `*_as` functions the
  agent tools go through.

A view is rendered with the viewer's authority, always — `forPublic` for an anonymous one.
There is no "the view is allowed so the rows are" shortcut, which is exactly the mistake that
makes a `min_role: 100` list leak somebody's private rows.

The app's **table subset** bounds what a view may name, checked on save and again on render:
a view naming a table the application does not have is a configuration error with the table
named, not an empty list.

### 12. Actions in views

A v1 view's Action column runs something named by a string. Three kinds, and the framework
resolves them in this order:

1. **The view actions**, which are v1's own and are implemented in the vendored bundle:
   `Delete`, `Save`, `Reset`, `Cancel`, `GoBack`, `Login`, `Sign up`, `Logout`.
2. **A trigger of this server**, by name, resolved through the existing `TriggerHost` surface
   and bounded by the application's declared trigger subset (`Application.triggers`, which
   exists).
3. **Anything else** — a v1 `state_action`, a plugin action — which is refused by name, at
   save time where possible and at run time otherwise, the way an unknown action in a trigger
   already is.

That ordering is stated because v1's is the other way round in one case (a trigger named
`Delete` shadows the built-in), and this server's rule is that the built-in set is fixed and
a trigger cannot take a name out of it.

### 13. The import: what a v1 backup becomes

`backup::v1` currently reports views and pages as not imported. It now translates them, and
the translation creates the **application** they belong to:

- **One application per restored v1 backup**, named from the pack's `site_name` config, with
  framework `saltcorn-ui`, a subdomain derived from the name, every imported table in its
  subset, and the imported file store in its store list. A restore into a server that already
  has an application of that name **replaces that application's views and pages** and keeps
  its subdomain and settings, which is what re-importing a backup means.
- **Views** map one-to-one; `configuration` crosses **unchanged**. A view whose
  `viewtemplate` is not one of the six registered patterns (`Room`, or one a v1 plugin
  supplied) is a line in the report naming the view and the pattern, not a failure. So is a
  view on a table that did not import.
- **Pages** map one-to-one, layout unchanged, with `root_page_for_roles` kept: it is what
  makes `/` resolve.
- **The menu** comes from the pack's `menu_items` config, minus every `Admin Page` and
  `User Page` entry — those point at v1's admin UI, which does not exist here — with a note
  per kind dropped. What is left (View, Page, Link and Header entries) is the app's menu.
- The existing notes for page groups, the library, tags, models, plugins and code pages stay
  exactly as they are.

### 14. What the admin UI gets, and what it waits for

An application's screen gains **Views** and **Pages** tabs: the list, with the pattern, the
table, the role and a link that opens the view on the app's subdomain; create, rename, delete;
and the configuration editor of Phase 10 — the pattern's `configuration_workflow` steps, crossing
as `FormField` lists through `sc_module::spec::config_fields_to_form_fields`, which is the
same translation a module's settings already go through and the same renderer that draws an
LLM provider's form.

What waits for `ui/builder` is the **layout** step and nothing else. A pattern whose
configuration is entirely form steps (`ListShowList`) is fully editable at the end of this
milestone; `List`, `Show`, `Edit`, `Feed` and `Filter` are editable except for their layout,
which is shown as read-only JSON with a sentence saying what will edit it. An imported view's
layout is therefore *preserved and rendered* from day one, which is what "focus first on
running existing views" asks for.

### 15. Tests

Five levels, because the failure modes are at five levels:

- **Unit, in Rust**: the view and page records against a real Postgres; the save-time
  validation (unknown pattern, table outside the subset, duplicate name, bad role); the
  snapshot's shape and its generation.
- **Unit, in JavaScript, run through the worker**: each shim answering what v1 answers; the
  refusals naming themselves; the absent tier answering `undefined`; the embed-depth cap;
  `getState().getConfig` defaulting; and the export-partition test over `plugin-helper`.
- **Golden HTML**: each of the six patterns rendered over a fixture table and asserted against
  a committed expected document. This is the test that catches a shim that returns a plausible
  wrong thing, and it is the reason the vendored copy can be refreshed with confidence.
- **Live, over HTTP**: the BooksDB fixture restored, the application mounted on its subdomain,
  and the definition of done driven through the assembled router — sign in, page, filter,
  list, show, edit, save, delete.
- **A third-party plugin, installed** (ignored by default; npm): `@saltcorn/kanban` installed
  from its checkout, its two patterns on the registry, a Kanban view created over a fixture
  table, rendered, and its `set_card_value` route posted to. This is the only test that can
  tell a compatibility library from a private bundle, because it is the only one written by
  somebody who did not know what we shimmed.

---

## Phase 1 — The view, the page, and where they live

- [x] 1.1 `crates/sc-viewpattern`: a new crate. It sits **above** `sc-app` rather than at
      §2's layer 8, because it implements `Framework`; `sc-module` implements its runtime
      seam from above, which is the same acyclic shape `sc-model` and `sc-app` already have
      with `sc-module`. §2's tree is corrected in Phase 12.
- [x] 1.2 `View` and `ViewId`, and `_fd_views`: `id`, `application`, `name`, `description`,
      `viewpattern`, `table_name`, `configuration` (json, v1-shaped), `min_role`, `slug`,
      `attributes`. Unique on (`application`, `name`). Bootstrapped idempotently, like
      `_fd_applications`.
- [x] 1.3 `Page` and `PageId`, and `_fd_pages`: `id`, `application`, `name`, `title`,
      `description`, `layout` (json), `min_role`, `attributes` (carrying
      `root_page_for_roles`). Unique on (`application`, `name`).
- [x] 1.4 `save_view` / `load_view` / `list_views` / `delete_view` and the page four, all
      scoped by `AppId`. Validation (§1, §11): the pattern is registered; the table is in the
      application's subset; `min_role` is a role that exists; the name is unique in the app
      and is URL-safe.
- [x] 1.5 `ViewSet` — every view and page of one application, loaded once and re-loaded on a
      write, with a `generation` counter for §4.
- [x] 1.6 Live tests against Postgres: the round trip, the four refusals each naming what is
      wrong, two applications holding same-named views over the same table, and a generation
      that moves on write and not on read.

## Phase 2 — v1's source, vendored and bundled

- [x] 2.1 `ui/saltcorn-ui/vendor/`: the copied files, each with a header naming its upstream
      path and the v1 version, and `vendor/README.md` — the list, the line §2 draws, and the
      refresh procedure. The six patterns, `plugin-helper.ts`, `viewable_fields.ts`,
      `base-plugin/types.ts`, `fieldviews.ts`, `fileviews.ts`, `models/form.ts`,
      `models/fieldrepeat.ts`, `models/expression.ts`, and `@saltcorn/markup`. Not `room.ts`,
      not `workflow-room.ts`.
- [x] 2.2 `plugin-helper.ts` partitioned per export (§2): the rendering half kept, the
      querying half (`generate_joined_query`, `json_list_to_external_table`,
      `build_schema_data`, and whatever else the partition finds) replaced by the refusal that
      names it — with a test that walks the module's exports and asserts each is on exactly
      one side.
- [x] 2.3 `ui/saltcorn-ui/src/index.ts`: the bundle's entry — the pattern registry, the
      fieldview/fileview registries, the **library exports keyed by v1 specifier** (§5,
      `@saltcorn/markup/tags`, `@saltcorn/data/plugin-helper`, …), and the entry points §3.3
      calls — and its esbuild config, with every host-supplied specifier (§5's right-hand
      column) marked external. Output `dist/view-runtime.js`, one ESM file.
- [x] 2.4 `ui/saltcorn-ui/public/`: Bootstrap 5.3, jQuery, `saltcorn-common.js`,
      `saltcorn.js`, `saltcorn.css` and the icon font, vendored from v1's
      `packages/server/public` with the same header rule, staged into `dist/public/`.
- [x] 2.5 `crates/sc-cli/build.rs` builds it as the third bundle and records
      `SC_SALTCORN_UI_BUNDLE_DIR`; `ServerConfig::saltcorn_ui_dir` and `main.rs` carry it;
      `scripts/build-static.sh`, `install.sh` and the Dockerfile stage it beside the other two.
- [x] 2.6 A build with `SC_BUILD_ADMIN=0` records no directory, and an application whose
      framework is `saltcorn-ui` then fails to **mount** with a sentence naming the missing
      bundle — once, on the mount, not once per request.
- [x] 2.7 A test that the bundle exists and evaluates: `sc-viewpattern`'s `bundle_shape` test
      reads the real `dist/view-runtime.js`, asserts the six patterns are exported with the
      names v1 gives them and that every v1 specifier §5's left-hand column promises resolves
      to an object with the exports v1 has on it, and runs in every `cargo test` that has a
      built bundle.

## Phase 3 — Where it runs: the built-in module and the render seam

- [x] 3.1 `sc_viewpattern::ViewRuntime` — the object-safe seam, declared here and implemented
      one layer up (the `TableProviderHost`/`FrameworkHost` shape): `patterns()`,
      `render(view, state, ctx)`, `render_page(page, ctx)`, `post(view, body, ctx)`,
      `route(view, route, body, ctx)`, and `config_step(pattern, table, step, context)` — a
      **call per step carrying the accumulated context**, not load-time data (§6).
- [x] 3.2 `sc-module`: the reserved built-in module `@feldspar/saltcorn-ui`, loaded from the
      bundle directory rather than the modules root, with an empty `ModulePermissions`. The
      name is refused to an installed module, on its card, like `react` is refused to a
      declared framework.
- [x] 3.3 `module-host.mjs` gains the view entry points, dispatching into the bundle, and
      `viewPatterns()` — the registry manifest that crosses **once, at load**: name, label,
      description, `table_required`, `view_quantity`, whether it has `routes`, and the *names*
      of its configuration steps. Data, for the reason §13.3 gives about frameworks: the admin
      UI asks these on the path that renders a form. A step's **fields** are not on it, for
      the reason §6 gives: they do not exist without a table.
- [x] 3.3a `saltcornModule()` resolves §5's left-hand column into the bundle's library exports
      instead of into a stub namespace, and the **absent tier** (§5) is a named list answering
      `undefined`. One `require` table, for the built-in patterns and for every installed
      module alike — an action that reaches for `@saltcorn/markup/tags` today gets stubs and
      gets the real thing after this item.
- [x] 3.4 `ViewSnapshot` (§4): built from the application row and its `ViewSet`, serialised
      once, cached on the worker behind its generation, re-sent only when it moves.
      `__scDefineViews(generation, json)` beside `__scDefineSchema`.
- [x] 3.5 The bounds: a render runs under the module call timeout; the embed depth cap (16)
      names the cycle it broke; a pattern that throws produces an *Application* error carrying
      the view's name, not a 500 with a stack.
- [x] 3.6 `sc_module::ModuleViewRuntime` implements the seam, installed at boot beside
      `ModuleFrameworks`.
- [x] 3.7 Tests: the built-in module loads with no modules root at all; an installed module
      claiming the reserved name keeps its actions and loses nothing else; the snapshot is
      sent once for two renders at one generation.

## Phase 4 — The compat layer in JavaScript

- [x] 4.1 `View` from the snapshot, synchronous: `findOne`, `find`, `find_table_views_where`,
      `find_all_views_where`, `find_possible_links_to_table`, and the instance properties
      (`name`, `table_id`, `configuration`, `min_role`, `slug`, `attributes`,
      `viewtemplateObj`). `run`, `runMany`, `runPost`, `runRoute`, `get_state_fields` and
      `combine_state_and_default_state` dispatch into the registry **in-worker** (§3).
- [x] 4.2 `Page` from the snapshot, and `renderPage` over `@saltcorn/markup`'s `renderLayout`
      with the embedded-view dispatch of 4.1.
- [x] 4.3 `getState()` (§7): `types`, `keyFieldviews`, `fileviews`, `viewtemplates` from the
      bundle; `getConfig` over the declared key set with v1's defaulting; `roles`; `actions`
      (§12's three kinds); `functions` over the existing `modfn` surface; `getLayout`
      answering the one built-in layout; `log`; `i18n`/`__` as identity; `emitRoom` refused
      by name.
- [x] 4.4 The `req`/`res` shims: `user`, `query`, `body`, `params`, `method`, `path`,
      `headers`, `xhr`, `csrfToken()`, `flash()`, `getLocale()`, `__`, `get_base_url()`;
      `res.redirect`, `res.json`, `res.status`, `res.sendWrap`, `res.trigger_return`. Built
      from the `AppRequest` of §8 and read back out of it after the call.
- [x] 4.5 `Form` and `FieldRepeat` come from the vendored `models/form.ts` and
      `models/fieldrepeat.ts` and **replace** the stub in `module-host.mjs`, so a module's
      `configuration_workflow`, a plugin pattern's repeated config section and an Edit view
      build the same classes. `Workflow` keeps its stub; the note says which is which.
- [x] 4.6 `Trigger` over the existing trigger surface (`Trigger.findOne`, `trigger.run`,
      bounded by the app's subset), `File` and `User` minimally over the file and auth
      surfaces, `Crash` to the error log, `Library`/`PageGroup` inert and empty.
- [x] 4.7 The refusal list extended (§5): every v1 model member the view runtime reaches that
      this server does not implement, on the one list in `v1_api.js`, each naming itself. The
      build-time check that a name cannot be both implemented and refused already exists and
      now covers these.
- [x] 4.8 JavaScript unit tests through the worker: each shim's shape against v1's, the
      refusals, the absent tier answering `undefined` for each name on it (with the plugin
      idiom that justifies it in the test's name), `getConfig` defaulting, and the depth cap.

## Phase 5 — The framework, and rendering a view

- [x] 5.1 `AppRequest` and `AppResponse` grow (§8). `CodeFramework` ignores the new fields and
      a test asserts its bytes are unchanged; `router.rs` fills them from the live request and
      applies `session` through the existing `apply_response`.
- [x] 5.2 A framework **factory** registry in `sc-app`: a named constructor installed at boot,
      the shape `installed_frameworks` already has, so a `Framework` implemented outside
      `sc-app` can be mounted without `sc-app` depending on it.
- [x] 5.3 `SaltcornUiFramework`: name `saltcorn-ui`, label and the picker's sentence,
      `serves_ui() == true`, `build() == None`, `framework_builder_agent` answering `None`
      (there is no source tree to code in), and `framework_default_csp` per §10.
- [x] 5.4 Its `config_spec`: site name, the menu (JSON for now), the root page per role, and
      the handful of `getConfig` keys §7 declares. Validated on save like every other
      framework's.
- [x] 5.5 The GET routes: `/` (the role's root page, else the first page, else a "nothing
      here yet" document), `/view/:name`, `/view/:name/*slug`, `/page/:name`,
      `/static_assets/:v/*` from the bundle, and `/files/serve/*` from the app's file stores
      through the existing access rules.
- [x] 5.6 The document (§9): doctype, head, the asset tags, `emergency_layout`'s `wrap` with
      the app's menu and brand, alerts, and the `<title>` from the view's `page_title`
      attribute.
- [x] 5.7 Mounting: no build, so a save re-reads the view set and bumps the generation, and
      `reload_all` (SIGHUP) reloads views and pages with the application row.
- [x] 5.8 Golden-HTML tests (§15) for the six patterns over a fixture table, and a live test
      rendering *List Books* over HTTP.

## Phase 6 — Posting: forms, routes and actions

- [x] 6.1 `POST /view/:name` → the pattern's `runPost`: form-encoded body into `req.body`, the
      redirect or the re-rendered form back out.
- [x] 6.2 `POST /view/:name/:route` → the pattern's `routes` (`run_action`,
      `update_matching_rows`), answering JSON, and the `delete` route.
- [x] 6.3 The view actions of §12.1 in the vendored bundle, and §12.2's trigger resolution
      through `TriggerHost` bounded by `Application.triggers`. §12.3's refusal, at save time
      where the configuration names it and at run time otherwise.
- [x] 6.4 CSRF: `req.csrfToken()` answers the app session's token, the rendered forms carry
      it, and a POST without it is refused by the existing double-submit check.
- [x] 6.5 Live tests: an insert through *Edit Books* that lands a row; an update; the Delete
      action; the Filter view's dropdown and range round-tripping through the state and
      narrowing the embedded list; and a POST with no CSRF token refused.

## Phase 7 — Who is looking: roles, authority and signing in

- [ ] 7.1 The viewer's role reaches the framework, and a view or page whose `min_role` excludes
      it is **not run**: an anonymous viewer is redirected to the login page with a `dest`, a
      signed-in one gets a 403 document naming the view.
- [ ] 7.2 Every read and write the runtime makes carries the viewer as `Authority::User(id)`
      (`forPublic` when anonymous), through the `*_as` functions §7.3 defines. A test with an
      ownership formula asserts two users see two different lists **through a view**.
- [ ] 7.3 `/auth/login`, `/auth/logout` and `/auth/signup` rendered by the framework, answered
      with `AppResponse::session`, honouring the same lockout and password rules the admin
      login does. Sign-up is offered only when the app's settings allow it.
- [ ] 7.4 The table-subset check on render (§11), with the table named.
- [ ] 7.5 Live tests: the redirect, the round trip through login to the originally requested
      view, logout, and a view naming a table outside the subset failing with that sentence.

## Phase 8 — A v1 backup becomes a Saltcorn UI application

- [ ] 8.1 `backup::v1` translates `views` and `pages` instead of counting them, and the two
      lines leave `note_what_was_left_out`.
- [ ] 8.2 The application: named from `site_name`, framework `saltcorn-ui`, subdomain derived
      and de-duplicated, every imported table in its subset, the imported file store in its
      list, and the row written through the ordinary `save_application`.
- [ ] 8.3 The menu from `menu_items`, minus `Admin Page` and `User Page` entries, with a note
      per kind dropped and the `Header`/subitem nesting kept.
- [ ] 8.4 The report lines: a view whose pattern is not registered, a view or page on a table
      that did not import, and a view referencing a view that did not import — each naming the
      view, and none of them failing the restore.
- [ ] 8.5 `min_role`, `slug`, `attributes` and `root_page_for_roles` carried; the restore
      dialog's selection covers views and pages like every other kind.
- [ ] 8.6 Re-importing into an existing application of the same name replaces its views and
      pages and keeps its subdomain, settings and CSP.
- [ ] 8.7 Tests over `saltcorn-v1-BooksDB.zip`: seven views, one page, the application row, the
      menu, the subdomain — and the same archive restored twice leaving seven views, not
      fourteen.

## Phase 9 — The admin UI: an application's views and pages

- [ ] 9.1 Endpoints: `listViews`, `getView`, `deleteView`, `listPages`, `getPage`,
      `deletePage`, `listViewPatterns` — and `saveView`/`savePage`, which Phase 10 fills in.
- [ ] 9.2 The Application screen's **Views** and **Pages** tabs: the list with pattern, table,
      role and a link that opens it on the app's subdomain; delete with the usual
      confirmation; and an empty state that says a Saltcorn UI application with no views
      serves nothing.
- [ ] 9.3 The application list shows Saltcorn UI apps without a Build button and without a
      "saved but unbuilt" state, because there is nothing to build.
- [ ] 9.4 Tests: the API round trip, and `views.test.ts` for the list's own logic.

## Phase 10 — Editing a view, without the builder

- [ ] 10.1 The pattern's `configuration_workflow` as a **wizard**: one `config_step` call per
      step (§3.1) carrying the table, the view name and the context accumulated so far,
      translated to `FormField`s by `config_fields_to_form_fields` and rendered by the same
      form the LLM provider and file store screens use. `saveView` validates the configuration
      by replaying the steps. A step whose form cannot be built names the step and the reason,
      rather than being skipped as a module's settings step is.
- [ ] 10.2 Creating a view: table, pattern, name, `min_role`; the pattern's `initial_config`
      supplies the first configuration, so a new List has its table's columns in it.
- [ ] 10.3 The layout step shown as read-only JSON with the sentence naming what will edit it.
      `ListShowList` has no layout step and is therefore fully editable here; the other five
      are editable except their layout.
- [ ] 10.4 Renaming a view, and the report of what references it (`connectedObjects`, which
      the patterns already export) shown before the rename rather than after.
- [ ] 10.5 Tests: a view created from nothing through the API renders; a configuration that
      the steps refuse is refused on save naming the field; a rename updates nothing silently.

## Phase 11 — View patterns from an installed plugin

- [ ] 11.1 `viewtemplates` as a module facility key (§6): parsed from the loaded plugin,
      installed into the registry whole on every module change, one namespace with the
      built-ins, a clash losing *that pattern* with the reason on the module's card.
      `ModuleManifest::unsupported` stops counting it.
- [ ] 11.2 `headers` on the manifest — `{ script | css, onlyViews }`, crossing at load as data
      — and `/plugins/public/<name>@<version>/*` served by the app out of the installed
      package's `public/` directory, with the same path confinement the file stores use.
- [ ] 11.3 The document builder (§9) injects the headers of every module whose `onlyViews`
      names the pattern being rendered, or that declares no `onlyViews`, de-duplicated and in
      manifest order.
- [ ] 11.4 `virtual_triggers` read and reported as unsupported, naming the view — not silently
      dropped, because a Kanban with `real_time_updates` on and no triggers is a Kanban that
      looks like it works.
- [ ] 11.5 `db.connectObj.version_tag` answers the asset version tag, because two plugins
      build a `<script src>` out of it and a broken one is a silent 404 rather than an error.
- [ ] 11.6 The `@saltcorn/kanban` test (§15, ignored by default; npm): installed, both
      patterns registered, a Kanban view configured over a fixture table through the wizard of
      Phase 10, rendered, and `set_card_value` posted to.
- [ ] 11.7 `@saltcorn/mind-map` installed in the same test: its pattern registers and its
      render fails naming `db.query` (§6). Asserted, so that the boundary is a fact the suite
      states rather than a paragraph in this file.

## Phase 12 — Documentation and the definition of done

- [ ] 12.1 `docs/TECHNICAL_DESIGN.md`: §13.3's Saltcorn UI paragraph replaced with what was
      built, §18.5 marked answered with §10's reasoning and the externalisation work named as
      the follow-up, §2's crate tree corrected for `sc-viewpattern`'s position, §9.2's ER
      diagram gaining `_fd_views` and `_fd_pages`.
- [ ] 12.2 `docs/tutorial-saltcorn-ui.md`: restore a v1 backup, or start empty — a table, a
      List, a Show, an Edit, a page, a menu, and a link that works.
- [ ] 12.3 README §3 and `docs/OPERATIONS.md` (the third bundle, the build-time variable, and
      what `--no-ui` costs); the CHANGELOG.
- [ ] 12.4 The definition of done, run by hand, against a real server.

---

## The definition of done, run by hand (12.4)

1. `feldspar serve` on a database with no applications.
2. Backup → Restore → `saltcorn-v1-BooksDB.zip`. The dialog says *Saltcorn 1.7.0, imported*.
   Restore everything.
3. The report says four tables, their rows, one trigger refused by name, seven views and one
   page imported — and lists the page group, the library entry and the tag it did not import.
4. Applications lists **BooksDB**, framework *Saltcorn UI*, no Build button. Its Views tab has
   seven rows, its Pages tab one.
5. Open `booksdb.<base-domain>/`. Because `root_page_for_roles` is empty, a "nothing here yet"
   document names the pages that exist. Follow *BooksOverview*.
6. Anonymous, the page is `min_role` 1, so the login form appears with a `dest` back to it.
7. Sign in as the admin. `/page/BooksOverview` renders: the publisher dropdown, the pages
   range slider, and *List Books* beneath with five rows, each showing the author's last name
   joined from `Authors` and the publisher's name from `Publishers`.
8. Pick an author in the dropdown. The list re-runs and narrows without a full page load.
9. *Show* on a row renders the Show view. Back; *Edit* renders the form with the author and
   publisher as selects; change the page count, Save, and land back on the list with the new
   value.
10. *Add row* → the Edit view empty → Save → a sixth row.
11. *Delete* on it → five rows.
12. `View source`: the HTML is v1's, the assets come from `/static_assets/`, and the response
    carries the Saltcorn UI CSP with `script-src 'self' 'unsafe-inline'` and nothing else
    relaxed.

---

## Explicitly OUT of scope for this milestone

- **The builder.** `ui/builder` — Craft.js over the layout step — is the next milestone. §14
  says exactly what waits for it and what does not, and the layout of every imported view is
  rendered, preserved and shown meanwhile.
- **`room` and `workflow-room`, and everything realtime.** Both are socket.io views; this
  server has no socket transport for applications and `emitRoom` is a named refusal. It needs
  a websocket route on the app's subdomain and a room-membership rule, which is a design of
  its own.
- **Page groups.** v1's "pick a page by screen size and role" layer over pages. The model is
  small; what it needs is a second resolution step in front of `/page/:name` and an editor,
  and neither belongs in a milestone about rendering.
- **The library and tags.** The library is saved layout fragments for the builder — it arrives
  with the builder. Tags are a cross-entity selection mechanism this server does not have for
  any entity yet.
- **File upload from an Edit view.** `req.files` is empty and the upload fieldviews render
  read-only. It needs multipart parsing in `AppRequest` and a `File` model over the `fs`
  surface that can write; the BooksDB fixture has no File column, so nothing in the definition
  of done needs it.
- **Themes as plugins.** One built-in layout (§9). A theme system is a plugin facility key, a
  layout seam and an asset story, and v1's own answer to it is the part of v1 that aged least
  well.
- **i18n.** `__` is the identity function and `getLocale()` answers the server's one locale.
  The strings the patterns expose (`getStringsForI18n`) are still collected, so the catalogue
  has somewhere to come from later.
- **A v1 `db` module for plugins** — `db.query`, `db.sqlsanitize`,
  `db.getTenantSchemaPrefix`. This is what `@saltcorn/mind-map` needs and does not get (§6),
  and it is out because it is a decision rather than an omission: raw SQL from a plugin goes
  around the plan seam, the ownership rule and the row cap, all three of which exist on
  purpose. `db.connectObj.version_tag` is supplied (11.5) because it is a string, not a query.
- **A plugin pattern's own builder components.** v1 plugins can register Craft.js components
  for the builder; nothing here can, because there is no builder yet. It belongs with
  `ui/builder`.
- **Mixing frameworks in one application** (§18.1). A Saltcorn UI app is a Saltcorn UI app.
- **v1's search, notifications, user settings and sign-up flows** beyond the three auth routes
  of Phase 7.3.

## Carried past this milestone

- **Externalising the inline handlers** (§10). The work that turns the Saltcorn UI CSP back
  into the strict one: delegated listeners over `data-` attributes in the vendored markup and
  `viewable_fields`. It is mechanical, it is large, and it is testable against the golden HTML
  this milestone commits.
- **A view's own `_fd_` history.** Renaming a view breaks the references to it by name, and
  10.4 only *reports* them. Rewriting references on rename wants an entity-reference index,
  which several other entities would use.
- **`Feed` and `ListShowList` in the definition of done.** They are registered, tested against
  golden HTML and importable, but the BooksDB fixture has neither, so nothing end-to-end
  exercises them. A second fixture would.
- **The `getConfig` key set** (§7) is the keys the six patterns read today. A pattern added
  later that reads another key needs it declared, and the failure mode — the supplied default,
  silently — is the one place this milestone chooses v1's contract over this server's
  no-silent-failure rule. A warning on an undeclared key would be the middle path.
- **Performance.** Every render is a worker call with a JSON round trip of the rows. That is
  the right shape and the wrong constant; a render cache keyed on the view, the state and the
  role, invalidated by the table's write, is the obvious next thing and it needs the write
  notification `sc-bus` already carries.
- **Whether raw SQL is ever offered to a plugin**, and in what form: as the server's own role,
  read-only, behind a permission on the module's card, or never. `@saltcorn/mind-map` is the
  case that forces the question — its recursive CTE walks a parent link, which the plan seam
  genuinely cannot express — so the alternative answer is a *recursive relation* in the seam
  rather than a SQL escape hatch, and that is the one worth costing first.
- **The absent tier as a growing list** (§5). It holds the names today's two plugins
  feature-detect. Every plugin found to feature-detect another name adds one, and there is no
  way to know the list is complete; a report of what a loaded module reached for and did not
  find would turn that from guesswork into data.
