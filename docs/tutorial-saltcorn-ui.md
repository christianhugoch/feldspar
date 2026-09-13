# Tutorial: Saltcorn UI — Saltcorn 1's views, on this server

Saltcorn 1 built applications out of **views** — a List, a Show, an Edit, a Filter over a table —
arranged on **pages**, with a menu across the top. Saltcorn UI is that experience on this server:
an application whose framework is **Saltcorn UI** owns a set of views and pages, and the server
renders them on the application's subdomain. The HTML is v1's: the view patterns are Saltcorn
1's own source code, run on the JavaScript worker inside the `feldspar` process, not a
re-implementation of them.

There is nothing to build. No project directory, no bundler, no Build button: saving a view is
deploying it.

This tutorial goes two ways. **Part A** restores a Saltcorn 1 backup and ends with its views
running. **Part B** starts from nothing — a table, a List, a Show, an Edit, a page, a menu and a
link that works. They are independent; read the one you need.

**What is not here yet**, so that nothing below surprises you: the **drag-and-drop builder**. A
view's layout — which columns a List shows and in what order, what a Show lays out where — is
shown read-only, and an imported view keeps the layout it came with. Everything else about a view
is edited in the admin UI. Pages have no editor at all yet (Part B, step 6 makes one from the
browser console). The builder is the next milestone.

## Prerequisites: how you start the server

An application is served on its own subdomain, so the server needs a base domain:

```bash
feldspar serve --base-domain localhost
```

The admin UI is at `http://localhost:3032` and an application with subdomain `booksdb` at
`http://booksdb.localhost:3032` — browsers resolve `*.localhost` to 127.0.0.1.

The binary must have been built **with** its front-end bundles (the default). Saltcorn UI's view
runtime is one of them, and a binary built with `SC_BUILD_ADMIN=0` or `build-static.sh --no-ui`
has none: every Saltcorn UI application then fails to mount, with a line at boot naming the
application and the missing bundle, and there is no flag that supplies it afterwards (README §6).

---

## Part A — Restore a Saltcorn 1 backup

This uses `saltcorn-v1-BooksDB.zip` from the repository's test fixtures
(`crates/sc-server/tests/fixtures/`): three tables — Books, Authors, Publishers — two books, seven
views and one page. Any v1 backup (**Settings → Backup** in Saltcorn 1) goes the same way.

### Step A1 — Restore it

**Backup → Restore**, and choose the zip. The dialog reads the archive before anything happens
and says what it is — *Saltcorn 1.7.0-alpha.1, imported* — and what is in it: the tables and
their rows, the file store, the users, the triggers, **one application**, **7 views** and **1
page**. Leave everything ticked and restore.

### Step A2 — Read the report

It is two lists, what was restored and what was not, and the second one is worth reading rather
than skimming. For BooksDB it says:

- **restored:** the three tables, their columns and rows, the file store and its file, one
  account, `application booksdb`, `7 views into application booksdb`, `1 page into application
  booksdb`, and **`application booksdb serving`**.
- **not restored**, each with its reason:
  - the account came without its password — v1 hashes with bcrypt, this server with argon2id,
    so set a password on it;
  - trigger `AddBook` is a v1 workflow, and trigger `TrimPages` runs `modify_row`, which is not
    an action this server has;
  - the menu's 10 *Admin Page* and 5 *User Page* entries, which point at Saltcorn 1's own
    screens — Saltcorn UI's menu has its own Login and Logout;
  - the tag and the two plugins.

