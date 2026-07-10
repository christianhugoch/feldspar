This is a description of Saltcorn v1.X, provided to guide the design of the next version.

## Overview

Saltcorn is an open-source, extensible no-code database application builder. Applications
are built by an administrator through a web interface (no code required for standard
applications), and are served as traditional server-rendered web pages with progressive
enhancement, rather than as a client-side single-page application. It is implemented in
Node.js (with a substantial TypeScript portion) on top of Express, with PostgreSQL as the
primary database backend and SQLite supported for development, embedded and mobile use.
A single Saltcorn server can run one application or many (multi-tenancy: each tenant is
an isolated application on its own subdomain, backed by a separate PostgreSQL schema).

The core building blocks of an application are tables, views, pages, triggers and files,
all of which are defined by rows in the database rather than by generated source code.
Almost everything else — field types, view templates, actions, themes, authentication
methods — is supplied by plugins, and a base plugin supplies the built-ins using the same
mechanism.

## Entity types

Saltcorn applications contain the following entity types:

* Tables: These are relational database tables and consist of fields of specified types
    and rows with a value for each field. Fields optionally can be required and/or unique.
    Every field has a name, which is an identifier that is valid in both JavaScript and SQL,
    and a label, which is any short user-friendly string. Every table has a primary key
    (composite primary keys are not supported) which by default is an auto-incrementing integer
    with name `id` and label ID. The `id` primary key field is always unique and not-null by
    definition — never set unique=true or not_null=true on it. Fields can also be of Key type
    (foreign key) referencing a primary key in another table, or its own table for a self-join.
    Tables can have calculated fields, which can be stored or non-stored. Both stored and
    non-stored fields are defined by a JavaScript expression, but only stored fields can
    reference other tables with join fields and aggregations. Field types (String, Integer,
    Float, Date, Bool, Color, JSON, etc.) are provided by plugins; a type bundles validation,
    optional type attributes (e.g. min/max, options for a String select) and a set of
    fieldviews. Tables are managed through Saltcorn's schema layer, which alters the
    underlying SQL tables as fields are added, removed or changed, and can also import/export
    tables as CSV. Plugins can additionally supply external tables and table providers,
    which expose data sources (that are not managed by Saltcorn) as if they were tables.

* Views: Views are elementary user interfaces into a database table. A view is defined by
    applying a view template (also sometimes called a view pattern, the two are synonymous) to
    a table with a certain configuration. The view template defines the fundamental relationship
    between the UI and the table. For instance, the Show view template displays a single database
    row, the Edit view template is a form that can create a new row or edit an existing row, the
    List view template displays multiple rows in a grid, and the Filter view template sets the
    view state for other views on the same page. Views can embed views, for instance Show
    can embed another row through a Key field relationship, or some views are defined by an
    underlying view. For instance, the Feed view repeats an underlying view for multiple rows.
    New view templates are provided by plugin modules.

    Two supporting concepts define how views render and what they show:

    - Fieldviews control how an *individual* field value is shown or edited, and are specific
      to a field type. For example, a Date value can be rendered as a formatted date, as a
      relative time ("2 days ago"), or edited with a calendar picker. Each type ships with a
      set of fieldviews and plugins can add more; fieldviews may have their own configuration
      (e.g. a format string or placeholder). View templates combine fieldviews into a complete
      interface — the view configuration chooses which fieldview to use for each field.

    - View state determines *which* row(s) a view shows and is carried in the URL query
      string. For single-row views (Show, Edit) the state identifies the row; for
      multi-row views (List, Feed) it filters the subset of rows shown. Because state
      lives in the query string, any filtered or drilled-down screen is shareable as a URL.
      Views embedded on the same page share state, which is what makes clickable,
      cross-filtering dashboards possible (e.g. a Filter view narrowing a List view).

    A view, like a page, is addressable at its own URL (`/view/{name}`) and views also back
    the auto-generated REST API.

