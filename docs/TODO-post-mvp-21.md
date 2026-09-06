# Saltcorn v2 — Bundled modules

Ordered, checkable task list for the twenty-first milestone after the MVP. Earlier lists are
archived in [docs/TODO-mvp.md](./TODO-mvp.md) (the MVP),
[docs/TODO-post-mvp-1.md](./TODO-post-mvp-1.md) (file stores + the React framework),
[docs/TODO-post-mvp-2.md](./TODO-post-mvp-2.md) (the `_sc_tables`/`_sc_fields` overlays,
rich types and File fields), [docs/TODO-post-mvp-3.md](./TODO-post-mvp-3.md) (ownership
formulae, calculated fields and row-level security),
[docs/TODO-post-mvp-4.md](./TODO-post-mvp-4.md) (actions and triggers),
[docs/TODO-post-mvp-5.md](./TODO-post-mvp-5.md) (the file-store IDE),
[docs/TODO-post-mvp-6.md](./TODO-post-mvp-6.md) (agents),
[docs/TODO-post-mvp-7.md](./TODO-post-mvp-7.md) (the GraphQL provider),
[docs/TODO-post-mvp-8.md](./TODO-post-mvp-8.md) (REST queries, custom SQL and the
generated client), [docs/TODO-post-mvp-9.md](./TODO-post-mvp-9.md) (table constraints
and indexes), [docs/TODO-post-mvp-10.md](./TODO-post-mvp-10.md) (email),
[docs/TODO-post-mvp-11.md](./TODO-post-mvp-11.md) (tables in code),
[docs/TODO-post-mvp-12.md](./TODO-post-mvp-12.md) (concurrent code bodies),
[docs/TODO-post-mvp-13.md](./TODO-post-mvp-13.md) (modules),
[docs/TODO-post-mvp-14.md](./TODO-post-mvp-14.md) (SQLite),
[docs/TODO-post-mvp-15.md](./TODO-post-mvp-15.md) (modules in-process),
[docs/TODO-post-mvp-16.md](./TODO-post-mvp-16.md) (table providers),
[docs/TODO-post-mvp-17.md](./TODO-post-mvp-17.md) (writable table providers),
[docs/TODO-post-mvp-18.md](./TODO-post-mvp-18.md) (workflows),
[docs/TODO-post-mvp-19.md](./TODO-post-mvp-19.md) (the Python code adapter) and
[docs/TODO-post-mvp-20.md](./TODO-post-mvp-20.md) (the administration MCP server).
Scope and rationale remain in [docs/GOALS.md](./GOALS.md) and
[docs/TECHNICAL_DESIGN.md](./TECHNICAL_DESIGN.md) (**§15.1**, which this milestone
extends, and a new **§15.1a**).

A module comes from a registry, and a registry is a name an admin has to know. That is the
right shape for the long tail — the dozens of v1 plugins, the one somebody wrote for their
own building — and the wrong one for the short: an RSS table, a Markdown renderer, things a
third of applications want, that no server should carry unasked, and that nobody should have
to go and find.

This milestone is the short tail. A **bundled module** is developed in this repository, ships
inside the release tarball, and appears on the Modules tab as a card with an Install button.
It is still a module: nothing is loaded until an admin installs one, and what installing does
is what installing has always done. What ships is the module and **not its dependency tree** —
`plugins/rss` is an `index.js` and a `package.json` naming `rss-parser`, and `rss-parser` is
downloaded by npm at the moment somebody clicks Install.

**Milestone definition of done:** an admin on a fresh host opens Settings → Modules and sees,
above the install form, a card headed *RSS feeds* saying what it supplies, that installing
downloads `rss-parser` from npm, and that installing lets it connect to any host. One click
installs it: npm fetches the dependency, the module loads on a worker with the permission the
card named, and *RSS feed* is on the New table screen. A table pointed at a feed serves the
feed's items as rows. Nothing was typed, nothing was looked up, and a server that never
pressed the button downloaded nothing and runs nothing extra. The catalog's second card is a
**Python** module — one manifest, one endpoint, one button, whichever language.

