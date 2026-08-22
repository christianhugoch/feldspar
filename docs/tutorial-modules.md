# Tutorial: Modules — actions somebody else already wrote

Saltcorn's built-in actions are deliberately few. When you want a trigger that publishes to an
MQTT broker, snapshots a Proxmox VM or talks to whatever else your building runs on, the answer
is a **module**: a Saltcorn plugin, installed from the admin UI, live on a running server. The
modules are the ones written for Saltcorn v1 — there are dozens of them, they are ordinary npm
packages, and this version of Saltcorn loads the **actions** they supply.

This continues from [tutorial-triggers.md](tutorial-triggers.md): you have a server, an admin
login, and you know how a trigger binds an event to a configured action. Nothing here needs a
table you do not already have.

**Before you start**, the server needs Node.js and npm on its `PATH` — a module is JavaScript
and it runs in a Node process beside the server. The Modules tab says so if they are missing.

## What a module is

An npm package whose main file exports v1's plugin object:

```js
module.exports = {
  sc_plugin_api_version: 1,
  configuration_workflow,               // the module's own settings
  actions: (cfg) => ({                  // what this version loads
    mqtt_publish: { configFields: [...], run: async ({ row, configuration }) => { … } },
  }),
  eventTypes: () => ({ … }),            // reported, not loaded (yet)
};
```

Two of those keys are read: `actions`, whose entries become actions your triggers can run, and
`configuration_workflow`, whose form becomes the module's own settings. Everything else — view
templates, types, field views, table providers, event types — is **counted and named** in the
tab so you can see what you are not getting, and is a later milestone.

## Step 1 — Install one

Go to **Settings → Modules**. Two kinds of source:

| Type | What you type |
|---|---|
| JavaScript — npm package | `@saltcorn/mqtt`, or `@saltcorn/mqtt@0.2.0` to pin a version |
| JavaScript — local directory | `/srv/checkouts/mqtt` — an absolute path on the **server** |

Type `@saltcorn/mqtt` and press **Install**. The server runs `npm install` into its modules
directory (named on the screen; `--modules-dir` moves it), loads the package, and the module
appears with a green **1 action** badge, its version, and the action it supplies:

```
@saltcorn/mqtt                       1 action
v0.2.0 · npm

Actions
  mqtt_publish

Also supplies eventTypes, which this version of Saltcorn does not load yet.
```

That last line is the honest part: `@saltcorn/mqtt` also raises an `MqttReceive` event in v1,
and here it does not. You get the action.

> **Installing a module runs somebody else's code as your server.** `npm install` runs install
> scripts, and the module itself runs in a Node process with the server's privileges and its
> network. There is no sandbox in this version. Install modules you trust; the endpoints are
> admin-only for the same reason.

## Step 2 — Configure it

Most modules need settings of their own — which broker, which cluster, which key. Press
**Settings** on the module's card and you get a form: **Broker URL**, **Port**, **Protocol**,
**Username**, **Password** and the rest. That form is not written anywhere in Saltcorn — it is
the module's own v1 `configuration_workflow`, read out of the package and rendered from the same
declaration a file store's backend or an LLM provider uses.

Password fields are **secrets**: they come back to the browser as `••••••` and a save that
leaves them untouched keeps what is stored.

Saving reloads the module, so the next run of any of its actions uses the new settings.

## Step 3 — Use its action in a trigger

Go to **Triggers → New trigger**. `mqtt_publish` is in the action picker, and choosing it gives
you the **Channel** setting the module declared:

| Field | Value |
|---|---|
| Name | `publish_task` |
| Event | `A row is inserted` |
| Table | `tasks` |
| Action | `mqtt_publish` |
| Channel | `tasks/new` |

Save it and insert a row. The module's `run` is called with v1's argument object — `{ row,
old_row, table, configuration, user, payload, mode, req }` — so plugin code written years ago
sees exactly what it expects.

Nothing was restarted. Installing a module rebuilds the action set the trigger dispatcher runs
from and revalidates every stored trigger against it, so a trigger you saved *before* installing
the module (and which was refused as "unknown action") starts working the moment it is there.

## Developing a module against a running server

Point the **local directory** source at your checkout. The package is copied into the modules
directory, so an edit reaches the server when you press **Install** again — that is the loop.
(It is a copy rather than a link because npm does not install a linked package's dependencies,
which is a module that does not load at all.)

## When something goes wrong

Modules fail in three ways, and all three are sentences on the module's card rather than a
server that will not start:

- **"its package is not installed"** — the row is there and the package is not, which is what a
  restored backup looks like before its modules are reinstalled. Install it again.
- **"needs the package `node-fetch`, which is not installed"** — the module requires something
  it never declared as a dependency. v1 got away with that because v1's own server had the
  package; here nothing does. Install the missing package the same way you installed the module
  (as an npm package — it goes into the same directory), or fix the module's `dependencies`.
- **"its action `insert_row` is not available: two actions are registered under the name"** —
  the module claims a name a built-in already has. The built-in keeps it; the module's other
  actions still work. Nothing silently swaps underneath a trigger.

And one that is not a failure: a module using an API this version does not implement fails
**when it is used**, naming it —

```
the Saltcorn v1 API data/models/table.findOne is not available to modules in this
version of Saltcorn.
```

Only `Workflow`, `Form` and `utils.interpolate` are real so far. A module whose actions do not
touch v1's models — which is most modules that talk to something outside Saltcorn — never meets
that message.

## Removing one

**Remove** on the card unloads the module, uninstalls the package and deletes the row, and the
actions it supplied leave the picker immediately. A trigger still naming one says so rather than
quietly doing nothing.

## What this is not, yet

- **Only actions.** Views, types, field views, table providers and event types are named in the
  tab and not loaded.
- **Only JavaScript.** The install form has a Type dropdown because Python and the rest come
  later; today it has one language and two sources.
- **No sandbox, no store.** You type a package name; nothing browses or rates modules for you.