* Triggers: Triggers connect actions to events. An event is an occurrence at a point in
    time with a type — table events (Insert, Update, Delete and their Validate variants),
    periodic events (hourly, daily, weekly), login/logout, errors, custom user-defined events,
    or API calls — and may carry a user, a channel (typically a table name) and a payload
    (typically a row). An action is the response: built-in and plugin-provided actions cover
    things like modifying rows, sending emails, calling webhooks, running JavaScript code, or
    multi-step workflows. A trigger binds one action to one event (scoped to a table or
    channel where applicable) with a configuration whose fields are declared by the action.
    Actions can also be run directly from buttons placed in views and pages, without an
    event. Events can be recorded in a configurable event log for auditing and debugging.
    A trigger's action can also be a workflow — a durable multi-step program rather than a
    single action (see the Workflows section below).

* Pages: A page has static content but can also embed views for dynamic content. Pages can
    be either defined by a Saltcorn layout, for pages that can be edited with drag and drop, or
    by HTML for more flexible graphic designs. HTML pages should be used for landing pages.
    Unlike views, pages are not tied to a table; they compose static content and any number of
    embedded views, and shared view state across those views enables dashboard-style
    interaction. Pages are addressable at `/page/{name}`, and a page can be designated as
    the root page shown at `/` (configurable per role).

* Files: uploaded files (images, documents) are stored on disk or in the database, organised
    in folders, with a minimum-role-to-access setting per file. Fields can reference files,
    and fileviews (e.g. download link, image display) control how they are rendered.

* Plugin modules: plugins are npm packages (or store extensions) that extend Saltcorn.
    Before they can be used, they need to be installed; they are installed and updated at
    runtime (via live-plugin-manager) without redeploying the server. A plugin exports a
    plain object declaring which of the extension points it supplies:

    - `types` — new field types, each with fieldviews and validation
    - `fieldviews` — additional fieldviews for existing types
    - `viewtemplates` — new view templates
    - `actions` — new action types for triggers and buttons
    - `eventTypes` — new event types
    - `functions` — functions made available to formulas and code actions
    - `layout` — a theme controlling overall page markup and styling (layout themes in
      Saltcorn are plugin modules)
    - `routes` — custom HTTP endpoints
    - `external_tables` / `table_providers` — read-only or dynamically-provided tables
    - `fileviews` — renderings for files
    - `authentication` — additional authentication methods (e.g. OAuth providers)
    - `headers` — scripts/stylesheets injected into every page

    A plugin may also have a configuration that sets options for that plugin, declared as a
    `configuration_workflow`. Besides plugins, "packs" bundle tables, views, pages and plugin
    references into an installable application template; a public module store lists both.

## Workflows

Workflows, introduced as a stable feature in Saltcorn 1.2.0, extend the trigger/action
system from single actions to durable, multi-step execution with control flow logic.
A workflow is built and triggered like any other trigger — it can run periodically on a
schedule, in response to a system or table event, or be initiated by a user from a button —
but instead of one action it consists of a sequence of named steps.

* Steps and context: steps communicate by reading from and writing to a shared context,
    a JSON object that accumulates state as the run proceeds. A step can run any action
    (including the built-in code actions), set context values, or interact with the user.

* Control flow: workflows support loops, conditionals and error handling, so multi-step
    logic with branching ("loops, ifs and buts") can be expressed without dropping down to
    code, though individual steps can still run JavaScript. Each step has a name and a 
    next_step attribute: this is a JavaScript formula (evaluated against the context at the end of running that step), where the names of all of the other steps 
    are in scope as their identifier. The formula value is the name of the next step. So if we have 
    steps with names step1, step2 and step3, and an integer `age` in the context then step1's next_step can be
    `age<18 ? step2 ? step3`. the step names are simply introduced as strings containing the step name - 
    i.e. step2 = "step2" is in the evaluation scope for the next_step formula. loops are handled with a special ForLoop step type. 

