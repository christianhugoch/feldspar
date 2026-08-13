# Tutorial: Triggers — make the server do things by itself

Write an audit trail that no client can forget to write. Expose one server-side job to your app
as a typed API call. Then have the server run something every night while nobody is watching.
**All of it happens in a browser**, and none of it is code you deploy.

This continues from [tutorial-ownership.md](tutorial-ownership.md): you have a server started
with `--base-domain localhost`, a `tasks` table (`id`, `title`, `done`, `owner`) served by a
React `todo` app on `http://todo.localhost:3032`, a **Member (40)** role, and a user
`member@example.com` who holds it. Any table and any app will do — the names below just assume
those.

## The rule in one line

A **trigger** binds one **event** to one configured **action**:

> when *this happens*, and *this condition holds*, run *that action* with *these settings*.

That is the whole model. A trigger is not a list of steps — a sequence of steps is a workflow,
and keeping the two apart is what makes a trigger something you can reason about at a glance.
Two triggers on the same event is how you get two things done.

The events are: a row **inserted**, **updated** or **deleted** in a table; **none** (it runs
only when something asks); a user **signs in**; the server **starts up**; an **error** is
reported; and the four periodic ones — **every five minutes**, **hourly**, **daily**, **weekly**.

## Step 1 — A table to write the trail into

Triggers write through the ordinary row path, so their target is an ordinary table. Go to
**Tables**, create `task_audit`, open it, and add four fields:

| Field | SQL type | Nullable |
|---|---|---|
| `task` | `bigint` | ✔ |
| `what` | `text` | ✔ |
| `who` | `text` | ✔ |
| `at` | `bigint` | ✔ |

`at` is a `bigint` rather than a `timestamptz` on purpose, and the reason is worth knowing before
you write your first formula: the formula language has **no `new`**, so `new Date().toISOString()`
is not something a field value can compute. `Date.now()` — epoch milliseconds — is, and it sorts
and compares exactly as well.

Leave its access roles alone: admin-only is right for an audit table, and a trigger's write
does not go through the caller's role — a trigger is *the admin's* configuration, so it writes
with admin authority. That is the point of an audit trail: the row a user may not insert is
exactly the one you want written when they act.

## Step 2 — The table trigger, with an "only if"

Go to **Triggers → New trigger**:

| Field | Value |
|---|---|
| Name | `audit_completed` |
| Event | `A row is updated` |
| Table | `tasks` |
| **Only if** | `done && !old.done` |
| Action | `insert_row` |
| Table (action setting) | `task_audit` |
| Field values | see below |

**Field values** is a JSON object of *field → formula*. Paste:

```json
{
  "task": "row.id",
  "what": "'completed: ' + row.title",
  "who": "user ? user.email : 'nobody'",
  "at": "Date.now()"
}
```

Press **Save**. Now open the app at `http://todo.localhost:3032`, sign in as
`member@example.com`, and tick a task off. Back in the admin UI, open **Tables → task_audit**; the
**Rows** card has one row: the task's id, `completed: Draft the report`, and the member's email.

Three things just happened that are worth naming.

**The `only_if` decides per row.** It is a JavaScript expression over the affected row, and it
sees three things: the row's fields as bare identifiers (`done`), the same row as `row`, and —
on an update — the row **as it was** as `old`. So `done && !old.done` means *became* done: it is
true for the tick, and false for every later save of an already-done task. Untick and re-tick a
task and you get exactly one new audit row, not two.

**The caller travels with the event.** `user` is the signed-in user's record (or `null` for an
anonymous write), which is why the trail knows *who*. It is the same `user` an ownership formula
sees.

**The write is a write.** `insert_row` goes through the same path the API uses, so the target
table's types, rules and *its own triggers* all apply. That is a feature — writing an audit row
that itself fires something is how you denormalise — and it is bounded: a chain more than five
triggers deep is refused, with the whole chain in the error, rather than looping.

### If nothing was written

Open **Triggers**. The status column is the diagnosis:

- **Not usable** with a reason — the trigger is stored but not live (a table it names was
  dropped, a formula stopped resolving). Fix it in the form; it never fires while it says this.
