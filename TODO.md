# Saltcorn v2 — The builder: v1's drag-and-drop editor for views and pages, and the library

Ordered, checkable task list for the twenty-fifth milestone after the MVP. Earlier lists are
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
[docs/TODO-post-mvp-22.md](./docs/TODO-post-mvp-22.md) (predictive models),
[docs/TODO-post-mvp-23.md](./docs/TODO-post-mvp-23.md) (the v1 `Table` API) and
[docs/TODO-post-mvp-24.md](./docs/TODO-post-mvp-24.md) (Saltcorn UI: the v1 views, running).
Scope and rationale remain in [docs/GOALS.md](./docs/GOALS.md) ("builder is not built in to
the admin ui … This needs to be implemented in react due to high availability of underlying
libraries i.e. craft and react-flow") and [docs/TECHNICAL_DESIGN.md](./docs/TECHNICAL_DESIGN.md)
(**§13.3**, "Saltcorn UI", and the `ui/builder` bullet, which says this milestone is next).

The last milestone made a Saltcorn UI application run. An imported List lists, an imported Edit
saves, and a view can be created from nothing and configured step by step, **except for one
step**: the layout. That step is the one that makes a Show, an Edit or a Filter anything at
all, and right now the admin UI shows it as read-only JSON with a sentence apologising for it.
A new Show gets v1's default (a label and a value per field) and can never be anything else. A
new List gets a column per field and no Show link, no Edit link and no Delete. A new Filter
gets an **empty** layout (`initial_config` is `{ layout: {}, columns: [] }`), so it filters
nothing. Pages are worse off: they have **no editor at all**, and the tutorial creates one by
pasting a `fetch` into the browser console. That is not "the saltcorn1 experience"; it is the
saltcorn1 renderer with the experience taken out.

This milestone puts the experience back: **the builder**. It is v1's `@saltcorn/builder`, the
Craft.js canvas Saltcorn 1 has shipped for years, **vendored the same way the view patterns
were** and served on its own admin route. It edits the layout of **Show**, **Edit**, **List**
and **Filter** views, and of **pages**. Pages also get the rest of v1's page editor: a
properties form to create and edit them, and the `POST /page/:name/action/:rndid` route a page's
action buttons post to. **Page groups** stay out, because they sit on top of pages (A/B
testing, dispatching device widths to different pages) and a page can be built, served and
used without them. It also brings the **library**: named, reusable layout fragments ("shared
components" in current v1) that are saved from the builder, placed in any view's or page's
layout, edited in place so everything that uses them changes, and rendered by v1's own
`Library.resolveSegment`. Library items belong to the Saltcorn UI framework and are stored per
application, beside that application's views and pages. No other framework sees them.

**Milestone definition of done:** an admin restores `saltcorn-v1-BooksDB.zip`.

1. **A view.** **Applications → BooksDB → Views → Show Books → Configure**: the *Layout* step
   has an **Open in builder** button instead of a JSON dump. The builder opens: toolbox on the
   left, the imported layout on the canvas exactly as the subdomain renders it, settings on
   the right. They drag the *Publisher → name* join field under the title, press **Save**, and
   `booksdb.localhost:3032/view/Show%20Books?id=1` shows the publisher.
2. **A List, from nothing.** A List called *Recent books* over Books opens straight into the
   builder with v1's default columns. They add a *Show* link column, an *Edit* link column and
   a *Delete* action column, press **Next**, and the wizard carries on at *Create new row*, a
   form step. They save, and the list works on the subdomain with every link going somewhere.
3. **A Filter, from nothing.** A Filter called *Find books* opens on an empty canvas. They drop
   in a search bar, a dropdown filter on *author* and a *Clear* action button, and save.
4. **A page, from nothing.** **Pages → New page**: name *Home*, title *Library*, minimum role
   *public*. **Create** opens the page in the builder. They lay out two columns: *Find books*
   on the left, *Recent books* on the right with state *shared*, a heading above both, and a
   *GoBack* action button below. They save and set *Home* as the public home page. On
   `booksdb.localhost:3032/`, searching narrows the list, *Clear* clears it, and the action
   button posts to `/page/Home/action/<rndid>` and answers instead of 404ing.
5. **The imported page.** *BooksOverview* opens in the builder with its filter and list in
   place. A text block added above them is saved and rendered.
6. **The library.** In *Show Books* they select the card holding the title, choose **Save as
   library component** and name it *Book header*. They place *Book header* on the *Home* page
   and change its heading there, and *Show Books* shows the changed heading too. **Library** on
   the application lists *Book header*, used by one view and one page.

Everything is still v1's source: the canvas is v1's `Builder.js`, the saved layouts are v1's
JSON, and the rendered HTML comes from v1's `show.ts`, `filter.ts` and `renderLayout`,
resolving `library` segments with v1's `resolveSegment`.

**Not in this milestone:** **page groups**. A page group is a named set of pages with a rule
choosing among them by screen size, role or random split. It is another resolution step in
front of `/page/:name` with its own editor, and pages work without it. The builder's
page-group options answer an empty list, so it never offers a link to one. Also out:
**HTML-file pages** (v1's `html_file` property: a page whose content is an HTML file from the
file store, which the runtime already refuses by name), *Generate layout with copilot*,
uploading an image from inside the builder, v1's help topics, TypeScript completions in the
builder's formula editors, replacing CKEditor 4, sharing a library item between applications,
editing the menu in anything but JSON, and several people building one layout at once.
*Explicitly OUT* names each one and what it would take.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

---

# The specification

### 1. The builder is v1's source, vendored, and GOALS' "TypeScript" applies to the host

`@saltcorn/builder` is about 17 400 lines of React 18 JSX. The parts that matter are
`Builder.js` (1 150), `Toolbox.js` (980), `storage.js` (870, the Craft node tree ⇄ v1 layout
JSON translation), `Library.js` (590), `elements/Container.js` (1 310), `elements/utils.js`
(2 170) and thirty element components. The last milestone did not reimplement `list.ts`, for a
reason that applies here with more force: the builder and the renderers form **one contract**.
A layout is correct if and only if `storage.js` writes it, the pattern or `renderLayout` reads
it, and the two agree. A TypeScript rewrite of `storage.js` would be a second writer of v1's
layout format, checked against a reader it does not own. Every drift would appear as a layout
that looks right in the canvas and wrong on the subdomain.

So it is **vendored, not rewritten, not depended on**, and the rules are the ones
`ui/saltcorn-ui/vendor/README.md` already states:

- `ui/builder/vendor/saltcorn-builder/` is `packages/saltcorn-builder/src/` taken at **the same
  commit as `ui/saltcorn-ui/vendor/`** (`@saltcorn/data` 1.7.0-alpha.1, saltcorn/saltcorn
  `0508c45ac2`). The builder and the renderers it writes for must never come from two versions.
  `refresh.sh` takes both from one checkout or refuses.
- Every file gets the two-line upstream header. **No vendored file is edited.** Behaviour that
  must differ goes in `ui/builder/src/`, as an alias, a shim or an injected module, each with a
  sentence saying why.
- `saltcorn-builder.css` and `fonticonpicker.react.css` come from `packages/server/public/` and
  CKEditor 4 from `packages/server/public/ckeditor/` (v1 ships 4.16.2), the same way
  `saltcorn.js` came for the view runtime.

**GOALS says "Use TypeScript for the React code", and this is a stated exception, not a
quiet one.** The exception is the vendored directory and only that directory. `ui/builder/src/`
is TypeScript, and every call it makes to this server goes through the generated typed client,
as GOALS requires ("Interactions with the Admin UI API must go through a typed Typescript
library consumer"). The vendored JSX calls this server through `fetch` and `href`, never
directly (§3). That is how the typed-client rule holds for code nobody here wrote.

It is bundled by **esbuild**, like `ui/saltcorn-ui`, not by v1's webpack + babel: JSX loader,
`react`/`react-dom` 18, one output file, one CSS file, with the dependency versions pinned to
v1's `package.json` at that commit. It is the **fourth** bundle `sc-cli`'s build script makes.

### 2. Where it runs: its own admin route, like the IDE, not inside the SPA

The builder is **not** a screen in `ui/admin`. It is a separate document at two routes,
`/builder/applications/:app/views/:view` and `/builder/applications/:app/pages/:page`, served by
the admin server under the admin session, the way `/ide/` is (§12.1). The reasons are
structural:

- **Its CSS and the admin UI's CSS cannot share a document.** The admin UI is Tabler over
  Bootstrap 5.3. The builder's canvas must render with the **same stylesheet the subdomain
  serves** (plain Bootstrap 5.3, Font Awesome 5.15, `saltcorn.css`, `saltcorn-builder.css`),
  or it stops being a WYSIWYG editor. `saltcorn-builder.css` is 1 000 lines of un-namespaced
  selectors.
- **It expects a v1 page around it.** It uses jQuery with Bootstrap's `.dropdown("toggle")`
  bridge (`JoinField.js`, `RelationOnDemandPicker.js`), `window.notifyAlert`,
  `window.ajax_modal`, `validate_expression_elem`, `window._sc_lightmode`, and it looks up
  `#saltcorn-builder`, `#scbuildform` and `#builder-header-actions` by id. v1's
  `saltcorn-markup/builder.ts` renders that page, and the host document is a port of it (§4).
- **Its CSP differs from the admin UI's, and a relaxation belongs to a route.** Details below.
  An SPA screen cannot have a different policy from the SPA.
- It weighs megabytes (Craft.js, CKEditor, Monaco, react-select, the icon picker), and only
  an admin building a layout should pay for that.

**Its Content-Security-Policy** is `BUILDER_CONTENT_SECURITY_POLICY`, beside
`IDE_CONTENT_SECURITY_POLICY` in `sc-server/src/security.rs`, served per response on the
builder's routes only. It starts from the admin UI's strict policy. The only relaxations are
the ones a failing test proves necessary, each written beside the constant with the reason. The
expected set is `style-src 'unsafe-inline'` (already in the admin policy: styled-components and
Craft's inline `style` props), `worker-src 'self' blob:` for Monaco's workers, and whatever
CKEditor 4's editing iframe needs. It gets **no third-party origin, no `'unsafe-eval'` unless
Monaco or CKEditor is proven to need it, and no `'unsafe-inline'` script**. The host document's
boot data is a JSON `<script type="application/json">`, not an inline script.
`@monaco-editor/react` loads Monaco from `/monaco` via its AMD loader, which is a CDN pattern
in all but name. It is aliased to a shim that hands the loader the ESM `monaco-editor` this
bundle already imports, with same-origin workers, the way `ui/admin/src/CodeEditor.tsx` does.
Nothing is fetched from `/monaco`.

**The routes answer only for a Saltcorn UI application and a view or page in it.** The view
route also requires a pattern whose current step is a builder step. Anything else is a 404
naming which condition failed. A page whose layout is an `html_file` is refused, naming it,
because that page has no layout to build. A binary built without the builder bundle serves a
short page saying so, and the admin screens keep today's read-only JSON (§9), so a
`SC_BUILD_ADMIN=0` build degrades rather than breaks.

### 3. The seam is every URL the vendored builder reaches, held to a table

v1's builder talks to v1's server through about twenty URL shapes. Some are `fetch` calls
(`/library/content/:id`, `/viewedit/savebuilder/:id`, `/field/preview/...`) and some are hrefs
it renders (`/viewedit/config/:name`, `/actions/configure/:name`, `/admin/help/:topic`). None of
those paths exist here, and some collide with paths that do (`/api/:table/distinct/:field`
against this server's own `/api/`). This is `plugin-helper.ts` again (TODO-post-mvp-24 §2): a
foreign module's contact surface with a server it was not written for. The answer is the same
one: **a partition, in one file, held by a test**.

`ui/builder/src/routes.ts` is that file. Every v1 URL shape is in exactly one of three columns:

| Column | What it does | Members |
|---|---|---|
| **mapped** | becomes a typed-client call, or a URL on this server | `POST /viewedit/savebuilder/:id` → `saveViewLayout` · `POST /pageedit/savebuilder/:id` → `savePageLayout` · `GET /library/content/:id` → `getLibraryItem` · `POST /library/savefrombuilder` → `createLibraryItem` · `POST /library/save-updates` → `saveLibraryUpdates` · `POST /field/preview/:table/:field/:fieldview` → `builderFieldPreview` · `GET /field/fieldviewcfgform/:table` → `builderFieldviewConfigForm` · `POST /view/:name/preview` → `builderViewPreview` · `POST /page/:name/preview` → `builderPagePreview` · `GET /api/:table/distinct/:field` → `builderDistinctValues` · `/files/serve/:id` → the application's file-store serve URL · `/viewedit/config/:name` → the admin wizard's hash route · `/pageedit/edit/:page` → the page builder route · `/view/:name` and `/page/:name` → the subdomain's URLs · `POST /crashlog/` → the browser console and a notice |
| **refused** | answers v1's error JSON (`{ error }`) with a sentence naming the feature and saying it is not in this version; hrefs render disabled with the same sentence as a tooltip | `POST /viewedit/copilot-generate-layout` · `POST /files/upload` · `/admin/help/:topic` · `/admin/ts-declares` · `/actions/configure/:name` · anything naming a page group |
| **unreachable** | the builder cannot reach it with the options this server sends (§5) | none initially; a URL moves here only with the option that makes it unreachable written beside it (e.g. `has_copilot_generate: false`) |

`/api/:table/distinct/:field` is v1's public row API, and the builder calls it from `Tabs.js`
(tabs generated from a field's distinct values) in every table mode. `builderDistinctValues`
answers the same `{ success: [...] }` as the **admin**, for a table in the application's
subset only. It is not a public route on the subdomain, and nothing here adds one.

**How the vendored code is made to go through it without editing it:** esbuild `inject`s a
module that exports `fetch`, so every free `fetch` identifier in `vendor/` resolves to
`builderFetch(url, init)`. That function matches the URL against the table, calls the typed
client and answers with a `Response`. `src/` code uses `globalThis.fetch` explicitly and is
not rewritten. Rendered hrefs are handled by one delegated click listener on the document that
matches `a[href]` against the same table: mapped hrefs navigate to their mapping, refused ones
show their sentence. An unknown URL is **refused, never passed through**, and the refusal names
the URL, so a v1 refresh that adds an endpoint fails visibly rather than 404ing into a blank
panel.

**The test** (`ui/builder/src/routes.test.ts`, vitest) walks every string and template literal
in `vendor/` that begins with `/` or is passed to `fetch`/`url:`/`href`, normalises template
holes to `:param`, and asserts that each lands in exactly one column. It is the
`bundle_shape` test's twin, and it is what makes `refresh.sh` safe to run.

### 4. The host document is a port of `saltcorn-markup/builder.ts`

v1's builder page is one function in `@saltcorn/markup`, and it is already vendored. It emits:
the bundle script, `ckeditor/ckeditor.js`, the two stylesheets, `div#saltcorn-builder`, a
`form#scbuildform` with hidden `contextEnc`, `stepName`, `columns`, `layout` and `_csrf`
inputs, `builder.renderBuilder("saltcorn-builder", options, layout, mode)`, and a `domReady` that
stubs `set_state_fields`, `set_state_field` and `pjax_to` and applies the dark theme. The host
document is **that output**, rendered by Rust, around a page chrome that v1's viewedit and
pageedit routes supplied and this server must supply itself:

- the application and the view or page name; for a view, the step name, "step *n* of *m*" and
  a **Back to configuration** link to the admin wizard; for a page, **Page properties** and
  **Back to pages** links;
- `#builder-header-actions`, where the builder portals its own header buttons;
- the globals it calls, from one host module (`src/globals.ts`), each commented with its v1
  origin. `notifyAlert` becomes a Bootstrap toast. `ajax_modal` is refused (§3's help topics).
  `validate_expression_elem` is v1's, taken from `saltcorn-common.js` (already vendored
  under `ui/saltcorn-ui/public/`). jQuery and `bootstrap.bundle.min.js` are served in that order
  from the Saltcorn UI assets, so `.dropdown("toggle")` works.

**`#scbuildform` does not post to a URL.** For a view, v1 posts it to the workflow route, which
decodes `columns` and `layout` into the context under the step's `contextField` and runs the
next step (`models/workflow.ts`, the `step.builder` branch of `run`). The host intercepts the
submit, reads the same two inputs, and calls `saveViewLayout` (§6) with them. On success it
navigates to the wizard at the next step, or to the view list if the layout was the last step,
which is what v1's **Next** does. For a page, v1's builder saves through `pageedit/savebuilder`
and its **Done** returns to the page list, so the host calls `savePageLayout` and returns to
the Pages tab. In both modes, the builder's autosave (`Library.js` `doSave` on blur, pagehide
and beforeunload) posts to `savebuilder` and lands on the same endpoint through §3, so an
abandoned tab keeps what it had, as it does in v1.

### 5. The builder's options are computed in the worker, by v1's code

`renderBuilder`'s `options` is the biggest object in the exchange: `fields` (each
`field.toBuilder`), `actions`, `triggerActions`, `builtInActions`, `actionConfigForms`,
`actionDescriptions`, `field_view_options`, `parent_field_list`, `child_field_list`,
`agg_field_opts`, `roles`, `min_role`, `library`, `views`, `pages`, `page_groups`, `images`,
`mode`, `tableName`, and so on. It comes from two places in v1, and both run on the worker here.
**Rust never assembles it and never looks inside it.**

**For a view, the pattern computes it.** Each pattern's builder step is an
`async (context) => options` function: `list.ts`, `show.ts`, `edit.ts` and `filter.ts`.
`Workflow.runStep` adds `fonts`, `icons`, `keyframes`, `join_field_picker_data`, `tables`,
`views` and `max_relations_layer_depth`. All of that code is vendored. So `config_step` for a
builder step stops answering "`builder: true`, no form" and answers **`builder_options`**: the
object v1 would have passed to `renderBuilder`, computed by v1's code, as the admin, over the
application's snapshot.

**For a page, v1's server route computes it.** `pageBuilderData` is in
`server/routes/pageedit.ts`, not `@saltcorn/data`, so there is nothing to vendor. It is
**ported** into `ui/saltcorn-ui/src/builder-routes.ts` as `page_builder_options`, headed with its
upstream path, and reached through a new `ViewRuntime` method. Everything it reads it reads
from the snapshot:
- the application's views (as `select_option`), its pages and its images;
- the roles;
- the actions that do not require a row, plus `GoBack` and the application's triggers whose
  event is *API call* or *Never*, with their config forms and descriptions;
- the library items `suitableFor("page")`;
- `fixed_state_fields` per view (from `view.get_state_fields`, which the `View` shim already
  has);
- `page_name`, `page_id` and `mode: "page"`.

`page_groups` is `[]`, `translations` is `{}` and `isRTL` is `false`.

The work is making the code they call answer instead of refuse. Today these are named
refusals or inert stubs, and each moves to *implemented* with its v1 upstream cited:

- `table.get_join_field_options`, `table.get_relation_options` and
  `table.get_relation_data` (in `v1_api.js`'s `BUILDER` group), and
  `table.get_child_relations` / `get_parent_relations` if the Filter step reaches a form the
  shim lacks. They are pure over the schema: v1's `models/table.ts` walks fields and key
  references, and `common-code/relations` (vendored) does the relation search. They are ported
  over the `SchemaSnapshot`, restricted to the **application's table subset**, so the builder
  never offers a join to a table the app cannot see.
- `plugin-helper`'s `build_schema_data`, refused today because v1 builds it with `db`. It is
  rebuilt from the same snapshot, subset-restricted. It moves from *refused* to *kept*, and the
  partition test is updated in the same change.
- `File.findImagesForBuilder` (the `FILES` group): the image files in the application's file
  stores, as v1's `{ id, filename, location }` shape, where `location` is §3's serve URL.
  (Filter's own step sends `images: []`, v1's "temp fix till we rebuild builder", and that is
  kept as v1 wrote it.)
- `PageGroup.find` answers `[]`. `getState().fonts`, `icons` and `keyframes` answer v1's
  built-in defaults. `getState().keyFieldviews` already answers, which gives Filter its
  `has_select2`. `getState().functions.copilot_generate_layout` is absent, so
  `has_copilot_generate` is `false` and the copilot button is never drawn.
- `Library.find` and `Library.findOne` answer from the snapshot (§8).
- `field.toBuilder` and `table.getFields` for key fields with `reftable` loaded, if the
  `Field` shim lacks them.

The options are **golden-tested against v1 itself** (§13). The fixtures are what a real
Saltcorn 1.7.0-alpha.1 passes to `renderBuilder` for *Show Books*, *Edit Books*, *List Books*
and *Filter books*, and for the page *BooksOverview*, over the BooksDB backup. They are
recorded once by a script committed beside them. That is the test that catches a shim returning
a plausible wrong thing, which a round trip through our own code cannot.

### 6. Saving a layout is a save, and is checked like one

**`saveViewLayout`** (`PUT /api/applications/:id/views/:name/layout`) takes `{ step, columns,
layout, libraryUpdates? }` and does what v1's two view save paths do between them:

- It merges `{ columns, layout }` into the configuration under the step's `contextField`, else
  at the top level. That is the `workflow.ts` builder branch (the wizard's Next). v1's
  `viewedit/savebuilder` spreads the whole body into the configuration, which is the same thing
  for the four patterns in scope, none of which has a `contextField` on its builder step. The
  one merge rule serves both.
- It saves through the same `save_view` path as `saveView`, so **every existing save check
  runs**. That includes the replay of the non-builder steps (a layout that changes what a later
  step's form would accept is refused naming that step), the action resolution (a layout
  naming an action the app does not declare is refused naming it), the table subset for
  embedded views and join fields, and `min_role`.

**`savePageLayout`** (`PUT /api/applications/:id/pages/:name/layout`) takes `{ layout,
libraryUpdates? }` and replaces the page's layout, which is all v1's `pageedit/savebuilder`
does. It saves through the same path as `savePage`, so the page's existing checks run: every
embedded view exists in the application. It adds the view checks that apply to a page: an
`action` segment names v1's page actions or a trigger the application declares, and a view link
or embed names one of the application's views.

**Both** do the following:

- Refuse a `library` segment whose `library_id` is not an item of this application, naming the
  id.
- Apply `libraryUpdates` (`[{ library_id, layout }]`, v1's in-place edits to a shared
  component) **in the same transaction as the view or page**, so a refused save does not leave
  its library edits half-applied. v1 does them one after another, last write wins. The
  transaction is the one improvement taken, because this server has one.
- Bump the view-set generation once, so the subdomain serves the new layout from its next
  request.

**No layout normalisation, in either direction.** What `storage.js` wrote is what is stored. A
Craft round-trip test (§13) guards the loading direction: every view and page layout in the
BooksDB fixture, loaded into the canvas and saved without a change, is byte-for-byte what it
was, apart from a listed and justified set of differences `storage.js` itself introduces (key
order, defaulted props), each named in the test.

### 7. Pages get the rest of v1's page editor, and what a built page needs to run

Building a page's layout is not enough to make pages first-class. v1's `pageedit.ts` also
creates pages and edits their properties, and a page layout can contain things the renderer
here does not yet answer.

**Properties.** v1's `pagePropertiesForm` is name, title, description, minimum role,
`html_file`, `no_menu` and `request_fluid_layout`. All of them except `html_file` (*Explicitly
OUT*) go into a **page properties form** in the admin UI, saved through the existing
`savePage`. `no_menu` and `request_fluid_layout` live in the page's `attributes`, where the v1
import already puts `no_menu`. **New page** is that form, and **Create** opens the new page in
the builder, which is v1's redirect to `/pageedit/edit/:name`. A new page's layout is `{}`, as
in v1. Renaming a page shows what refers to it (menu entries, home page per role, links in
layouts) before confirming, the way renaming a view does.

**The document honours the properties.** `no_menu` omits the navbar and `request_fluid_layout`
uses a fluid container. Both are passed into `emergency_layout`'s wrap the way v1's `page.ts`
passes them to the layout's `sendWrap`. Today both are imported and ignored.

**`POST /page/:name/action/:rndid`.** An `action` segment in a page layout renders as
`page_post_action('/page/<name>/action/<rndid>')`. The runtime already emits that, and the
framework has no such route, so every action button on a page 404s. The route is ported from
v1's `routes/page.ts`, implemented in the worker, and follows the framework's existing
view-route rules:
- the page's `min_role` against the viewer, with the same redirect-or-403 as rendering;
- a CSRF check;
- find the `action` segment by `rndid` in the layout, **including inside resolved library
  items**;
- run it with the vendored `run_action_column` under the viewer's authority, inside a
  transaction;
- answer v1's `{ success: "ok", ... }`, `{ error }` with 400, or 404 "Action not found".

**Fixed state, in one form.** v1 has two spellings of an embedded view's fixed state: the
modern one, `configuration` on the `view` segment, which the builder writes, and the legacy
`page.fixed_states[segmentName]`, which v1's `getEditNormalPage` folds into the segments before
opening the builder. The v1 import currently copies `fixed_states` into `attributes`, and the
renderer here reads neither. So the **import** does v1's fold once: each `view` segment with
`state: "fixed"` and no `configuration` gets the legacy entry, and `fixed_states` is not kept.
From then on there is one spelling, the builder's, and the renderer reads it. There is no
fallback reader for the old spelling, because nothing on this server ever wrote it.

**A page embedded in a page.** The builder's *Page* element (`Page.js`) embeds another page and
previews it through `/page/:name/preview`. The runtime's `renderLayout` must render a `page`
segment the way v1's `Page.run` does, with the same depth cap as embedded views, naming the
cycle it breaks. If it does not already, that is a task, and there is a test either way.

### 8. The library: per application, belonging to Saltcorn UI, and v1's model everywhere else

A library item is v1's `_sc_library` row, `{ name, icon, layout }`, and placing one in a layout
writes `{ type: "library", library_id, slots }` (or, from older v1 builders, a copied layout).
Current v1 calls these **shared components**: editing inside a placed instance saves back to
the item, so everything using it updates. A **slot** (`library-slot`) is a hole that each
placement fills independently, with a field and fieldview or with dropped-in content. At render
time `Library.resolveSegment` swaps the reference for the item's layout, fills the slots and
guards against an item that contains itself. `show.ts` and `edit.ts` call it on every `library`
segment. Pages and filters must too: the runtime's page render and `filter.ts`'s layout walk
resolve `library` segments as v1's do, checked by the golden tests either way. Which toolbox
offers which item is v1's `suitableFor(mode)`, for all five modes.

**Storage: `_fd_library`, in `sc-viewpattern`, beside `_fd_views` and `_fd_pages`, with the
same rules** (TODO-post-mvp-24 §1): the §9 required columns (UUID `id`, `name`, `description`,
`attributes`), plus `application`, `icon` and `layout` (JSON, v1-shaped, stored untouched). It
is unique on (`application`, `name`). `application` is not a foreign key, and deleting the
application deletes its library in `delete_application_views_and_pages`, which is renamed to
say so.

**Per application, not global, and not shared across applications.** A library item's layout
names fields, join paths, views, pages and actions, and each of those means something only
inside one application's table subset and view set. A global item would place a join to a
table the application cannot see, or a link to a view it does not have. That is exactly the
argument that made views per-application, and it lands the same way here. Copying an item to
another application is a plausible follow-up and is out (*Explicitly OUT*).

**Only Saltcorn UI applications have one.** Every library write refuses an application whose
framework is not `saltcorn-ui`, naming the framework. The admin UI only draws the Library tab
for such an application, and nothing outside `sc-viewpattern` and the Saltcorn UI admin screens
knows the table exists. It is not an overlay, not a module facility and not visible to agents'
table traits, because it is framework storage, like `_fd_views`.

**In the worker: `models/library.ts` is vendored, not shimmed.** It is 230 lines, and all but
four of them are pure: `suitableFor` and `resolveSegment`. Reimplementing `resolveSegment`
would be a second copy of v1's slot-filling rules, so it is not. The file is added to
`vendor/saltcorn-data/models/` and removed from `build.mjs`'s `HOST_DATA_MODULES`. Its one
`../db/index.js` import is resolved **for that importer only** to `src/shims/library-db.ts`,
which answers `db.select("_sc_library", …)` and `db.selectMaybeOne("_sc_library", …)` from the
`ViewSnapshot`'s library list and refuses every write by name. The worker never writes the
library: `create`, `update`, `delete` and `saveLibraryUpdates` belong to the admin API (§6,
§10). The `ViewSnapshot` gains `library`, and a library write moves the generation like a view
write.

**v1's integer ids become UUIDs, and that is one of the two translations the import makes.**
`library_id` in a v1 layout is `_sc_library.id`, a serial and a meaningless number on this
server. The v1 restore (§11) mints a UUID per item and rewrites `library_id` in every imported
view's and page's layout, and in every imported item's own layout (nested items). The
alternative, an integer column kept forever so untranslated layouts keep resolving, is
backwards-compatibility code for data this server has never held, which CLAUDE.md rules out.
It and §7's fixed-state fold are the two places TODO-post-mvp-24 §1's "configuration stored
untouched" gives way, which is why both are written down here and in the restore's module
comment.

**References.** A view's or page's references (`viewReferences`, and the rename and delete
warnings) include the library items its layout places. A library item's references are the
views and pages whose layouts place it, found by walking layouts in the snapshot. Deleting an
item with references shows them and asks. Once deleted, a reference renders blank, which is
v1's own `resolveSegment` behaviour and deliberately not a hard error: a missing shared
component must not take a working page down.

### 9. The admin UI: views and pages open the builder, and the application gets a Library tab

- **The layout step in `ViewEditor.tsx`** shows **Open in builder** as its primary action, with
  the JSON underneath, collapsed. The button navigates to §2's view route with the step index.
  `LAYOUT_READ_ONLY`'s sentence goes, and a binary without the builder bundle keeps a variant of
  it that says why.
- **Creating a view whose first unskipped step is a builder step** (Show and Filter always, and
  Edit and List when every step before the layout is skipped) lands in the builder directly
  after `createView`, the way v1's "Configure" does.
- **The wizard returns to the right place.** §4's navigation back lands on
  `#/applications/:id/views/:name/configure?step=n`. The wizard reads `step` and reloads the
  configuration, because the builder saved it.
- **The Pages tab** gains **New page** (§7's properties form, then the builder), and each row
  gets **Edit** (opens the builder), **Properties** (the form) and a rename with references.
  Delete and the existing *Home page for* column stay as they are. The tutorial's
  browser-console step goes.
- **A Library tab** on a Saltcorn UI application, beside Views and Pages: name, icon, "used by"
  (expandable to the views and pages, each linking to its editor), rename, delete (§8's
  references first), and a read-only view of the item's layout JSON. Items are **created and
  edited in the builder** (*Save as library component*, and editing a placed instance), as in
  v1, whose `/library/list` has no editor either. This tab is where an admin finds and tidies
  them.
- All of it goes through the regenerated typed client, and each screen's logic that is not
  JSX (the step index, the landing decision, the properties form's validation, the references
  expansion) lives in a `.ts` beside it with a vitest file, the way `views.ts` does now.

### 10. The admin API grows by the builder's calls, the page editor's, and the library's

Every endpoint is declared in `sc-api/src/admin.rs` with its schemas, so it lands in the
generated client and the OpenAPI document. The `ui/builder` host uses nothing that is not
there. All are admin-only, CSRF-checked, and scoped to an application that must be a Saltcorn
UI application. Creating a page and editing its properties need nothing new: `savePage`
already creates and updates. The builder's options are not an endpoint, because the builder
route renders them into the document's boot data (§4, §5).

| Operation | Path | Does |
|---|---|---|
| `saveViewLayout` | `PUT /api/applications/:id/views/:name/layout` | §6 |
| `savePageLayout` | `PUT /api/applications/:id/pages/:name/layout` | §6 |
| `pageReferences` | `GET /api/applications/:id/pages/:name/references` | what names the page (menu, home page per role, layouts), for rename and delete |
| `listLibrary` | `GET /api/applications/:id/library` | items with `used_by` |
| `getLibraryItem` | `GET /api/applications/:id/library/:item` | one item, fresh (v1's `/library/content/:id`: a placed instance must start from the latest layout, not the page-load snapshot) |
| `createLibraryItem` | `POST /api/applications/:id/library` | v1's `savefrombuilder`: `{ name, icon, layout }` → the new id; a duplicate name is refused naming it |
| `saveLibraryItem` | `PUT /api/applications/:id/library/:item` | rename, icon, description |
| `saveLibraryUpdates` | `POST /api/applications/:id/library/updates` | v1's `save-updates`, for the builder's standalone call; transactional over the batch |
| `deleteLibraryItem` | `DELETE /api/applications/:id/library/:item` | with `references` in the answer's refusal unless `?confirm=true` |
| `builderFieldPreview` | `POST /api/applications/:id/builder/field-preview` | a fieldview rendered over the first readable row (v1's `routes/fields.ts` `/preview`), in the worker |
| `builderFieldviewConfigForm` | `POST /api/applications/:id/builder/fieldview-config` | a fieldview's `configFields` as v1's form JSON (v1's `/field/fieldviewcfgform`), in the worker |
| `builderViewPreview` | `POST /api/applications/:id/builder/view-preview` | an embedded view rendered for the canvas with the state given, as the admin, in the worker (v1's `/view/:name/preview`) |
| `builderPagePreview` | `POST /api/applications/:id/builder/page-preview` | an embedded page rendered for the canvas, as the admin, in the worker (v1's `/page/:name/preview`) |
| `builderDistinctValues` | `GET /api/applications/:id/builder/distinct/:table/:field` | §3: v1's `{ success: [...] }`, subset-restricted, as the admin |

The `builder*` calls are **v1 server route code**, so they are ported beside
`page_builder_options` in `ui/saltcorn-ui/src/builder-routes.ts`, each function headed with the
upstream route it ports, and reached through new `ViewRuntime` methods. The previews' output is
HTML for the canvas. It is what the subdomain would render, and the builder shows it inside its
preview scratchpad, which is how v1 does it too.

### 11. The import, and this server's own backup

- **v1 restore** (`backup::v1`) stops noting `library` as not imported. The pack's library
  entries go into the one application the backup becomes, with §8's id rewrite applied to
  items, views and pages together, before any is saved. Pages get §7's fixed-state fold. A
  library item whose name collides on re-import is replaced, the same rule as views. The report
  gets a line, `n library items into application x`. Page groups are still noted as not
  imported.
- **This server's backup** carries `applications/<subdomain>/library.json` beside `views.json`
  and `pages.json`, under the same "views" choice, restored in the same replace-not-append way,
  and **before** the views and pages, so the save checks in §6 find the items they place.

### 12. Five modes, allow-listed; a plugin's mode is not claimed

The builder routes allow **`show`, `edit`, `list` and `filter`** for views and **`page`** for
pages, and refuse any other mode, naming it. A plugin pattern whose workflow has a builder step
goes through the same `config_step` and would reach the builder in whatever mode its options
name. This milestone neither builds that nor tests it. When one arrives, it is an allow-list
entry plus its §3 URLs, not a surprise.

### 13. Tests

- **Rust, over real Postgres:**
  - `_fd_library`: bootstrap and round trip, per-application uniqueness, the framework
    refusal, application delete cascading;
  - `saveViewLayout` and `savePageLayout`: merge and replace rules, each refusal naming what
    it names (unknown action, unknown library id, table outside the subset, missing embedded
    view, a later step that no longer accepts), and the transaction (a refused save leaves
    `libraryUpdates` unapplied);
  - references in every direction, and page rename references;
  - the backup round trip;
  - v1 restore with the id rewrite (including a nested item) and the fixed-state fold.
- **The worker:**
  - the five option goldens against the fixtures recorded from v1 (§5), key for key, with any
    intended difference (images' `location`, `has_copilot_generate`, `translations`) listed in
    the test with its reason;
  - `get_join_field_options`, `build_schema_data` and `builderDistinctValues` refusing to see
    outside the subset;
  - `resolveSegment` in a Show, an Edit, a Filter and a page, with a field slot and a content
    slot, rendering the same golden HTML as the equivalent inline layout; a self-containing item
    rendering blank;
  - a page embedding a page, and the cycle refused;
  - the `builder*` routes' output against goldens.
- **HTTP on the subdomain:**
  - `POST /page/:name/action/:rndid`: success, an action inside a placed library item, the
    404, the role check, CSRF;
  - `no_menu` and `request_fluid_layout` in the document;
  - a page built from nothing with a filter and a shared-state list, driven: search, clear,
    the action.
- **HTTP on the admin server:**
  - the builder routes' answers: the document for each of the four patterns and for a page;
    the 404s naming application, view, page or step; the refused mode; the refused `html_file`
    page; the no-bundle page;
  - their CSP header.
- **The partition tests:**
  - §3's URL table (vitest);
  - `bundle_shape` for `build_schema_data` moving to kept and `models/library` moving from host
    to vendored;
  - a globals test that the host document defines every global in §4's list.
- **The builder bundle, in jsdom** (v1's builder package already runs under `jsdom` +
  `react-test-renderer` in its own tests): mounting `Builder` with each recorded options object
  does not throw, and the **Craft round trip** of §6 holds for every view and page layout in
  the fixture.
- **`ui/admin` vitest** for §9's logic.
- **The definition of done, by hand**, in a real browser, with the console open. A CSP
  violation report counts as a failure.

---

# The work

## Phase 1 — The library, stored

- [x] 1.1 `_fd_library` in `sc-viewpattern/src/tables.rs`: the §9 columns, `application`,
      `icon`, `layout`, the (`application`, `name`) key, bootstrapped with the other two.
- [x] 1.2 `LibraryItem` and `LibraryItemId`; `save_library_item` / `load_library_item` /
      `list_library` / `delete_library_item` / `apply_library_updates` (transactional), refusing
      a non-Saltcorn-UI application naming its framework, and a duplicate name naming it.
- [x] 1.3 Deleting an application deletes its library; rename
      `delete_application_views_and_pages` to say so.
- [x] 1.4 `ViewSet` and `ViewSnapshot` carry the library; a library write moves the generation.
- [x] 1.5 References: the items a view's or page's layout places (a layout walk over `library`
      segments, including inside nested items), and the views and pages that place an item.
- [x] 1.6 Live tests: round trip, uniqueness per application, framework refusal, cascade,
      references both ways, generation bump.

## Phase 2 — The library, rendered

- [x] 2.1 Vendor `models/library.ts` (header, `refresh.sh` entry); remove `models/library` from
      `HOST_DATA_MODULES`.
- [x] 2.2 `src/shims/library-db.ts`: reads from the snapshot, every write refused by name,
      resolved for `models/library.ts`'s import only (an esbuild `onResolve` keyed on the
      importer, with a test that no other vendored file reaches it).
- [x] 2.3 `bundle_shape` updated for the move; the snapshot's library reaches `getState()` and
      `Library.find`/`findOne` answer from it.
- [x] 2.4 `library` segments resolved in a page's render and in `filter.ts`'s layout, as v1
      does (verify against v1's `models/page.ts` and `filter.ts`; add the call where the runtime
      lacks it).
- [x] 2.5 Golden tests: a Show, an Edit, a Filter and a page placing an item with a field slot
      and a content slot render the same HTML as the inline equivalent; a missing item and a
      self-containing item render blank.

## Phase 3 — Pages, running what a built page contains

- [x] 3.1 `POST /page/:name/action/:rndid` (§7): routed by the framework, run in the worker with
      `run_action_column` under the viewer's authority in a transaction, `min_role`, CSRF, the
      segment found inside resolved library items too, v1's three answers.
- [x] 3.2 `no_menu` and `request_fluid_layout` from the page's `attributes` into
      `emergency_layout`'s wrap.
- [x] 3.3 A `page` segment embedded in a page renders under the depth cap, naming a cycle
      (verify; implement if missing).
- [x] 3.4 Tests: the action route's answers; the two properties in the document; an embedded
      page and a page cycle.

## Phase 4 — The library and pages, imported and backed up

- [x] 4.1 `backup::v1`: import `library` into the application; mint UUIDs and rewrite
      `library_id` in item, view and page layouts (nested items included) before saving; remove
      the "not imported" note for it; the report line; replace on re-import.
- [x] 4.2 `backup::v1`: §7's fixed-state fold into pages' `view` segments; stop copying
      `fixed_states` into `attributes`.
- [x] 4.3 A fixture: extend the BooksDB pack (or add a second small v1 pack) with a library item
      that has slots and is placed in a Show view and a page, and a page with legacy
      `fixed_states`, so the rewrite and the fold have something real to work on. Record how it
      was made beside the fixture.
- [x] 4.4 This server's backup: `applications/<subdomain>/library.json`, restored before views and
      pages, replace-not-append.
- [x] 4.5 Tests: the v1 import with the rewrite and the fold, rendered on the subdomain; the
      backup round trip.

## Phase 5 — The builder's options, from the worker

- [x] 5.1 `ConfigStep` gains `builder_options: Option<Json>`, filled for a builder step by
      `module-host.mjs`'s `view_config_step` running the step's `builder(context)` plus
      `Workflow.runStep`'s additions, as the admin; `ui/saltcorn-ui/src/index.ts` answers it.
- [x] 5.2 Port `table.get_join_field_options`, `get_relation_options` and `get_relation_data`
      over the `SchemaSnapshot`, restricted to the application's subset; off the `BUILDER`
      refusal list, each citing `models/table.ts`.
- [x] 5.3 `build_schema_data` from the snapshot, subset-restricted; moved to *kept* in the
      `plugin-helper.ts` partition.
- [x] 5.4 `File.findImagesForBuilder` over the application's file stores; `PageGroup.find` → `[]`;
      `getState().fonts`/`icons`/`keyframes` defaults; `copilot_generate_layout` absent; any
      `Field`/`Table` member the four builder steps reach that the shims lack (found by running
      them, Filter's `get_child_relations`/`get_parent_relations(true)` included, each added
      with its upstream cited).
- [x] 5.5 `page_builder_options` in `builder-routes.ts`, ported from `pageBuilderData`, and its
      `ViewRuntime` method.
- [x] 5.6 The v1 recording script (`crates/sc-server/tests/fixtures/record-builder-options.*`)
      and its five fixtures (four views, one page), recorded against a Saltcorn 1.7.0-alpha.1
      over the BooksDB backup.
- [x] 5.7 Tests: the five goldens with their listed differences; subset restriction; a
      non-builder step still answers `builder_options: null`.

## Phase 6 — The admin API

- [x] 6.1 `saveViewLayout` and `savePageLayout` (§6): the merge and replace rules, the existing
      save checks, the page's action and view checks, the library-id check, `libraryUpdates` in
      the same transaction, one generation bump.
- [x] 6.2 `pageReferences`, and page rename through `savePage` refusing nothing but reporting
      what refers to the old name, as view rename does.
- [x] 6.3 The library endpoints (§10): `listLibrary`, `getLibraryItem`, `createLibraryItem`,
      `saveLibraryItem`, `saveLibraryUpdates`, `deleteLibraryItem` with references.
- [x] 6.4 The `builder*` endpoints: `ViewRuntime` gains field preview, fieldview config form,
      view preview, page preview and distinct values; `builder-routes.ts` ports v1's routes;
      `ModuleViewRuntime` implements them.
- [x] 6.5 Regenerate `ui/admin/src/client.ts` (and the builder's copy of it, or a shared import;
      decide in 7.1 and say which).
- [x] 6.6 Tests: every refusal naming its subject; the transaction; the previews' and distinct
      values' goldens; a non-Saltcorn-UI application refused on every endpoint.

## Phase 7 — `ui/builder`: vendored and bundled

- [x] 7.1 `ui/builder/`: `package.json` pinned to v1's builder dependency versions at the
      vendored commit, `tsconfig.json` for `src/`, `build.mjs` (esbuild: JSX, the `fetch`
      inject, the aliases, one JS and one CSS output), `vendor/README.md`.
- [x] 7.2 `ui/builder/vendor/refresh.sh`: copies `packages/saltcorn-builder/src/` and the CSS and
      CKEditor assets with headers; refuses a checkout at a different commit from
      `ui/saltcorn-ui/vendor/`'s.
- [x] 7.3 Shims in `src/shims/`, each with its reason: `@monaco-editor/react` onto bundled ESM
      Monaco with same-origin workers; anything else the first build or the jsdom mount shows
      reaching outside the document's origin.
- [x] 7.4 `src/routes.ts` (§3) and `builderFetch`; the delegated href listener; `routes.test.ts`
      walking the vendored literals.
- [x] 7.5 `src/globals.ts` (§4) and its test.
- [x] 7.6 `crates/sc-cli/build.rs`: the fourth bundle; `SC_BUILD_ADMIN=0` records none;
      `scripts/build-static.sh` and the static-build Dockerfile carry it.
- [x] 7.7 jsdom tests: the mount with each recorded options object; the Craft round trip over
      every BooksDB view and page layout, with the listed normalisations.

## Phase 8 — The builder routes

- [x] 8.1 `/builder/applications/:app/views/:view?step=n` and
      `/builder/applications/:app/pages/:page` in `sc-server`: admin session, §2's 404s, the
      `html_file` refusal, §12's mode allow-list, the no-bundle page.
- [x] 8.2 The document (§4): v1's `builder.ts` output rendered in Rust around the page chrome for
      each mode, boot data as JSON (application id, view or page name, step and step count for
      a view, CSRF token, options, layout, mode), the Saltcorn UI stylesheets and scripts in v1's
      order, the builder bundle, and CKEditor.
- [x] 8.3 `BUILDER_CONTENT_SECURITY_POLICY` in `security.rs`, served per response on the routes
      and their assets, each relaxation justified in its comment by the test that needed it.
- [x] 8.4 `#scbuildform`'s submit → `saveViewLayout` → the wizard's next step, or →
      `savePageLayout` → the Pages tab; autosave through the same calls; a refused save shown
      with `notifyAlert` and the canvas kept.
- [x] 8.5 The builder's static assets (bundle, CSS, CKEditor) served from the builder `dist`
      under a versioned prefix, like `/static_assets/:tag/`; and `/files/serve/*` on the
      builder's origin redirected to the application's, because an image `src` or CSS `url()` the
      builder renders does not pass through the link listener (7.4, `routes.ts`).
- [x] 8.6 HTTP tests: the document for each of the four patterns and a page, the 404s, the
      refused mode and page, the CSP header, the no-bundle page.

## Phase 9 — The admin UI

- [ ] 9.1 `ViewEditor.tsx`: **Open in builder** on a builder step, the collapsed JSON, the
      no-bundle variant; `step` in the hash route honoured on return. The builder route (8.2)
      links to and saves back to `#/applications/:id/views/:name?step=n`, which `App.tsx`'s
      route match does not accept yet (it matches the whole hash, query included).
- [ ] 9.2 Creating a view whose first unskipped step is a builder step lands in the builder.
- [ ] 9.3 The Pages tab: **New page** (the properties form → the builder), **Edit**,
      **Properties**, rename with `pageReferences`; `pageForm.ts` for the form's validation
      (name required and unique in the application, the role list) with vitest. The page
      builder's **Page properties** link is `#/applications/:id/pages/:name/properties`.
- [ ] 9.4 The **Library** tab: list with icon and `used_by`, rename, delete with references,
      read-only layout; hidden for non-Saltcorn-UI applications.
- [ ] 9.5 View and page rename and delete warnings include the library items a layout places.
- [ ] 9.6 vitest for the step-index, landing and references logic.

## Phase 10 — Documentation and the definition of done

- [ ] 10.1 `docs/TECHNICAL_DESIGN.md`: §13.3's Saltcorn UI section gains the builder, the page
      editor and the library (what is vendored, the routes and their CSP, the URL partition,
      where the options come from, the page action route, the library's per-application storage,
      the import's id rewrite and fixed-state fold); the `ui/builder` bullet and the repository
      tree say what was built; `_fd_library` in §9's table.
- [ ] 10.2 `docs/tutorial-saltcorn-ui.md`: remove "the builder is not here" and the
      browser-console page; Part B's List, Show, Edit and a new Filter built in the builder, with
      the Show/Edit/Delete columns added there instead of through `saveView`; the page built from
      the Pages tab with the filter and list on it; a section on the library (save a component,
      place it, slots, edit in place, the Library tab); Part A gains "open an imported view and
      page in the builder".
- [ ] 10.3 `README.md` and `docs/OPERATIONS.md`: the fourth bundle, what a build without it does,
      the builder routes' CSP.
- [ ] 10.4 The definition of done, run by hand against a real server in a real browser with the
       console open, written up below with whatever it found.

---

## Explicitly OUT of scope for this milestone

- **Page groups.** A named set of pages and a rule choosing one by screen width, role or random
  split (A/B testing). They sit on top of pages: a second resolution step in front of
  `/page/:name`, plus an editor for the members and their conditions. Pages are complete without
  them, so the builder's `page_groups` is `[]`, a URL naming one is refused (§3), and the v1
  import keeps noting them as not imported.
- **HTML-file pages** (v1's `html_file` property). The page's content is an HTML file from the
  file store, edited in v1 with an iframe preview rather than the builder. The runtime already
  refuses them by name, and the properties form does not offer the field. It needs a file-store
  picker and a decision about what CSP such a page is served under.
- **Generate layout with copilot.** v1 calls a `copilot_generate_layout` function supplied by a
  plugin. The equivalent here is an agent with a layout-writing tool (§11.3), a design of its
  own. `has_copilot_generate` is `false` meanwhile.
- **Uploading an image from the builder.** `/files/upload` is refused. Images already in the
  application's file stores are offered, and a file is uploaded through the file manager. It
  needs a choice of which store an upload lands in.
- **v1's help topics** (`/admin/help/:topic`, 47 markdown files in v1's server). Vendoring them
  is easy; rendering them in a modal under the builder's CSP and keeping them true of this
  server is not.
- **TypeScript completions in the builder's formula editors** (`/admin/ts-declares`). The
  editors work without completions. This server already generates row types for code bodies,
  so this is a mapping, not an invention.
- **Replacing CKEditor 4.** v1 ships 4.16.2, and CKEditor 4 is end-of-life as open source.
  Vendoring it keeps v1's Text element working. Replacing it is a change to v1's builder, and
  belongs upstream first.
- **A menu editor.** v1's pageedit has *Add to menu* and the menu is edited in its own builder.
  Here the menu stays the application's JSON setting, as it is today. It is a form over v1's
  `menu_items` shape plus an "add this page" shortcut, and it is independent of the canvas.
- **Cloning a page or a view.** v1 has both. Cloning is a copy with a new name plus a references
  question (does the copy's layout still point at the original's embedded views?), and it is
  easy to add once the editors exist.
- **Sharing a library item between applications**, or copying one. §8 says why an item is
  per-application. A copy action that checks the target's subset is the likely shape.
- **Several people building one layout at once.** v1's model is last write wins, and so is this
  one, apart from §6's transaction. Realtime collaboration is a message-bus feature (GOALS),
  not a builder feature.
- **i18n of the builder's strings.** `useTranslation` answers the identity, as `__` does in
  the view runtime, and `translations` is `{}`.
- **The builder in a plugin pattern's mode** (§12).

## Carried past this milestone

- From TODO-post-mvp-24: `room`/`workflow-room` and realtime, tags, file upload from an Edit
  view, themes as plugins, i18n, a v1 `db` module for plugins, and externalising inline
  handlers to drop `'unsafe-inline'` from Saltcorn UI's CSP. That last one now also covers
  `page_post_action` and any inline handler the builder's preview HTML carries, since the
  preview is what the subdomain would render. Page groups move from that list to this
  milestone's *Explicitly OUT*, with the reason stated there.
