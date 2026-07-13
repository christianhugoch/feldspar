# Saltcorn v2

A ground-up rewrite of Saltcorn in Rust. This repository currently implements the
**MVP milestone**: a single Postgres database, a create-first-user / login flow,
a table and field editor, a row editor, and user management — all driven from a
**React + TypeScript admin SPA** served by the `saltcorn` process over a typed
JSON API.

This README is for **operators**: how to install the prerequisites, set up the
database, build the server, and run it. (Design and planning docs live under
[`docs/`](docs/); the roadmap is in [`TODO.md`](TODO.md).)

---

## 1. What you get

- A single binary, `saltcorn`, that serves the admin UI and its API.
- One Postgres database is the source of truth. The server **introspects** whatever
  tables already exist and, on first start, creates a `users` table if none is
  present. It does **not** create the database itself — you do that once (below).
- An admin logs in through the SPA, creates tables and fields, edits rows, and
  manages users.

The MVP is deliberately scoped: one database, basic column types only, no
workflows/agents/models, no file stores or applications yet. See the "Out of MVP
scope" section of [`TODO.md`](TODO.md).

---

## 2. Prerequisites

| Component | Version | Needed for |
|---|---|---|
| **Rust** (with `cargo`) | 1.85+ (edition 2024) | building the `saltcorn` binary |
| **PostgreSQL** | 13 or newer (16 recommended) | the primary data store |
| **Node.js + npm** | Node 18+ | *only* to build the admin UI bundle (optional; see §5) |