Nothing in the second list stopped the application. A view whose pattern this server does not
have (v1's `Room`), or whose table was not restored, would be a line there too, naming the view.

**One application per backup.** Saltcorn 1 had one set of views per site; here the unit is the
application, so the backup becomes an application named after its `site_name`, with every
imported table, file store and trigger in its subsets. The views are v1's configurations,
**untouched** — the server does not translate them, it runs them.

**Restoring the same backup again** replaces that application's views and pages with the
backup's and keeps everything else you changed: its subdomain, its settings, its
Content-Security-Policy. Seven views stay seven.

### Step A3 — The application

**Applications** now lists **BooksDB**, framework **Saltcorn UI**, and — unlike a React app — no
Build button and no *Not built yet* badge. Press **Views**: the seven views, each with its pattern,
table, minimum role and a link that opens it on the subdomain. **Pages** has one, *BooksOverview*.

### Step A4 — Open it

`http://booksdb.localhost:3032/`. BooksDB's page is not anybody's home page, so `/` is a short
document naming the pages and views there are. Follow **BooksOverview**.

Every BooksDB view and its page is `min_role` 1 — admins only, as they were in Saltcorn 1 — so
you are sent to the application's **Sign in** form, with the way back in its `dest`. Sign in as an
admin of this server; the imported account has no password until you set one.

You land on `/page/BooksOverview`: *Filter books*, with an author dropdown and two page-count
range inputs, and *List Books* under it — a row per book, the author's last name joined in from
Authors, with *Show*, *Edit*, a *TrimPages* action and *Delete* on each row.

(The *Publisher* column is empty: both books in the fixture have no publisher. Add one through
*Edit* and it appears.)

### Step A5 — Use it

- **Filter.** Pick *Melville*. The list re-runs and narrows without a page load — the Filter sets
  the page's state and `saltcorn.js` reloads the list's fragment, exactly as in v1.
- **Show** on a row renders *Show Books*.
- **Edit** renders *Edit Books*, with the author and publisher as selects. Change the page count,
  **Save**, and you are back where you came from with the new value.
- **Add row** (at `/view/Edit Books` with no id) creates a book; **Delete** on its row deletes it.
- **TrimPages** answers with an error naming `TrimPages`: the view still names the trigger, and
  the trigger was not restored. Write it again as this server writes triggers — **Triggers → New**,
  event *none*, action `run_js_code` — and the column runs it. The application already declares
  it.

### Step A6 — What the browser was sent

View the page's source. The markup is v1's, the scripts and stylesheets come from
`/static_assets/<version>/` — Bootstrap 5.3, jQuery, `saltcorn.js` — and the response carries:

```
content-security-policy: default-src 'self'; script-src 'self' 'unsafe-inline'
```

That `'unsafe-inline'` is the one relaxation, and it is deliberate. v1's HTML puts JavaScript in
`onclick` and `onchange` attributes all through its views, and an attribute handler cannot be
signed with a nonce. Nothing else moves: no `eval`, no other origin, `default-src 'self'`. The
policy is the framework's default and shows on the application's edit screen; one you state there
wins. (A side effect of `default-src 'self'`: v1's inline `style="…"` attributes do not apply, so
a few spacings differ from Saltcorn 1.)

---

## Part B — Start from nothing

### Step B1 — A table

**Tables**, type `books` in **New table name**, **Create table**. A new table has **no primary
key** — the table editor says so — and an Edit view cannot save a row without one, so the first
field is the key. Open the table and add:

- `id` — `integer`, tick **Primary key**. An integer key is generated by the database
  (`generated by default as identity`), so no form ever asks for it.
- `title` — `text`, not nullable
- `pages` — `integer`, nullable

Add a row or two from the table editor so there is something to list.

### Step B2 — The application

**Applications → New application**:

| Field | Value |
|---|---|
| Name | `Library` |
| Subdomain | `library` |
| Framework | **Saltcorn UI** |
| Site name | `Library` |
| Tables | `books` |

Leave *Menu*, *Home page per role* and the rest as they are for now, and the
Content-Security-Policy box empty. **Create application**. There is no banner and nothing to
build: creating it mounted it, and `http://library.localhost:3032/` already answers, with a
document saying there is nothing here yet. (If it answers with the admin sign-in screen instead,
the application did not mount — the server log says why; see *Things that trip people up*.)

A view may only name a table in the application's **Tables**. That is the difference from Saltcorn
1 worth knowing: views belong to an application, not to the server, so two applications can each
have a `List books` over the same table, configured differently.

### Step B3 — A List

On the row, **Views → New view**:

| Field | Value |
|---|---|
| Name | `List books` |
| Pattern | **List** |
| Table | `books` |
| Minimum role | *Admin* |

Create it, and the configuration editor opens on the pattern's own steps — v1's `List` has four:
*Columns*, *Create new row*, *Default state*, *Options*. A new List starts with a column per field
(that is v1's own initial configuration), so there is nothing you have to change. **Save**.

*Columns* is the layout step, and it is shown as read-only JSON with a sentence saying the builder
will edit it. The other three are ordinary forms, built by v1's code over your table.

Open `http://library.localhost:3032/view/List%20books`. Sign in as your admin (the view is
admin-only) and you are sent back to the list.

**A save replays the steps.** If a value is one the step's form would not accept — a number
where it wants a choice, a view it does not offer — the save is refused naming the step and the
setting, and nothing is saved.

### Step B4 — A Show and an Edit

Two more, over `books`, both *Admin*:

- `Show book`, pattern **Show**. Its one step is its layout, so it is complete as created: a
  label and a value per field.
- `Edit book`, pattern **Edit**. Its steps are *Layout* (read-only; a form field per field),
  *Fixed and blocked fields* (skipped when every field is on the form), and *Edit options*.

Configure **Edit book → Edit options**: *Destination type* **View**, *Destination view*
**List books**. Save. That is where a saved form goes.

`/view/Show%20book?id=1` shows the first book; `/view/Edit%20book?id=1` edits it and, on Save,
comes back to the list. `/view/Edit%20book` with no id is an empty form that creates a row.

### Step B5 — A link that works

Back to **List books → Configure → Create new row**: *Use view to create* **Edit book**, *Display
create view as* **Link**, *Label for create* `Add a book`. Save.

The list now has an **Add a book** link. Follow it, fill in the form, Save, and you are back on the
list with the new row — List → Edit → List, every hop a v1 link, nothing written by hand.

A per-row *Show* or *Edit* link is a column in the List's **layout**, which is the step the builder
will edit. Until it arrives, a new List gets those links only by editing its configuration through
the `saveView` endpoint; an imported List has them already (Part A).

### Step B6 — A page

Pages have no editor yet, so this one is saved from the browser console, through the same admin
API the screens use. Open the application's Views tab and note its id in the address bar
(`#/applications/<id>/views`). Then, in the console on `http://localhost:3032`:

```js
const csrf = document.cookie.match(/(?:^|;\s*)sc_csrf=([^;]*)/)?.[1];
const app = "<the application's id>";
await fetch(`/api/applications/${app}/pages/Home`, {
  method: "PUT",
  headers: { "content-type": "application/json", "x-csrf-token": csrf },
  body: JSON.stringify({
    name: "Home",
    title: "Library",
    min_role: 1,
    // v1's layout shape: this one is a single embedded view. `shared` means the
    // page's query string is the view's state, so ?id= and filters reach it.
    layout: { type: "view", view: "List books", state: "shared", name: "books" },
    attributes: {},
  }),
}).then((r) => r.json());
```

The Pages tab lists it. `http://library.localhost:3032/page/Home` renders the list inside the
page. A layout is v1's JSON and is stored exactly as given: `above` for a column of segments,
`besides` for a row, `{ type: "blank", contents: "…" }` for text — anything v1's `renderLayout`
understands.

### Step B7 — A menu and a home page

**Applications → Library → Edit**. Two of Saltcorn UI's settings are JSON, in v1's own shapes:

**Menu** — v1's `menu_items`:

```json
[
  { "type": "Page", "label": "Home", "pagename": "Home", "min_role": 1 },
  { "type": "View", "label": "Add a book", "viewname": "Edit book", "min_role": 1 },
  { "type": "Link", "label": "Saltcorn", "url": "https://saltcorn.com", "min_role": 100 }
]
```

An entry is shown to a viewer whose role is at or above its `min_role` (1 is admin, 100 is
public); a `Header` entry with `subitems` makes a dropdown.

**Home page per role** — a role id to a page name:

```json
{ "1": "Home" }
```

**Save changes.** It is live immediately. `http://library.localhost:3032/` now opens *Home* for
an admin, with the menu across the top and *Logout* beside it. A role with no home page still gets
the document naming what there is.

---

## Who sees what

Two separate checks, and it matters that they are separate:

- A view's **minimum role** decides whether it runs at all. Below it, somebody who is not signed in
  is sent to `/auth/login` and back; somebody who is gets a page saying the view is not for their
  role.
- Every row the view reads or writes is read or written **as the viewer**, under the table's own
  read and write roles and ownership formula ([tutorial-ownership.md](tutorial-ownership.md)). A
  public List over a table whose rows are private shows each visitor only their own — the view
  being allowed does not make the rows allowed.

Signing in and out are the application's own `/auth/login` and `/auth/logout`, with the same
accounts as the admin UI. **Offer sign-up** turns on `/auth/signup`, and new accounts get **Role of a
new account** (80 unless you change it; never an admin role).

## Actions in a view

A view's action column or button runs one of three things, in this order: one of v1's built-in view
actions (*Delete*, *Save*, *Cancel*, *GoBack*, …); a **trigger the application declares** (the
application's Triggers card); anything else is refused — when the view is saved if its
configuration names it, when it runs otherwise.

## View patterns from a module

A Saltcorn 1 plugin whose content is view templates supplies patterns here too. Install
`@saltcorn/kanban` from **Settings → Modules** ([tutorial-modules.md](tutorial-modules.md)), and
**Kanban** is offered in **New view** beside the six built-in patterns, configured through the same
wizard, with its scripts added to the pages that render it. A plugin that writes raw SQL through
v1's `db` — `@saltcorn/mind-map` — installs and registers, and its views fail naming `db.query`:
this server does not hand a plugin SQL.

## Things that trip people up

- **The subdomain serves the admin sign-in screen instead of the application.** The application is
  not mounted. The server log (and the application's save) says why — most often a binary built
  without the Saltcorn UI bundle.
- **A view is refused on save naming a table.** The table is not in the application's **Tables**.
- **Saving an Edit view's form fails with a JavaScript error** — "Cannot read properties of
  undefined (reading 'type')", or "Field initialised with no name and no label". The table has no
  primary key. Add one (Part B, step B1); v1's Edit cannot insert or update without it.
- **An action column answers "not found" naming a trigger.** The application declares a trigger the
  server does not have — typically a v1 trigger the restore refused. Write it again under the same
  name.
- **A renamed view breaks links to it.** Rename shows what refers to the old name (views that embed
  or link to it, pages that show it) before you confirm, and changes none of them.
- **`/view/List books` is a 404 in a script.** Names may contain spaces, so encode them:
  `/view/List%20books`.

## What next

- [tutorial-ownership.md](tutorial-ownership.md) — rows a view shows per viewer.
- [tutorial-triggers.md](tutorial-triggers.md) — triggers a view's action column runs.
- [tutorial-modules.md](tutorial-modules.md) — patterns from an installed plugin.
- The builder, when it lands, edits the layout step this tutorial leaves alone.
