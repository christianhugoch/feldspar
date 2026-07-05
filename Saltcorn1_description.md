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
    which expose data sources that are not managed by Saltcorn as if they were tables.

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
    code, though individual steps can still run JavaScript.

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