Install Rust via [rustup](https://rustup.rs/):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustc --version   # must be >= 1.85
```

Node is required **only** if you want the admin web UI. Without it the server
still runs and its JSON API works, but the browser UI will be a blank bootstrap
page (see §5 and §9 Troubleshooting).

---

## 3. Get the code and do a first build

```bash
git clone <this-repo-url> saltcorn
cd saltcorn

# Compile the server binary (without the web UI bundle for now — see §5).
cargo build --release -p sc-cli
```

The binary is produced at `target/release/saltcorn` (or `target/debug/saltcorn`
for a plain `cargo build`). You can also run it through cargo with
`cargo run --release -p sc-cli -- <args>`.

---

## 4. Database setup

Saltcorn needs one Postgres database and a role that **owns** it (so it can create
the `users` table and any tables you add from the admin UI).

### Option A — local PostgreSQL

Assuming a local Postgres you can administer (as the `postgres` superuser):

```bash
# Create a dedicated role and database. Choose your own password.
sudo -u postgres psql <<'SQL'
CREATE ROLE saltcorn WITH LOGIN PASSWORD 'change-me';
CREATE DATABASE saltcorn OWNER saltcorn;
SQL
```

Verify you can connect as that role:

```bash
psql "postgres://saltcorn:change-me@localhost:5432/saltcorn" -c '\conninfo'
```

### Option B — Docker

```bash
docker run --name saltcorn-db \
  -e POSTGRES_USER=saltcorn \
  -e POSTGRES_PASSWORD=change-me \
  -e POSTGRES_DB=saltcorn \
  -p 5432:5432 \
  -d postgres:16
```

That gives you `postgres://saltcorn:change-me@localhost:5432/saltcorn`.

### Notes

- **The server never creates or drops the database.** Create it once as above.
- The connecting role must be able to `CREATE TABLE` in the database (owning it is
  the simplest way). On startup the server creates a `users` table if one is not
  already present.
- Any tables that already exist in the database are picked up automatically — no
  import or registration step.

---

## 5. Building the admin web UI (optional but recommended)

The admin SPA lives in [`ui/admin`](ui/admin) and is compiled to a static bundle
that the server serves. Building it needs Node and is **opt-in** so the Rust-only
build path never requires a JS toolchain. There are two ways to serve it.

### Option A — build the bundle and point the server at it (recommended)

Build the SPA, then pass its output directory to `saltcorn serve` with
`--static-dir` at run time:

```bash
cd ui/admin
npm ci
npm run build          # outputs ui/admin/dist (main.js + main.css)
cd ../..

target/release/saltcorn serve --static-dir ui/admin/dist ...   # see §6
```

This is the most predictable path: what you serve is exactly the `dist` you point
at, and rebuilding the UI does not require rebuilding the Rust binary.

### Option B — bake the bundle into the binary

> **Important: `SC_BUILD_ADMIN` is a _build-time_ variable, read by `cargo build`
> — not by `saltcorn serve`.** Putting it on the run command (e.g.
> `SC_BUILD_ADMIN=1 saltcorn serve ...`) has **no effect**; the binary was already
> built without the bundle, and the browser will get a blank page (see §9).

Set `SC_BUILD_ADMIN=1` on the **build**. The build script runs `npm ci && npm run
build` in `ui/admin`, embeds the resulting path in the binary, and then
`saltcorn serve` serves the UI automatically with no `--static-dir` needed:

```bash
SC_BUILD_ADMIN=1 cargo build --release -p sc-cli   # runs the UI build, embeds it
target/release/saltcorn serve ...                   # no --static-dir needed
```

### If you skip both

The JSON API still works, but the browser shows only an empty bootstrap document
(its `/main.js` and `/main.css` requests fall through to the fallback HTML). See
§9 Troubleshooting.

---

## 6. Running the server

The one command is `saltcorn serve`. It takes **database flags** and **server
flags**; database settings may also come from environment variables.

### Database connection

Provide **either** a full connection URL **or** the individual parts. A URL, when
present, wins.

| Flag | Environment fallback | Default |
|---|---|---|
| `--database-url <url>` | `DATABASE_URL` | — |
| `--db-host <host>` | `PGHOST` | `localhost` |
| `--db-port <port>` | `PGPORT` | `5432` |
| `--db-user <user>` | `PGUSER` | (libpq default) |
| `--db-password <pw>` | `PGPASSWORD` | — |
| `--db-name <name>` | `PGDATABASE` | (libpq default) |

### Server options

| Flag | Meaning | Default |
|---|---|---|
| `--bind <addr>` | address:port to listen on | `127.0.0.1:3000` |
| `--static-dir <dir>` | directory holding the built admin bundle | (embedded bundle if built with `SC_BUILD_ADMIN=1`, else none) |
| `--session-ttl-hours <n>` | session lifetime | `24` |
| `--secure-cookies` | set the `Secure` attribute on session/CSRF cookies (use behind HTTPS) | off |

Unknown flags in either group are rejected with a clear error rather than ignored.

### Examples

Local development, connecting with a URL and serving the pre-built bundle
(Option A from §5 — the simplest path):

```bash
target/release/saltcorn serve \
  --database-url postgres://saltcorn:change-me@localhost:5432/saltcorn \
  --static-dir ui/admin/dist \
  --bind 127.0.0.1:3000
```

Individual DB parts, listening on all interfaces:

```bash
target/release/saltcorn serve \
  --db-host localhost --db-user saltcorn --db-password change-me --db-name saltcorn \
  --static-dir ui/admin/dist \
  --bind 0.0.0.0:8080
```

With the bundle baked into the binary (Option B from §5 — note `SC_BUILD_ADMIN`
is on the **build**, and `--static-dir` is then unnecessary):

```bash
SC_BUILD_ADMIN=1 cargo build --release -p sc-cli
target/release/saltcorn serve \
  --database-url postgres://saltcorn:change-me@localhost:5432/saltcorn \
  --bind 127.0.0.1:3000
```

Using environment variables (handy for systemd/containers), behind a TLS proxy:

```bash
export DATABASE_URL=postgres://saltcorn:change-me@localhost:5432/saltcorn
target/release/saltcorn serve --bind 127.0.0.1:3000 --secure-cookies
```

On start the server prints the address it is listening on. If the database is
unreachable or misconfigured it exits immediately with an error naming the target
(the password is never printed) — it will not boot in a half-working state.

Stop the server with Ctrl-C; it also handles `SIGTERM` (what an orchestrator
sends) and shuts down gracefully.

---

## 7. First run: create the admin user

1. Start the server (§6).
2. Open the admin URL in a browser, e.g. `http://127.0.0.1:3000`.
3. The SPA detects that no user exists yet and shows a **create-first-user**
   screen. Enter an email and password; that first user is created as **role 1
   (admin)** and logged straight in.
4. From there you can create tables and fields, edit rows, and add more users.

If you did not build the web UI, you can drive the same flow over the API:

```bash
# Is there a user yet?
curl -s http://127.0.0.1:3000/api/auth/status

# Create the first admin (only works while no user exists).
curl -s -X POST http://127.0.0.1:3000/api/first-user \
  -H 'content-type: application/json' \
  -d '{"email":"admin@example.com","password":"a-strong-password"}'
```

---

## 8. Health check

The server exposes an unauthenticated liveness route for load balancers,
orchestrators, and smoke tests:

```bash
curl -s http://127.0.0.1:3000/health
# {"status":"ok"}
```

A `200` here means the process booted and is accepting requests.

---

## 9. Troubleshooting

- **`error: connecting to database ...` on startup.** The database is unreachable
  or the credentials/host/port/name are wrong. The message names the target
  (without the password). Confirm you can `psql` to the same URL, and that the
  database exists (§4) — the server does not create it.
- **Startup error mentioning the `users` table or permissions.** The connecting
  role cannot create tables. Make it the owner of the database (§4).
- **Blank page; console shows `main.css`/`main.js` "MIME type ('text/html')"
  errors.** The server is serving the fallback HTML document for `/main.js` and
  `/main.css` because it has no admin bundle to serve. This is almost always
  because `SC_BUILD_ADMIN=1` was put on the **run** command instead of the build
  (it is build-time only — see §5). Fix it either way:
  - quickest: restart with `--static-dir ui/admin/dist` (after `npm run build` in
    `ui/admin`), or
  - rebuild the binary with `SC_BUILD_ADMIN=1 cargo build --release -p sc-cli`,
    then run without `--static-dir`.
  Confirm with `curl -i http://localhost:3000/main.js` — a working setup returns
  `content-type: text/javascript`, not `text/html`.
- **Login/session doesn't stick behind HTTPS.** Add `--secure-cookies` so the
  cookies are sent over TLS. Conversely, do **not** use `--secure-cookies` for
  plain-HTTP local development, or the browser will drop the cookies.

---

## 10. For developers

Run the workspace checks and tests:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets
cargo test --workspace
```

Integration tests run against a **real Postgres**, reinitialised per test. Point
them at a database with `DATABASE_URL` (the same variable the server uses); CI uses
`postgres://saltcorn:saltcorn@localhost:5432/saltcorn_test` against a
`postgres:16` service. See [`docs/TECHNICAL_DESIGN.md`](docs/TECHNICAL_DESIGN.md)
§16 for the testing approach.