* Durable execution: a workflow run is persisted with its context and current step, so
    runs can pause — for example while waiting for user input — and resume later, surviving
    beyond a single request/response cycle.

* User interaction: workflows can request information from users and display results
    mid-run in two ways: through modal popup dialogs, or through the WorkflowRoom view
    template, which exposes a running workflow to the user as a chat interface. The chat
    presentation is particularly aimed at AI-backed workflows (e.g. LLM-driven agents
    interleaved with forms and actions).

* Server-driven interaction: since Saltcorn 1.4, both views and workflows can push
    interactions to the user driven by the server, over the same websocket connection used
    for real-time collaboration in views (e.g. live-updating Kanban boards and Edit views),
    and text generated by an LLM can be streamed to the user as it is produced.

## Authorization

Each user in Saltcorn has a role set by the user's role_id 1-100. Lower roles are more powerful, with 1 being the admin role
and 100 being the role of the unauthenticated user (public). By default the roles are admin (1), staff (40), user (80) and
public (100); roles can be added or removed. Tables, views and pages have a "minimal role" to access/read/write 1-100 and the user's
role_id has to be less than or equal to this minimal role to access. Because pages, views and tables each carry their own
minimum role, access must be granted consistently at every layer: a user needs access to the view and to the underlying
table operations the view performs.

Tables can have ownership by field (key to user) or formula. If this is satisfied, the user can access the row even if they do not meet the minimal
role to read or write. But if they do meet the minimal role criteria for the table as a whole, they can access all rows. Therefore a user who has a role_id less than or equal to the minimum role to read (or write) can read (or write, respectively) all rows even if ownership is set. Ownership only determines access for users with a role_id greater than the minimum role to read (or write).

## Agents