- **Off** — someone unticked **Enabled**.
- **Enabled** — it is live. If it still did not fire, the `only_if` said no: on an insert `old`
  is null, and the expression is evaluated *after* the write, against the row as it now is.

## Step 3 — A job the app can call

Some work has no natural moment: "sweep the finished tasks", "recalculate the totals", "send my
digest now". That is the **`none`** event — no intrinsic occurrence, it runs when something asks.

**Triggers → New trigger**:

| Field | Value |
|---|---|
| Name | `archive_done` |
| **Minimum role** | `40` |
| Event | `Only when something asks (no event)` |
| Action | `delete_rows` |
| Table (action setting) | `tasks` |
| **Where** | `done && owner === user.email` |

Save, then press **Run** on its row in the list. Run posts an empty payload and shows you the
action's result (`{"deleted": 0, "ids": []}` when you run it as an admin with no tasks of your
own) — or the action's *error*, which is what a test button is for.

Two settings there decide who may call it:

- **Minimum role** is the floor for reaching this trigger through an application's API. Leave it
  blank and the trigger is **admin-only** — the safe reading, because a trigger nobody has
  thought about the access of should not turn out to be public. `40` lets Members call it.
- The **Where** formula selects the rows to delete, in the target table's scope, and it can name
  `user` — so this deletes *the caller's* finished tasks and nobody else's.

### Expose it on the app

A trigger is not reachable from outside until an application says so. Go to **Applications →
Todo → Edit**. The **Triggers** card lists every trigger on the server with a checkbox; tick
`archive_done` and **Save changes**, then press **Build** on the app's row.

The app now serves `POST /api/actions/archive_done`, and its generated TypeScript client has a
typed `runArchiveDone(body)` beside `listTasks()` and friends — so from the app's own code it is
one call, with the session cookie and the CSRF token handled for you:

```ts
const { deleted } = await api.runArchiveDone({});
```

To try it without editing the app, use the browser console on
`http://todo.localhost:3032` while signed in as the member. A mutating request has to echo the
CSRF cookie in a header — that is what the generated client is doing for you above:

```js
const csrf = document.cookie.match(/(?:^|;\s*)sc_csrf=([^;]*)/)?.[1];
await fetch("/api/actions/archive_done", {
  method: "POST",
  headers: { "content-type": "application/json", "x-csrf-token": csrf },
  body: "{}",
}).then((r) => r.json());
```

…and their finished tasks are gone. Sign out and try the same call: **401**. Try a trigger you
did *not* tick: **404** — not 403, because a trigger the app does not expose has no endpoint at
all. Exposing is the application's decision, not a permission to be escalated.

The posted body arrives as the event's **payload**, readable in any formula as `payload.x`. Give
the action a `where` of `done && owner === user.email && title === payload.title` and the same
endpoint archives just the one task named in the body.

## Step 4 — Something every night

**Triggers → New trigger**:

| Field | Value |
|---|---|
| Name | `nightly_sweep` |
| Event | `Once a day` |
| Hour (UTC) | `3` |
| Minute past the hour | `30` |
| Action | `delete_rows` |
| Table (action setting) | `task_audit` |
| **Where** | `at < Date.now() - 30*24*3600*1000` |

Save. The list now shows `daily · at 03:30 UTC` under the event, and a **Last run** column that
says `—` until it has run.

**Schedules are UTC.** Not the server's local time and not yours: a server-side schedule has no
user to have a timezone, and a local one would mean an hour that happens twice a year and an
hour that does not happen at all. `03:30` is `03:30Z`, wherever the machine is.

The other three periodic kinds work the same way, each asking only for the parts it needs:
**every five minutes** (nothing to configure), **hourly** (a minute past the hour), **weekly** (a
day, an hour and a minute).

You do not have to wait until 03:30 to see whether the action works — but **Run** is only
offered for `none` triggers, because every other kind needs its own occurrence to say anything
about. To test a periodic action, build it as a `none` trigger first, press Run until it does
what you meant, then change its event to `daily`. The action and its settings are unchanged by
that switch.

### What the scheduler guarantees

- **A missed run is caught up once.** If the server is down over 03:30, it fires once shortly
  after the server is back — not once per missed night, and not never. The last run is stored on
  the trigger's row, which is what survives the restart.