**Not in this milestone:** a remote catalog or a module store, which is a registry and a trust
question of its own; version pinning or an upgrade notification for a bundled module (a
reinstall takes the release's copy, and the release is the version); dependency vendoring; and
turning `@saltcorn/rss` itself into the bundled one — `plugins/rss` is written against this
server's own reading of a table provider, and v1's plugin stays as the compatibility test it
already is.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

---

# The specification

### 1. What ships, and what does not

The **code** ships; the **dependency tree** does not. `plugins/<id>/` is source: an `index.js`
and a `package.json`, or a `pyproject.toml` and a package directory — kilobytes. Everything it
depends on is fetched by npm or pip when an admin installs it.

The alternative — vendoring the trees — would put a Markdown renderer, an XML parser and
everything under them into every artifact, downloaded by every installation, to be used by
some. This way a server that installs nothing downloads nothing, and the release tarball stays
the size it is.

The cost is that installing a bundled module **reaches the network**, and a host with no
registry access can install none of them. That is said on the card rather than discovered at
the click.

### 2. `ModuleSource::Bundled`, and why the id rather than the path

A bundled install is an ordinary **local directory** install with the directory filled in by
the server. What is stored on the row is the catalog **id** — `rss` — because the directory is
`<install prefix>/plugins/<id>`, which is a different string on a developer's checkout, on a
host running the tarball, and on that host after an upgrade moved the prefix. An id survives
all three, so the row stays reinstallable; a path would not.

`ModuleServices::install_package` resolves the id against the catalog and hands npm or pip the
directory. `Installer` and the Python environment never see `bundled`: reaching them with one
is a wiring mistake and says so.

### 3. The manifest

One `feldspar-module.json` per directory; the directory's name is the id.

| key | meaning |
| --- | --- |
| `name` | the package's own name once installed — the key `_sc_modules` is keyed by, and how the card knows it is already installed |
| `language` | `javascript` or `python`: which host loads it, which manager installs it |
| `title` · `description` · `supplies` | the card, in the admin's words. Nothing is installed yet, so there is no package to read them out of |
| `installs` | what installing downloads — the dependencies that are deliberately not in the release |
| `permissions` | what it asks to be allowed to reach (§4) |

`name` disagreeing with the package's own is the failure that silently breaks everything — the
row, the loaded set and the already-installed check all key on it — so a test reads both files
and asserts they agree.

### 4. The grant, which is the one thing worth arguing about

`_sc_modules.permissions` is deliberately the **server's** record and not the package's: what a
package declares is a request, and a request that granted itself would be no permission model
at all.

A bundled manifest's `permissions` is such a request, and it is granted **by the install** —
because the Modules tab prints it beside the button, in the words the permission screen uses
("Installing lets it connect to any host"). That is a person granting a permission after
reading it, which is the rule; it is not a package granting itself one.

On a **first** install only. A reinstall is how a bundled module is upgraded when a new release
ships a newer copy, and an admin who has since narrowed what it may reach must not have that
undone by an upgrade.

### 5. `net: ["*"]` — any host

The RSS module cannot be granted a host list, because a feed's URL is typed into the **table's**
settings and not the module's: an allow-list would have to be edited every time somebody adds a
table. So `*` is a `net` entry meaning any host — one entry, written out, printed back as "any
host" wherever a set is shown.

It is the **only** wildcard. A filesystem one would be a module that may read the database
password file; an environment one a module that may read every secret this process was started
with. Neither has the excuse this one has, which is that the host is chosen after the grant, so
`*` is refused in the other lists by the checks that were already there — and refused **by
name** in `env`, where it would otherwise be accepted as a variable nobody has.

### 6. Where the catalog is, at run time

The same problem the two UI bundles have, and the same answer. `crates/sc-cli/build.rs` records
`SC_PLUGINS_DIR`: the checkout's `plugins/` normally, `$SC_BUNDLE_PREFIX/plugins` for a binary
being packaged. Nothing is built — there is nothing to build — so `SC_BUILD_ADMIN=0` does not
turn it off and a `--no-ui` artifact still ships the catalog.

Nothing here fails. A directory that is not there is an **empty catalog**; a manifest that will
not parse is an issue on the boot log and one card missing. A server whose copy is broken must
still install from npm, list what it has, and say what is wrong.

---

## Phase 1 — The catalog

- [x] 1.1 `sc_module::bundled`: `BundledModule`, `BundledModules::{discover, get, require,
      modules, issues, root}`, the manifest reader, and `BUNDLED_IN_CHECKOUT`. Never fails;
      every unreadable entry is an issue naming the file. A Python manifest that requests
      permissions is **refused** rather than ignored — there is nothing to enforce it, and a
      grant nothing enforces is worse than no grant.
- [x] 1.2 `ModuleSource::Bundled`: parsed, stored, offered by both languages, and refused by
      `Installer` and by the Python environment with the sentence that says who was meant to
      resolve it.
- [x] 1.3 `ANY_HOST` (`*`) in `ModulePermissions::net`, `any_host()`, "may connect to any
      host" in `sentences()`, and `net_option` translating it into Deno's `allow_net` with no
      argument — the one place `Some(vec![])` is correct. `*` refused in `env` by name; the
      path checks already refuse it.
- [x] 1.4 Unit tests: the manifest reader against manifests written by the test, the wildcard
      in and out of the permission set, and a stable catalog order whatever the filesystem
      hands back.

## Phase 2 — The two modules

- [x] 2.1 `plugins/rss` — `@feldspar/rss`, a table provider over `rss-parser`. Written against
      this server's own reading of a provider: no `@saltcorn/*` require anywhere in it, a
      `configuration_workflow` that is a function returning steps, seven columns with `guid` as
      the key, and a TTL cache in module scope. Ignores `where` and `limit` deliberately — the
      catalog applies the query's real filter to whatever comes back, and twenty rows off the
      top of a feed are not the twenty a query asked for.
- [x] 2.2 `plugins/markdown` — `feldspar-markdown`, two functions over the `markdown`
      distribution, advertised through a `saltcorn.plugins` entry point. The Python half, so
      that "one catalog, one button, either language" is a fact rather than a claim.
- [x] 2.3 `plugins/README.md`: what a bundled module is, the manifest, and how to add one.

## Phase 3 — The API and the tab

- [x] 3.1 `listModules` carries `bundled`: the catalog with `installed` decided against the
      loaded set by package name. One endpoint, because the tab asks one question and two
      requests would be two loading states for one screen.
- [x] 3.2 `installModule` accepts `source: "bundled"` with the id as `location`. The language
      comes from the catalog, and so do the permissions (§4) — the request carries neither.
- [x] 3.3 The Modules tab's catalog section: a card each with what it supplies, what installing
      downloads, what installing grants, and one button. Installed is **Reinstall**, not a
      disabled button, because a reinstall is how a bundled module is upgraded.
- [x] 3.4 `modules.ts` helpers and their tests: the two sentences, the toolchain block per
      card, the requested-permission narrowing (unreadable is the *closed* set, never an open
      one), and the not-installed-first ordering.

## Phase 4 — Packaging

- [x] 4.1 `crates/sc-cli/build.rs` records `SC_PLUGINS_DIR`; `ServerConfig::plugins_dir` and
      `main.rs` fill it in; `ModuleServices::install` discovers the catalog from it.
- [x] 4.2 `scripts/build-static.sh` stages `plugins/`, `install.sh` copies it to the prefix,
      the Dockerfile copies it into the export stage, and both strip anything a package manager
      left in a plugin directory. Not conditional on `--no-ui`: there is nothing to build.
- [x] 4.3 The README of the tarball, `docs/OPERATIONS.md`'s layout and its build-time variable
      table.

## Phase 5 — The proof

- [x] 5.1 `sc-module`'s `bundled_catalog` test reads the **real** `plugins/` directory: every
      manifest parses, every `name` agrees with the package's own, every `installs` entry is a
      dependency the package actually declares, and each language's package is the shape its
      manager installs. No npm, no pip, no network — so it runs in every `cargo test`.
- [x] 5.2 `sc-module`'s `bundled_rss` test (ignored; npm): installed from the directory it
      ships in, loaded with the grant its card asks for, and read — the settings the New table
      dialog renders, the seven columns, the rows off a feed served on `127.0.0.1`, the
      fallback key when a feed omits `guid`, `max_items`, and read-only. Plus the denial when
      the grant is not there, and the sentence when no feed URL is configured.
- [x] 5.3 `sc-python`'s `bundled_markdown` test (ignored; pip): installed from the catalog,
      imported through its entry point, both functions called.
- [x] 5.4 `sc-server`'s API tests: the catalog listed before anything is installed; an id
      nothing ships refused naming what does, with npm never run; and (ignored) the one-call
      install — `{source: "bundled", location: "rss"}` — landing a loaded module with the
      card's permission, the provider on the New table screen, a **reinstall** that keeps a
      narrowed permission set, and a delete that leaves the catalog entry behind.

## Phase 6 — Documentation

- [x] 6.1 `docs/TECHNICAL_DESIGN.md` §15.1a, and `plugins/` in the repository tree.
- [x] 6.2 `docs/tutorial-modules.md` gains a step 0 — the modules that came with Saltcorn —
      and the wildcard in the permissions step.
- [x] 6.3 README §3, and the CHANGELOG.

## Phase 7 — Found in production, after the milestone

Installing the bundled RSS module on `feldspar-dev` — the first time any module was installed
from a **release tarball** rather than from a checkout — failed with `it did not load: the
module host stopped before answering this call`, beside the green
`@feldspar/rss 0.1.0 installed, supplying nothing this version of Saltcorn loads`. Not an RSS
problem and not a bundled-modules problem: no JavaScript module could load on any deployed
tarball, because `deno_core` reads its extensions' JavaScript from the absolute paths they had
on the **build machine**.

- [x] 7.1 `crates/sc-module/build.rs` builds the V8 startup snapshot, and embeds the
      `lazy_loaded_*` sources the snapshot does not consume. `wiring.rs` starts every worker
      from it. `deno_runtime`'s `transpile` feature goes with it — the snapshot holds the
      transpiled form — taking `deno_ast` out of the server's link.
- [x] 7.2 `sc_module::prime_v8`, and `sc_expr::set_isolate_prime` for the hook it is
      registered through: V8 shares one read-only heap per process, so the snapshot-backed
      isolate has to be built first and **held**. `sc_server::js_evaluator` wires the two.
- [x] 7.3 `wiring::try_build_worker`: `JsRuntime::new` panics rather than returning, and a
      panicking worker thread drops its call table instead of failing the calls in it — which
      is why the one sentence naming the cause went only to stderr. The Modules tab now shows
      it.
- [x] 7.4 `scripts/build-static.sh` refuses a cross-architecture build, and `build.rs` refuses
      it again: a snapshot belongs to the architecture that serialised it.
- [x] 7.5 `sc-module`'s `two_pools` test: the pool ordering, and a worker started inside a
      mount namespace with the cargo registry hidden behind a tmpfs — the deployment host,
      reproduced.

---

## Explicitly OUT of scope for this milestone

- **A remote catalog or module store.** Fetching a list of modules from somewhere is a
  registry, and a registry is a trust question; this milestone's entire claim is that these
  modules are already inside what you are running.
- **Version pinning and upgrade notification for a bundled module.** The release is the
  version. A reinstall takes what the release ships, and there is no state that says "a newer
  one is available" because the newer one arrives with the binary.
- **Vendoring dependencies.** §1.
- **Replacing `@saltcorn/rss`.** v1's plugin stays as the compatibility test it is
  (`crates/sc-module/tests/rss_provider.rs`); `plugins/rss` is a different claim about a
  different thing.
- **A `--bundled-modules-dir` flag.** The catalog is part of the artifact, like the IDE bundle
  and for the same reason: there is nothing for an operator to decide.

## Carried past this milestone

- **More bundled modules.** The mechanism is the milestone; the catalog is two entries. A CSV
  or JSON-over-HTTP table provider is the obvious third, and it needs nothing new.
- **A "what changed" line on a Reinstall.** The card cannot say what a reinstall would bring
  because it does not know what version is installed against what version ships — the module's
  `package.json` version is read at install and the catalog does not carry one. Adding
  `version` to the manifest would answer it, and it should be done when there is a bundled
  module whose upgrades matter.
- **Granting a narrower set at install time.** The card grants what the manifest asks for, and
  an admin who wants less edits it afterwards. A form on the card would be a permission editor
  in two places.