Agents are implemented as actions in the [agents](https://github.com/saltcorn/agents) plugin. An agent action is defined by enabling a number of skills, which each have a configuration. A skill is an elementary capability of an agent system. Most skills enable a tool for the LLM inference loop, but other skill change the chat behaviour.

Some examples of skills:

* A tool to query a chosen database table by matching against any field
* A Tool to making an HTTP request. 
* Expose a Javascript function written by the 
* A Tool for the AI to generate and then run Javascript code. The user chooses whether to give the code access to tables and HTTP requests.
* Long term memory - enable tools for storage and retrival into momeries stored in a database table
* MCP - connect the agent to an MCP server
* Model picker - show a dropdown to the user where they can change the inference model
* PreloadData - load data from the database into the system prompt
* Use any Saltcorn action or workflow as a tool
* Subagent - hand over to a different agent that has a different set of tools
* Web search - tool to search the internet for relevant information
* Plan approval - presents the user with a plan for solving the problem with an approval buttton in the chat. When approved, a user-defined system prompt is injected.


The agents can be run either by attaching them to events (table inserts, inbound API calls etc; in chich case an initial prompt, based on the variables in the triggering row has to be specified) or by building a view based on the Agent chat viewpatterns which is configured by picking an agent action, giving the user an interactive chat interface similar to the chatgpt interface. Previous chats can be accessed on the left in this interface, and chats can be shared with other users

Copilot (building Saltcorn apps with AI) is implemented as two different interfaces to a copilot agent, which is composed of basic copilot skills for building views, workflows. One interface to the copilot agent is a standard chat interface where the user can give type instructions for something to be built. The second interface is called the AppConstructor in which an app is developed in a number of stages:

* Description phase: the user describes the app with as much detail as they want
* Clarification phase: the AI agent asks the user questions about anything that needs clarification in the description
* Research: The AI agent searches the internet for relevant information. For instance, if the application is in a regulated industry the agent looks up the relevant regulatory guidance.
* Requirements: The AI generates the list of requirements. Each is ranked 1-5 by importance. The user can edit, delete, add and rescore requirements. 
* Planning phase: The AI agent builds a plan for implementing the application. This is split into phases and each phase is split into tasks. The tasks in each phase are ordered both by their dependency on other tasks and what entity type they are building (tables need to be built before views for instance). 
* Execution phase: The use can run a single task or all tasks for a phase. After the phase is complete, the user can test the app and give which is corrected. The tasks for the next phase is adjusted according to feedback and the AI's own progress report.
* User feedback: the copilot can build a user interface for users to give feedback and suggestions. When these are approved by admin they become new tasks and are implmented. 
* Self-healing: the AppConstructor can respond to any error in the system and automatically fix a build problem in the application. 

This is aligned to the way software in traditionally built, with the humans collaborating closely with the AI.

## Architecture and code base

Saltcorn is a monorepo of npm packages (managed with Lerna). The important packages, in
rough dependency order:

* `@saltcorn/db-common`, `@saltcorn/postgres`, `@saltcorn/sqlite` — the database layer.
    A common query-building/connection abstraction with PostgreSQL and SQLite drivers behind
    it, so all higher layers are database-agnostic.

* `@saltcorn/data` — the core of the project. It defines the entity model as classes
    (Table, Field, View, Page, Trigger, File, User, Plugin, ...) and handles their persistence:
    application structure itself is stored in database tables (prefixed `_sc_`), so an
    application is data, not generated code. This package also implements the schema layer
    (creating/altering the users' SQL tables), row queries with joins and aggregations,
    calculated fields, formula evaluation, the state/access-control logic, migrations, and
    the base plugin containing the built-in types, fieldviews and view templates.

* `@saltcorn/markup` — server-side HTML generation. A `tags` module exposes HTML elements
    as JavaScript functions, from which forms, tables and layouts are built; themes
    (layout plugins) translate an abstract layout tree into concrete markup.

* `@saltcorn/server` — the Express HTTP server: routing for the admin interface and for
    user-facing views/pages, authentication (via Passport), sessions, CSRF protection, the
    auto-generated REST API, and Socket.io for real-time features (collaborative rooms,
    server log streaming). Multi-node deployments synchronise through PostgreSQL
    LISTEN/NOTIFY.

* `@saltcorn/builder` — the drag-and-drop view/page builder, a React application built on
    Craft.js; it is the main client-side-heavy part of an otherwise server-rendered system.
    Blockly is available for visual programming of actions and CodeMirror for code editing.
    A separate `workflow-editor` package provides the visual editor for workflow steps.

* `@saltcorn/cli` — the `saltcorn` command-line tool (built on oclif) for serving,
    tenant/user management, backup and restore, plugin development and installation.

* `@saltcorn/mobile-app` / `@saltcorn/mobile-builder` — build an application into a
    mobile app (Capacitor-based) running against a local SQLite database with
    synchronisation to the server.

Cross-cutting characteristics worth preserving or consciously revising in a next version:

* Everything is a plugin: built-in types and view templates go through the same extension
    API that third-party plugins use, which keeps the core honest and small.
* Applications are rows, not code: create/configure/backup/restore of a whole application
    is database manipulation, and "packs" serialize applications for sharing.
* Server-rendered UI with a declarative layout tree, themed by plugins; view state in the
    URL makes screens addressable and composable.
* Runtime extensibility: plugins install, update and reload without a server restart or
    redeploy.
* Multi-tenancy via schema-per-tenant on one server process.


## Saltcorn Mobile Apps

### For users

Saltcorn can turn any Saltcorn application into a native **Android or iOS app**
without writing native code. From the admin panel (**Settings → Mobile app**), an
administrator fills in a build form and Saltcorn produces an installable app
package — an `.apk`/`.aab` for Android or an `.ipa` for iOS.

The generated app runs your existing **views and pages** natively on the phone.
You choose an *entry point* (the first view or page the user sees, optionally
different per user role), pick the target platform(s), and configure options such
as an app name, icon, splash screen, and which plugins/themes to include. When the
build finishes, the result files appear in the server's `mobile_app` file folder,
ready to download and publish to the app stores (or sideload for testing).

Beyond being a thin wrapper around your website, the app can do things a browser
cannot:

- **Offline mode** — data for selected tables is copied into a database on the
  phone, so users can keep reading and editing while disconnected. Changes are
  reconciled with the server later.
- **Synchronization** — offline changes sync back to the server automatically on
  reconnect, when the app resumes, on a timer (background sync), or when the
  server pushes a "please sync" signal.
- **Push notifications** — via Firebase (Android) and APNs (iOS).
- **Share-to** — the app can appear in the OS "Share" sheet so users can send
  content from other apps into your Saltcorn app.
- **Native login** — including single-sign-on / OAuth methods that redirect back
  into the app.

The build can run **locally** on the server machine (requires the platform
toolchains — Android SDK/Gradle, or Xcode + CocoaPods on a Mac) or inside a
prebuilt **Docker image** (`saltcorn/capacitor-builder`), which is the recommended
way to build Android apps on a server that does not have the Android toolchain
installed. iOS builds require a macOS machine with Xcode.

---

### Technical implementation

A Saltcorn mobile app is a [**Capacitor**](https://capacitorjs.com/) application: a
native shell around a WebView with JavaScript access to native APIs (filesystem,
SQLite, network status, push, share intents). The key idea is that the app does
**not** point the WebView at the server. Instead, the **Saltcorn engine itself runs
inside the WebView** — the same data and markup code that renders views on the
server is bundled into the app and executes on the device. The app is effectively a
"Saltcorn server in the browser," which is what makes offline operation possible.

Three pieces cooperate:

- **The build UI** (in the server admin routes) renders the build form, gathers the
  available entry points, plugins, signing files, and offline tables, probes which
  toolchains are present, and launches a build.
- **The builder** (`saltcorn-mobile-builder`) orchestrates the build. It copies a
  Capacitor project template, writes the app's configuration, bundles the Saltcorn
  engine and selected plugins with webpack, generates a schema snapshot and a
  prepopulated SQLite database to ship inside the app, applies platform-specific
  patches (Android manifest/Gradle, iOS plists/Podfile/entitlements), and finally
  invokes the native toolchain — locally, or inside the Docker builder image — to
  produce the installable package.
- **The runtime app** (`saltcorn-mobile-app`) is the template that ships in every
  app. On startup it loads the bundled engine, initializes a local SQLite database
  from the shipped schema, resolves authentication from a stored token, and renders
  the entry point. Navigation is handled locally: rather than making network
  requests, the app replays the server's request-handling routes in the browser
  (using a router plus mock request/response objects) and injects the resulting
  HTML into an iframe. When online it calls back to the server's API for
  authenticated operations; when offline it reads and writes the local database and
  reconciles changes later via the sync engine. Optional features — push, share
  extension, background sync — are only included when enabled.

### Data flow summary

```
Admin build form
   → builder: copy template, write config, bundle engine + plugins,
     build schema + SQLite, patch platforms, run native build (local or Docker)
   → .apk / .aab / .ipa copied into the server's `mobile_app` folder

On the device:
   load engine → init local SQLite from schema → resolve auth →
   render entry point → online: call server API │ offline: local SQLite + sync
```

## Saltcorn Email

### For users

Saltcorn can send email from your application — both **system emails** (address
verification, password resets) and **application emails** you design yourself and
trigger from your app's logic.

An administrator configures sending once under **Settings → Email**: the SMTP
server details (host, port, username/password, TLS), or a modern OAuth2 / Microsoft
365 (Graph) connection, plus the "from" address. A **test email** button confirms
the settings work before you rely on them.

Application emails are sent by the **Send email** action, which you attach to a
trigger or a workflow step. You control:

- **Recipients** — a fixed address, the current user, or an address taken from a
  field on the row (including a link to the users table); plus cc and bcc.
- **Subject** — static text or a formula, with `{{ }}` interpolation of row and
  user data.
- **Body** — the most powerful option is to render one of your **views** as the
  email, so the message reuses the layout you already designed and comes out as a
  responsive, email-client-friendly HTML message. Alternatively the body can come
  straight from a text, HTML, or MJML field on the row.
- **Attachments** — files from a File field on the row, or from related rows.
- Extras such as a per-message **language override**, an "only if" condition, and a
  field to record that sending succeeded.

Because emails are built from your existing views, they automatically match your
app's look and adapt to the recipient's screen without any HTML hand-coding.

---

### Technical implementation

Email has two foundations: a **transport** and a **renderer**. The transport is
built on demand from the site's configuration (`smtp_*` and `email_from`). In the
common case this is a [**nodemailer**](https://nodemailer.com/) SMTP transport,
honouring port, forced TLS, self-signed certificates, and either password or
**OAuth2** authentication (tokens refreshed automatically when expired). If the site
is configured for **Microsoft 365 / Graph**, a drop-in transport posts to the Graph
`sendMail` API instead, exposing the same `sendMail(...)` interface plus
throttling/back-off. Every part of Saltcorn obtains a transport this way and calls
`sendMail`, so switching providers is purely a configuration change. Because
ordinary web HTML renders poorly in email clients, message bodies are produced
through [**MJML**](https://mjml.io/), which compiles to table-based, responsive,
broadly-compatible email HTML; the `saltcorn-markup` package supplies the MJML tag
helpers (`<mj-section>`, `<mj-column>`, "bulletproof" buttons, etc.) and a renderer
that translates a Saltcorn **layout** — the same structure the web renderer uses —
into MJML.

These come together in the **Send email** action (a base-plugin action on triggers
and workflow steps) and in **system emails** (verification, password reset). To use
a view as the body, Saltcorn runs the view against a mock request/response (flagged
as email generation so components resolve absolute URLs from the base URL), wraps
the markup in an MJML document, and compiles it to final HTML; MJML fields are
compiled the same way, while text and HTML fields pass through unchanged. At run
time the action resolves recipients, evaluates the subject and any "only if"
condition against the joined row and user, builds the body, attaches files (from a
field or related rows), obtains a transport, and sends — optionally recording
success back on the row. It also runs in **workflow mode**, where recipients,
subject and body are plain interpolated strings rather than a view, and system
emails reuse the same rendering and transport so they inherit the site's setup and
branding.

### Data flow summary

```
Config (smtp_* / email_from) ─► transport (nodemailer SMTP | OAuth2 | MS Graph)

Send email action / system email
   → resolve recipients, subject, condition (row + user, {{ }} interpolation)
   → build body:
        view    → run view → wrap in MJML → compile to responsive HTML
        MJML field → compile to HTML
        text / HTML field → as-is
   → attach files (field or related rows)
   → transport.sendMail(...)  → (optionally record confirmation on the row)
```

## Resources

Code: https://github.com/saltcorn/saltcorn
Wiki: https://wiki.saltcorn.com/
Especially relevant wiki pages:
* Code base: https://wiki.saltcorn.com/view/ShowPage/saltcorn-code-base
* Plugin model: https://wiki.saltcorn.com/view/ShowPage/plugin-model
* Concepts: https://wiki.saltcorn.com/view/ShowPage/concepts
* Fieldviews and view templates: https://wiki.saltcorn.com/view/ShowPage/understanding-fieldviews-and-view-templates
* View state: https://wiki.saltcorn.com/view/ShowPage/view-state
* Events and actions: https://wiki.saltcorn.com/view/ShowPage/events-and-actions-terminology

Workflows are not documented well in the wiki; these blog posts have more information:
* Saltcorn 1.2.0 (workflows): https://blog.saltcorn.com/view/ShowPost/saltcorn-120---first-stable-release-with-workflows
* Saltcorn 1.4 (real-time collaboration, server-driven workflow interaction): https://blog.saltcorn.com/view/ShowPost/saltcorn-14---real-time-collaboration