- **A fresh trigger is not instantly overdue.** Its clock starts when it is created, so a daily
  trigger saved at noon first runs at the next 03:30.
- **A slow action is skipped, not queued.** If an `often` action takes twelve minutes, the
  occurrences that come round meanwhile are dropped: five queued copies of a report nobody read
  is worse than one late one.
- **Off is not down.** A disabled trigger's clock keeps advancing, so switching a nightly job off
  for a week and back on runs it *tonight* rather than immediately. Downtime is not a decision;
  disabling is.

## The actions you have

Every action declares its own settings, and the form is rendered from that declaration — so an
action added by a plugin gets a working form with no change to the admin UI.

| Action | What it does |
|---|---|
| `insert_row` | Insert one row into a table, each field a formula over the event |
| `update_rows` | Update the rows a `where` formula selects, each assignment a formula |
| `delete_rows` | Delete the rows a `where` formula selects (the `where` is required) |
| `fetch` | Send an HTTP request built from the event; the parsed response is the result |
| `run_js_code` | Run a JavaScript body against the event and return what it returns |

`fetch` is the webhook: point it at a URL, give it a JSON body of formulas, and its response
comes back as the trigger's result — so a `none` trigger exposed on your app can be a typed
front end to somebody else's API.

`run_js_code` is the escape hatch for a computation no combination of the others expresses. It
sees `row`, `old`, `user` and `payload`, and **nothing else**: no catalog, no network, no disk.
That is deliberate. Reaching the database from guest code is a separate milestone, not a corner
of this one.

## Things that trip people up

- **A trigger fires *after* the write, and cannot veto it.** The row is already committed when
  the action runs; a failing trigger is reported, and the request that caused it still succeeds.
  Validation that must *prevent* a write belongs on the field or the table, not here.
- **`old` is null on an insert.** It is in scope for every table event — a member of a null
  object is null rather than an error — so `!old.done` on an *insert* trigger is true for every
  row, which is rarely what you meant. `done && !old.done` is a condition about a change, and
  changes only happen on updates.
- **An `only_if` needs a row.** It is offered only for table events; a `login` or `daily` trigger
  has no row to test, so a condition on one is refused rather than accepted and never true.
- **The operation flags are not available.** `_insert`, `_update` and `_delete` (which ownership
  formulas use) are refused in an `only_if`: the trigger's own event *is* the operation, so
  `_insert` inside an insert trigger is a tautology and inside a delete trigger a lie.
- **Renaming a trigger breaks what refers to it, on purpose.** The name is the key an
  application's exposed subset and an API path use. Rename one an app exposes and the app will
  not mount until you fix the app too — visibly, rather than silently serving something else.
- **Deleting a trigger an app exposes is allowed.** Blocking it would leave you unable to remove
  a trigger you no longer want. The app keeps serving until its next build, and then names the
  missing trigger.
- **Formula values are formulas, not literals.** `"what": "completed"` sets the field to the
  *value of the identifier* `completed` — which is an unknown-identifier error on save. A literal
  string is quoted twice: `"what": "'completed'"`.
- **A formula is one pure expression.** No `new`, no assignment, no statements — so "now" is
  `Date.now()` (a number), not `new Date()`. `Math`, `JSON`, `String`, `Number` and `Date` are
  reachable as globals; a field of the same name shadows them.
- **Errors are events, but rejections are not.** The `error` event fires when a request fails,
  not when one is refused: a 404 for a bad URL or a 401 from the auth gate is a rejection, and an
  alerting trigger that fired on every probe of a wrong path would be useless for what it is for.

## What next

You now have the server acting on its own: on writes, on request, and on a clock. The natural
pairing is [tutorial-ownership.md](tutorial-ownership.md)'s formula language, which is the same
language these triggers are configured in — `only_if`, a `where`, and every field value are all
the one expression syntax, evaluated the same way, over the event instead of over a row.

Then [tutorial-agents.md](tutorial-agents.md), which adds one more action to the table above:
`run_agent`, whose configuration is an agent's name and a prompt formula over the same event. An
agent is a configured LLM loop that can read your tables, run the triggers you built here, and
edit your app's source — and hanging one off a trigger is how it runs when nobody is watching.
