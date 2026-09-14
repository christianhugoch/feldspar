#!/usr/bin/env bash
# Record what a real Saltcorn 1 passes to `builder.renderBuilder` for the
# BooksDB views and page (TODO "The builder" 5.6).
#
#   V1_CHECKOUT=~/saltcorn crates/sc-server/tests/fixtures/record-builder-options.sh
#
# It restores `saltcorn-v1-BooksDB.zip` into a Postgres database of its own,
# signs in to v1's own server as an admin, opens the builder the way an admin
# does — `/viewedit/config/<view>` for Show Books, Edit Books, List Books and
# Filter books, `/pageedit/edit/BooksOverview` for the page — and writes the
# options object out of each document's `renderBuilder` call into
# `builder-options/`. Nothing is computed here: the files are v1's answer.
#
# The checkout must be built (`npm run tsc` or v1's own build), and must be at
# the commit `ui/saltcorn-ui/vendor/` was taken from, or differ from it nowhere
# the options come from; the script refuses otherwise.
#
# Settings, all optional:
#   RECORD_DATABASE  the database to restore into (default saltcorn_v1_builder_record);
#                    created if missing, and RESET — never point it at a real one
#   RECORD_TEMPLATE  the template `createdb` copies (for a server whose template1
#                    has a collation mismatch)
#   PGHOST, PGUSER   how to reach Postgres (default the local socket, $USER)
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../../.." && pwd)"
v1="${V1_CHECKOUT:?set V1_CHECKOUT to a built saltcorn/saltcorn checkout}"
v1="$(cd "$v1" && pwd)"
database="${RECORD_DATABASE:-saltcorn_v1_builder_record}"
out="$here/builder-options"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# --- the checkout is the vendored one, where it matters ----------------------
vendored="$(sed -n 's/.*saltcorn\/saltcorn `\([0-9a-f]*\)`.*/\1/p' "$repo/ui/saltcorn-ui/vendor/README.md" | head -1)"
if [ -z "$vendored" ]; then
  echo "cannot read the vendored commit from ui/saltcorn-ui/vendor/README.md" >&2
  exit 1
fi
head_commit="$(git -C "$v1" rev-parse HEAD)"
if ! git -C "$v1" diff --quiet "$vendored" HEAD -- \
  packages/saltcorn-data packages/saltcorn-markup packages/saltcorn-builder \
  packages/server/routes/pageedit.ts packages/server/routes/viewedit.ts \
  packages/server/routes/utils.js; then
  echo "the checkout ($head_commit) differs from the vendored commit $vendored where the builder's options come from" >&2
  exit 1
fi

# --- every setting v1 reads, set; and v1 asked where it is before a reset ----
#
# v1 takes an unset variable from ~/.config/.saltcorn, so each is set here. An
# empty PGPASSWORD is not "no password": v1's connect.ts uses Postgres only when
# user, password and database are all truthy, and otherwise falls back to a
# SQLite file in ~/.local/share/saltcorn. The first attempt at this recording
# did exactly that. Peer authentication over the socket ignores the value.
export PGHOST="${PGHOST:-/var/run/postgresql}"
export PGUSER="${PGUSER:-$USER}"
export PGPASSWORD="${PGPASSWORD:-unused-by-peer-authentication}"
export PGDATABASE="$database"
export SALTCORN_MULTI_TENANT=false
export SALTCORN_FILE_STORE="$work/files"
export SALTCORN_SESSION_SECRET=record-builder-options
export SALTCORN_JWT_SECRET=record-builder-options
unset DATABASE_URL FORCE_SQLITE SQLITE_FILEPATH NO_DB_CONNECTION
mkdir -p "$SALTCORN_FILE_STORE"

if ! psql -d "$database" -Atc "select 1" >/dev/null 2>&1; then
  createdb ${RECORD_TEMPLATE:+-T "$RECORD_TEMPLATE"} "$database"
fi

reached="$(cd "$v1/packages/server" && node --input-type=module -e '
import db from "@saltcorn/data/db/index";
const { rows } = await db.query("select current_database() as d");
console.log(db.isSQLite ? "sqlite" : rows[0].d);
await db.close();
' | tail -1)"
if [ "$reached" != "$database" ]; then
  echo "v1 connects to \`$reached\`, not \`$database\`; refusing to reset it" >&2
  exit 1
fi

saltcorn="$v1/packages/saltcorn-cli/bin/saltcorn"
"$saltcorn" reset-schema -f
"$saltcorn" restore "$here/saltcorn-v1-BooksDB.zip"
"$saltcorn" create-user -a -e recorder@example.com -p Record-builder-options-1

# --- v1's own server, as an admin opening the builder -------------------------
mkdir -p "$out"
(cd "$v1/packages/server" && OUT="$out" V1="$v1" node --input-type=module) <<'EOF'
import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

const { default: getApp } = await import(pathToFileURL(`${process.env.V1}/packages/server/dist/app.js`));
const app = await getApp({ disableCsrf: true });
const server = http.createServer(app);
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const base = `http://127.0.0.1:${server.address().port}`;

const login = await fetch(`${base}/auth/login`, {
  method: "POST",
  redirect: "manual",
  headers: { "content-type": "application/x-www-form-urlencoded" },
  body: new URLSearchParams({ email: "recorder@example.com", password: "Record-builder-options-1" }),
});
const session = login.headers.getSetCookie().find((c) => c.startsWith("connect.sid="));
if (!session) throw new Error(`signing in answered ${login.status} and no session`);
const cookie = session.split(";")[0];

// The document's `builder.renderBuilder("saltcorn-builder", "<options>",
// "<layout>", "<mode>")`, as saltcorn-markup/builder.ts writes it.
const CALL = /builder\.renderBuilder\(\s*"saltcorn-builder",\s*"([^"]*)",\s*"([^"]*)",\s*("[^"]*")\s*\)/;

const record = async (file, url) => {
  const res = await fetch(base + url, { headers: { cookie }, redirect: "manual" });
  const html = await res.text();
  const call = html.match(CALL);
  if (!call) throw new Error(`${url} answered ${res.status} with no builder in it: ${html.slice(0, 300)}`);
  const options = JSON.parse(decodeURIComponent(call[1]));
  // The session's token, which `addCsrf` puts on the options: not v1's data.
  delete options.csrfToken;
  fs.writeFileSync(path.join(process.env.OUT, file), JSON.stringify(options, null, 2) + "\n");
  console.log(`recorded ${file} from ${url} (mode ${JSON.parse(call[3])})`);
};

await record("show-books.json", "/viewedit/config/Show%20Books");
await record("edit-books.json", "/viewedit/config/Edit%20Books");
await record("list-books.json", "/viewedit/config/List%20Books");
await record("filter-books.json", "/viewedit/config/Filter%20books");
await record("page-booksoverview.json", "/pageedit/edit/BooksOverview");
server.close();
process.exit(0);
EOF
