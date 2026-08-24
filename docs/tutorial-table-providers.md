# Tutorial: Table providers — a table that is not in your database

Every table you have made so far is a table in a database: Saltcorn issued a `CREATE TABLE`, and
the rows are on disk where it put them. A **table provider** makes the other kind. You pick it
instead of a database when you create the table, fill in the form it asks for, and you get
something that behaves like a table — it has columns, it lists, it filters, it goes in a view —
whose rows come from a **module** instead.

An RSS feed is the smallest honest example, and the one this tutorial uses. Others exist:
`@saltcorn/proxmox` presents a virtualisation cluster's VMs, `@saltcorn/postgres-tables`
presents a table in somebody else's PostgreSQL.

This continues from [tutorial-modules.md](tutorial-modules.md): you have a server, an admin
login, and you know how a module is installed and granted what it may reach.

## Step 1 — Install a module that supplies one

Go to **Settings → Modules**, type `@saltcorn/rss` and press **Install**. The card comes up
saying what you got:

```
@saltcorn/rss                        1 table provider
v0.1.0 · npm

Table providers
  RSS feed

Create a table from one under Data → Tables → New table. Saltcorn reads its rows;
it does not write them.

Reaches nothing: no host, no file, no environment variable.
```

Note the badge. `@saltcorn/rss` has **no actions at all** — the whole of it is one table
provider — which is a perfectly ordinary shape for a v1 plugin and the reason the badge counts
what a module actually supplies rather than counting actions.

## Step 2 — Let it reach the feed

The last line of the card is the one that matters next. A module reaches nothing you have not
granted it, and a feed is on somebody's web server. Press **Permissions** and put the feed's
host in **Hosts it may connect to** — for `https://news.ycombinator.com/rss` that is:

```
news.ycombinator.com:443
```

The port is part of the permission (`:443` for HTTPS, `:80` for plain HTTP), and a feed that
redirects to a different host needs that host too. Save; the card now says what it may reach.

Get this wrong and you will find out in a sentence rather than an empty table — see step 6.

## Step 3 — Create the table

Go to **Data → Tables → New table**. The dialog has a **Type** dropdown that now has a third
entry:

| Type | What it makes |
|---|---|
| New database table | A table in a database. Saltcorn creates and owns the columns. |
| Create from CSV | The same, with the columns and rows read out of a file. |
| **From a table provider** | A table whose rows come from a module. |

Choose it, and two things appear: a chooser listing every provider every installed module
supplies — `RSS feed (@saltcorn/rss)` — and, under it, **the form that provider declared**:

| Field | Value |
|---|---|
| Name | `headlines` |
| Type | From a table provider |
| Table provider | RSS feed (@saltcorn/rss) |
| Feed URL | `https://news.ycombinator.com/rss` |

That Feed URL box is not written anywhere in Saltcorn. It is the provider's own v1
`configuration_workflow`, read out of the package and rendered from the same declaration a file
store's backend, an LLM provider and a module's own settings use.

Notice what the dialog does **not** ask: which database. There is no answer — the rows are not
in one.

Press **Create**.

## Step 4 — Look at what you got

The table's page opens with a card you have not seen before, above the fields:

```
Table provider   [RSS feed]  @saltcorn/rss

The rows of this table come from @saltcorn/rss, not from a database.
Saltcorn reads them; it does not create, change or delete them.

Feed URL  [https://news.ycombinator.com/rss]
                                              [ Save provider settings ]
```

It is above the fields because it **decides** them. Under it:

```
Fields
  title    Title    String
  link     Link     String

These columns are the table provider's. Change what it presents in its settings
above, or in the module itself.
```

Two columns, no primary key, and no **Add field** button. Saltcorn did not choose those columns
and cannot change them: they are what the module answers when asked what it presents. Ask again
with different settings and you may get a different answer — `@saltcorn/postgres-tables` asks
you which columns of the remote table you want, and the field list follows.

**Rows** shows the number of items in the feed, and **View** lists them. From here on nothing is
special: a view over `headlines` is a view like any other, a formula over it is a formula, and
the REST API serves it at `/api/tables/headlines/rows`.

## Step 5 — Filter it, sort it, page it

Filtering, sorting and paging all work, and it is worth knowing *where* they happen, because it
decides what is fast:

- Saltcorn hands the provider your query in the form v1's providers understand — a `where`
  object and an `options` object. A provider that can use it does: `@saltcorn/postgres-tables`
  turns it into SQL and the remote database does the work.
- Then Saltcorn applies the whole query to whatever came back, because a provider is **allowed
  to ignore the hint**. `@saltcorn/rss` does exactly that: it answers the entire feed whatever
  you asked, and Saltcorn filters it here.

So `headlines` sorted by title is correct but reads the whole feed; a remote PostgreSQL table
sorted by an indexed column is correct *and* reads one page. Same query, same answer, different
amount of work — and which one you get is a property of the module, not of Saltcorn.

What does **not** work is a `JOIN`: a key field on another table pointing at `headlines` has
nothing to join to, because these rows are not in the database the join would run in. You get a
sentence saying so rather than a wrong answer.

## Step 6 — When it goes wrong

Three failures, and each one leaves the table where you can fix it.

**The module cannot reach the feed.** You forgot step 2, or the feed moved. The provider card
carries the reason:

```
This provider is not answering
  the table provider `RSS feed` of `@saltcorn/rss` could not say what columns
  `headlines` has: the module `@saltcorn/rss` was denied net access to
  "news.ycombinator.com:443": add "news.ycombinator.com:443" to its network
  allow-list in Settings → Modules → @saltcorn/rss → Permissions
```

**You removed the module.** The table stays in the list, with no columns and a sentence saying
no installed module supplies that provider. That is deliberate: the fix is to reinstall the
module, and a table that had vanished from the admin UI would be a table you could not fix.

**You tried to write to it.** This version of Saltcorn **reads** a provided table and does not
write one. A row editor's save, an `insert_row` action or a `POST` to its REST endpoint is
refused by name:

> `headlines` is served by the table provider `RSS feed` of `@saltcorn/rss`, and this version of
> Saltcorn reads a provided table but does not write one.

v1 has writable providers — `@saltcorn/postgres-tables` will insert, update and delete into the
remote table when you untick its **Read-only** box — and they are a later milestone here.

## Step 7 — Getting rid of it

**Delete table** on the table's page. For a provided table that is not a `DROP TABLE` — there is
nothing in any database to drop — it forgets the definition, and the confirmation says so:

> Delete the table "headlines"? Saltcorn forgets it. The data it was reading is not touched.

The feed is still there. Make the table again and you have it back.

## What this is, in one paragraph

A `_sc_tables` row usually *adds* to a table the database already has — a label, roles, an
ownership formula — and the table exists whether or not the row does. A provided table's row is
the opposite: it **is** the table. It names a module, a provider inside that module, and the
settings you typed; the columns are what that provider answers when asked, and the rows are what
it answers when read. Everything above `Catalog::provider` — views, the REST API, GraphQL,
formulas, a code body's `db.headlines` — is unaware of any of it.

## What this is not, yet

- **Read-only.** v1's `insertRow`, `updateRow` and `deleteRows` are not called.
- **No joins.** A provided table cannot be joined to, and a key field pointing at one will not
  resolve through a join.
- **No materialisation.** v1 can snapshot an external table into a real one on a schedule; here
  every read reaches the module.
- **Not cached by Saltcorn.** `@saltcorn/rss` keeps a five-minute cache of its own, which is why
  reloading the page does not re-fetch the feed — but that is the module's cache, not Saltcorn's,
  and a provider without one is asked every time.
