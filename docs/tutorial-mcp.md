# Tutorial: Let an external coding agent administer the installation

An application built here is **two halves**. One is code in a git repository: pages, routes,
styling, the calls the browser makes. The other is configuration in the server's database:
the tables and their fields, who may read and write them, the triggers over them, the
workflows, the agents, the applications themselves. A coding agent working in your repository
has the first half through the filesystem and, by default, no access at all to the second — so
asked to "add a `priority` field to `tasks` and escalate a task when it goes high", it can
write every line of the front end and do neither of the two things that make the feature work.

The **administration MCP server** is how it reaches the second half. It is served by the same
process, over one route, and it is reached with a bearer token you mint and can revoke.

By the end of this you will have turned it on, minted a token, run one `claude mcp add` line,
and watched a Claude Code session — which has no other access to your installation — add a
field, save a trigger and rebuild the application whose generated client that field just
changed.

This continues from [tutorial-react-todo.md](tutorial-react-todo.md) and
[tutorial-triggers.md](tutorial-triggers.md): a server started with `--base-domain localhost`,
a `tasks` table, and a React `todo` app served at `http://todo.localhost:3032` whose source is
in the `apps` file store. Any table and any app will do. You will also need
[Claude Code](https://claude.com/claude-code), or another MCP client that speaks streamable
HTTP.

## Step 1 — Turn it on

Go to **Settings → Development**. Two checkboxes near the bottom:

| Setting | Default | What it does |
|---|---|---|
| **Administration MCP server** | off | Serves `POST /mcp` at all |
| **MCP from this machine only** | on | Refuses a request whose peer is not on this host |

Tick the first. Leave the second ticked — the agent below runs on the same machine as the
server, which is the usual arrangement; untick it only when it genuinely does not, and read the
help text about proxies before you do.

Save. Both take effect on the next request: **no restart**, which is the point of them being
settings rather than command-line flags — the moment you want the server on is the moment the
server is already running.

While the first box is unticked, `POST /mcp` answers `404` and **never reads the token table**.
A disabled feature should not be distinguishable from an absent one, and it should not be a
code path that touches credentials.

## Step 2 — Mint a token

The panel directly below the switches is the credentials. Fill it in:

| Field | Value |
|---|---|
| Label | `claude-code on my laptop` |
| Expires in (days) | `90` |

Then the six checkboxes. These are **the copilot's own grants** — the same six an
`admin_copilot` agent carries (see [tutorial-agents.md](tutorial-agents.md)), fetched from the
trait itself rather than written out a second time in the screen. One vocabulary for *what may
this agent do to my installation*, whether the agent is the built-in chat copilot or an
external one over MCP. Tick:

- **May create tables, fields, triggers and custom SQL queries** ✔
- **May change existing tables, fields, triggers and custom SQL queries** ✔
- **May drop tables and fields, and delete triggers and custom SQL queries** ✘ — deliberately,
  and Step 6 is what that looks like
- **May change access rules** ✘ — role floors, ownership formulae and row-level security change
  what *every* user of the deployment can reach
- **May work on triggers** ✔
- **May work on applications' custom SQL queries** ✔

Press **Mint**. The token appears **once**:

```
fspk_qN8t2v…
```

and under it, the line that registers it, with the token already in it. Copy that line now. The
server stores a SHA-256 hash of the token and nothing else — there is no screen, anywhere, that
can show it to you again. If you lose it, revoke the row and mint another; that is cheaper than
it sounds.

The four leading characters are not decoration: `fspk_` is what makes the credential
recognisable in a paste and greppable by a secret scanner.

## Step 3 — One line

In the project directory of your application's repository:

```bash
claude mcp add --transport http feldspar http://localhost:3032/mcp \
  --header "Authorization: Bearer fspk_qN8t2v…"
```

That is the whole of the setup. Start `claude` in that directory and type `/mcp`; `feldspar`
should be listed as connected, offering about twenty-five tools.

Two things about that line are decisions rather than defaults:

- **`--transport http`.** The server speaks streamable HTTP on one route and opens no
  server-initiated stream: every tool here is request/response, so an SSE channel would be a
  connection held open for no traffic. There is no stdio transport, because stdio would need a
  second way to reach the running server's catalog, mounts and triggers — which is the thing
  HTTP already is.
- **The header, not a browser login.** `Authorization: Bearer` is the *only* credential this
  route accepts. A session cookie on it is **ignored, not honoured**, and that is the whole
  confused-deputy story rather than belt-and-braces: a page you visit cannot set an
  `Authorization` header cross-origin without a preflight this server will not answer, so no
  site you happen to browse can reach the administrative surface through the admin session you
  happen to be signed into.

## Step 4 — What it can see

Ask it something that needs the second half:

> What tables does this Saltcorn installation have, and which of them does the `todo` app
> serve?

It will call `describe_schema` and `describe_applications`. The first answers with every table,
its fields and their types, its access rules and the foreign keys in both directions — **and no
rows and no row counts**: this surface describes the shape of a database, never its contents.
There is no `list_rows` tool and there will not be one; administering a schema and reading the
data in it are different capabilities, and only the first is what building an application needs.

If your project was scaffolded by Saltcorn, there is also a generated `SKILL.md` beside the
generated client (`src/feldspar/SKILL.md` in a React app) that says all of this to the agent
without you typing it: which half of the application is in the repository, which half is in the
server, the tools that reach the second half, and the ones that deliberately do not exist. Copy
it to `.claude/skills/<name>/SKILL.md` to have it loaded automatically.

## Step 5 — A field, a trigger, and a rebuild

Now the thing the milestone exists for. Ask for all three at once:

> Add a `priority` integer field to `tasks`, defaulting to 3. Then add a trigger that writes an
> audit row whenever a task's priority is raised above 7, and rebuild the todo app.

Watch what it does.

**`edit_schema`, once.** Not one call per field: the tool takes an *ordered list* of operations
applied as **one transaction**, because a schema is a set of connected tables and a
per-operation tool turns a twelve-table domain into forty round trips. A foreign key in the list
may point at a table created earlier in the same list. If any operation is refused, none of them
happened.

**The result names what moved.** A schema change re-projects every mounted application that
serves an affected table and rewrites its generated TypeScript client on disk — at once, with no
restart. What it does *not* do is run a bundler, so the result says which applications have a
build step and are therefore serving a bundle compiled against the old schema:

```json
{
  "applied": true,
  "fields_added": 1,
  "applications": [{ "id": "…", "subdomain": "todo", "wants_build": true }],
  "notes": ["Re-projected, with the generated client rewritten: `todo`. No bundler was run … so call `buildApplication` with the `id` above to rebuild each against the schema this batch left."]
}
```

A list is what happened; an instruction is what a model acts on, so the result carries both.

**`describe_action` before `save_trigger`.** No fixed schema can carry every action's settings —
`send_email`'s depend on which file fields the table has — so the agent asks for the settings of
the action it wants and then configures it by the names it was given. Guessing gets a refusal
that names the setting and lists the real ones, which costs a turn.

**`buildApplication`.** It regenerates the client from the new schema, runs `npm run build`, and
serves the result on the subdomain with no restart. If the project does not compile, that comes
back as a **result** — `built: false` with the tools' output and the diagnostics parsed out of
it — not as a refusal: a model told only "the build failed" cannot fix anything.

Reload `http://todo.localhost:3032` and open `src/feldspar/client.ts`: `priority` is on
`TasksRow`, and it got there without anyone regenerating anything by hand.

## Step 6 — What it refuses, and how that reads

Ask for something the token was not granted, in the middle of something it was:

> Add a `due` date field to `tasks`, and drop the `task_audit` table.

The whole batch is refused and **nothing is applied** — not the field either:

```
operation 1 (drop_table on `task_audit`): not permitted to drop a table; the whole
batch was refused and nothing was applied. Turn on `allow_drop` to allow it.
```

Three properties of that sentence are deliberate. It names the operation **by index as well as
by name**, which is what a twelve-operation batch needs. It says what was *not* done, so the
agent does not report a half-applied change. And it names the checkbox that would allow it, so
you get "the token isn't granted drop — do you want to mint one that is, or should I leave the
table?" instead of a retry loop against "Forbidden".

The same shape applies at every level. A tool refuses in words the model can relay; a *tool*
failure is a result the model reads, and a JSON-RPC error is reserved for what is wrong with the
**call** — an unknown method, an unknown tool, a protocol revision this server does not speak, a
credential it will not accept.

An area that is off behaves differently again: `allow_triggers` unticked does not refuse the
trigger tools, it **removes them from the listing**. A tool a model can see is a tool it will
try, and a turn spent discovering what a caller is not for is a turn somebody pays for.

Two things it will not offer whatever you tick: **your source files** — you have the repository
and an editor, and a second way to write those files would be a second thing to keep in step —
and **user management, backup and restore**, which are administrative acts that building an
application does not require.

## Step 7 — Watching it work

A credential that lives ninety days is defensible when its use is visible in the log stream you
are already watching, and indefensible when it is not. Every tool call writes one line at
`info`:

```
MCP [claude-code on my laptop] describe_schema ok in 7ms
MCP [claude-code on my laptop] edit_schema ok in 41ms
MCP [claude-code on my laptop] edit_schema refused in 3ms: operation 1 (drop_table on `task_audit`): not permitted to drop a table…
```

The **label** is what appears — never the token, never its hash. Set **Log verbosity** to
`verbose` in the same Settings → Development section and the arguments are logged too, which is
the same rung an agent's tool-call arguments already sit on.

## Step 8 — Taking it back

In the token list, press **Revoke** on the row. The row stays, marked — a revocation is a thing
that happened, and this list is where it is seen to have happened — and the next call the agent
makes fails. The same is true if the token expires, if the user who minted it is deleted, or if
they stop being an administrator: the credential names a **user**, and it is checked against that
user on every call.

---

## The two sentences to read before you mint one

**This token is an administrator.** It runs with the full authority of the admin who minted it,
bounded only by the six boxes that were ticked. It is not a service account and not a restricted
role: every call it makes is authorized exactly as that person's own admin session would be,
which is precisely why there is one authorization model here and not two.

**Revoking it is the only way to take it back.** There is no session to expire, no browser to
close, and no way to see the plaintext again to check what you handed out. If a token might have
leaked — a shell history, a screen share, a pasted config — revoke the row and mint another.
That costs one line of setup; the alternative costs an installation.
