# The `code` framework: bring your own build

The [React tutorial](tutorial-react-todo.md) covers the path most applications should take:
Saltcorn creates the project, generates a typed client and hooks for your tables, installs the
dependencies and builds it. This document covers the other framework — **`code`** — which makes
no assumptions at all.

Use it when React's conventions are the wrong ones: a Next.js or SvelteKit app, a project that
already exists, a bundler that is not Vite, a layout that is not `<project>/dist`, or a
front end that is not React. Saltcorn's side of the arrangement shrinks to: run your build
command, serve what it emits, and generate a typed API client if you want one.

**What you give up**, relative to `react`: nothing is scaffolded, nothing is installed for you,
there are no generated hooks, and you state five settings instead of two, keeping them
consistent yourself. You need a shell on the server host (or another way to put files in the
store) to create the project in the first place.

## Prerequisites

Start the server with a base domain, so applications have somewhere to be served:

```bash
saltcorn serve --base-domain localhost
```

## Step 1 — Add a file store and a data model

Both in the admin UI, exactly as in the React tutorial:

- **File stores → New file store**: name `apps`, backend `local`, **Directory** `/srv/apps`, and
  tick **Create the directory if it does not exist** if it is not there yet. It connects
  immediately, with no restart. (`--file-store apps=/srv/apps` at startup does the same thing
  and is handy for scripted deployments.)
- **Tables → New table name** `tasks` → **Create table**, then open it and add:
  - `title` — SQL type `text`, not nullable
  - `done` — SQL type `boolean`, nullable

## Step 2 — Create the project yourself

On the server host, inside the store directory:

```bash
cd /srv/apps
npm create vite@latest todo -- --template react-ts
cd todo && npm install && git init
```

You now have `/srv/apps/todo`, building to `/srv/apps/todo/dist`. Any toolchain works here; the
only requirement is that your build command emits static files into a directory, and that an
`index.html` is among them if you want deep links to resolve.

## Step 3 — Register the application

**Applications → New application**, choosing **Code (bring your own build)**:

| Field | Value |
|---|---|
| Name | `Todo` |
| Subdomain | `todo` |
| Framework | **Code (bring your own build)** |
| **File store** | `apps` |
| **Source directory** | `todo` |
| **Output directory** | `todo/dist` |
| **Build command** | `npm run build` |
| **Generated client path** | `todo/src/client.ts` |
| Tables | `tasks` |
| APIs (Provider / Mount) | `rest` / `/api` |

Every path is relative to the file store, and they must agree with each other and with the
project you created — that consistency is the part the `react` framework takes off your hands.
The build command is split on whitespace and run directly (no shell), so pipes, globs and quoted
arguments do not work; put anything more involved in a script or an npm script and name that.

`rest` at `/api` projects each declared table into four endpoints — `GET/POST /api/tasks`,
`PUT/DELETE /api/tasks/{id}` — plus `/api/login`, `/api/logout` and `/api/whoami`. Save.

Leave **Generated client path** empty if you do not want a generated client; the app is then an
ordinary static bundle that can call the API however it likes.

## Step 4 — Build

Press **Build** on the app's row. The server emits the typed client (if you asked for one), runs
your build command in the source directory, and serves the output directory with an SPA fallback
to `index.html`. The banner carries your bundler's log, or its diagnostics if it fails; a failed
build leaves the previously built version serving.

**Dependencies are yours to install.** Unlike a scaffolded React app, `code` runs no
`npm install`: if the build needs it, run it on the host, or make your build command do it.

## Step 5 — Use the generated client

The generated `client.ts` is a typed factory. Endpoint paths already include the API mount, so
no base URL is needed:

```tsx
// src/App.tsx
import { useEffect, useState } from "react";
import { createClient } from "./client";

const api = createClient();

export default function App() {
  const [tasks, setTasks] = useState<any[]>([]);
  const [title, setTitle] = useState("");

  const refresh = async () => setTasks(await api.listTasks());
  useEffect(() => { refresh(); }, []);

  const add = async () => {
    await api.createTasks({ title, done: false });
    setTitle("");
    refresh();
  };

  return (
    <div>
      <input value={title} onChange={(e) => setTitle(e.target.value)} />
      <button onClick={add}>Add</button>
      <ul>
        {tasks.map((t) => (
          <li key={t.id}>
            <input
              type="checkbox"
              checked={t.done}
              onChange={() => api.updateTasks(t.id, { title: t.title, done: !t.done }).then(refresh)}
            />
            {t.title}
            <button onClick={() => api.deleteTasks(t.id).then(refresh)}>✕</button>
          </li>
        ))}
      </ul>
    </div>
  );
}
```

Method names come from the table: `listTasks`, `createTasks`, `updateTasks(id, …)`,
`deleteTasks(id)`. If the table's access requires a signed-in role, call
`await api.login({ email, password })` first — it sets a session cookie — and `api.whoami()`
returns the current user.

The `useEffect` + `useState` + `refresh()` shape above is exactly what the React framework's
generated hooks replace, and it is a fair picture of the difference between the two frameworks:
this one hands you a typed client and stays out of the way.

## Step 6 — Open the app

Rebuild after changes (**Build** again — no server restart), then visit
`http://todo.localhost:3000`.

## Notes

- **The client is regenerated on every build** and reflects the tables the *application*
  declares, not every table in the database. Adding a table to the app changes the client at the
  next build.
- **Content-Security-Policy**: a `code` app defaults to `default-src 'self'`. If your bundler
  emits inline scripts or styles, or your app loads anything cross-origin, edit the policy on the
  application — the strict default is deliberate, and loosening it is a decision worth making
  explicitly.
- **The output directory must exist after the build and must not be empty**, otherwise the build
  is reported as failed even if your command exited 0 — a bundle that produced nothing is not a
  deployable app.
- **An object-store-backed file store cannot host a build.** The bundler is a process handed a
  working directory, so the store needs a local path.
