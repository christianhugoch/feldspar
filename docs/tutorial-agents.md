# Tutorial: Agents — a model that can use your application

Connect an LLM, give it your `tasks` table, and ask it questions about the data. Give it a
trigger, and it can *do* things. Point a second agent at your app's source, and it can grep the
project, edit a file, build it, read the error it caused and fix it. Then wire one to a trigger,
so a row insert starts an agent with a prompt built from the row.

This continues from [tutorial-triggers.md](tutorial-triggers.md): you have a server started with
`--base-domain localhost`, a `tasks` table (`id`, `title`, `done`, `owner`), a React `todo` app
whose source is in a file store called `apps`, a **Member (40)** role, and the `archive_done`
trigger from that tutorial. Any table, app and trigger will do — the names below just assume
those.

You will need an API key from **Anthropic** or from anything speaking the **OpenAI Responses**
API. Everything here costs tokens: an agent is a model, and every turn is a request you pay for.

## The rule in one line

An **agent** is a configured loop:

> a **provider and model**, a **system prompt**, and a set of enabled **traits** — where each
> trait is one deliberate grant, configured with the thing it may reach.

One turn of the loop is: send the conversation and the tools; the model answers with text, or
with **tool calls**; each tool runs, in the order the model asked; its result goes back into the
conversation; round again — until the model stops asking, or the step budget runs out.

Two things follow from that shape and are worth holding on to before you start:

- **A tool sees only what its caller may see.** Every tool runs as *you*, the person chatting,
  so ownership and row-level security apply inside an agent exactly as they do outside it.
- **The agent's abilities are its traits, and nothing else.** There is no trait that reaches
  *any* table; each one names its table, its trigger, its directory. "What may this agent
  touch?" is answerable by reading its definition.

## Step 1 — Connect a provider

**Agents → LLM providers → New LLM provider**:

| Field | Value |
|---|---|
| Name | `house` |
| Backend | `anthropic` (or `openai_responses`) |
| API key | your key |
| Default model | `claude-sonnet-5` (or `gpt-5.1`) |
| Base URL | leave alone |

Press **Test connection** before Save. It sends one short prompt and shows you either the
model's answer or **the provider's own error text** — a rejected key says `401` here, in front of
you, instead of arriving as a mysterious failure inside a chat transcript later.

Then **Create provider**.

**Base URL is why there are only two backends.** `openai_responses` speaks the Responses API to
whatever URL you give it, so a gateway, a local server or another vendor that speaks it is a
field value rather than a code change.

Re-open the provider you just saved and look at the key: it reads `••••••••`. That is a
**sentinel**, not a truncation, and saving the form with it unchanged keeps the stored key. See
"trips people up" below for the one thing that can go wrong with that.

## Step 2 — An agent that can read your data

**Agents → New agent**:

| Field | Value |
|---|---|
| Name | `helper` |
| Minimum role | leave blank (admin only) |
| LLM provider | `house` |
| Model | blank — the provider's default |
| System prompt | `You help an admin understand their to-do list. Answer from the data, not from memory.` |

Then, in the **Traits** card, choose `query_table` and press **Add trait**. Its form appears —
rendered from the trait's own declaration, which is why a trait a plugin adds gets a working form
with no change to the admin UI:

| Trait setting | Value |
|---|---|
| Table | `tasks` |
| Fields | leave blank (all of them) |
| Maximum rows per call | `50` |

**Create agent**.

The tool the model is offered is called `query_tasks` — the name is derived from the
configuration, not fixed by the trait, which is what lets you add `query_table` twice for two
tables and have the model tell them apart. Two traits that would produce the same tool name are
refused when you save, where you can fix it, rather than discovered when the model picks the
wrong one.

The tool's **description and JSON schema are generated from your table's own fields**, so the
model is told that `done` is a boolean and `owner` is text rather than left to guess.

## Step 3 — Chat with it

Press **Chat** on the agent's row.

Ask: *how many tasks are not done, and who owns them?*

What you see, in order:

1. the model's text **streaming in**, a token at a time;
2. a **tool call** entry — collapsed, named `query_tasks`; open it and the arguments the model
   sent (`{"where": {"done": false}}`) and the rows that came back are both there;
3. the answer, written from those rows.

That is the loop, rendered. Ask a follow-up and the conversation continues — the whole history
goes to the model each turn, which is why a long chat eventually gets expensive.

Two buttons worth pressing once each:

- **Stop**, mid-answer. The provider stream is closed and the run is left `aborted`, with
  everything it had already said still in the transcript.
- **History**, in the card on the right. Reload the page and the conversation is still there:
  every step of every run is written to `_fd_runs` as it happens, so a chat panel that was closed
  mid-answer reopens on what actually happened. An old run opens read-only; **Continue** is the
  deliberate act that reconnects it.

### If the agent will not answer

Open **Agents**. A red badge means the agent is stored but **not usable**, with the reason on it
— a provider that was deleted, a trait naming a table that is gone. It stays listed and editable;
editing it is the repair. That check runs on save *and* on load, so an agent whose world changed
underneath it says so instead of failing mid-conversation.

## Step 4 — Let it do something

Reading is one grant; acting is another, and they are deliberately separate. Edit `helper` and
add a second trait:

| Trait | Setting | Value |
|---|---|---|
| `run_trigger` | Trigger | `archive_done` |

Save, chat again, and ask: *archive my finished tasks*. The transcript shows a `run_archive_done`
tool call, and the trigger runs — the **dispatcher's** trigger, the same one the app's API and
the Run button call, with its `only_if`, its cascade bound and its action.

**The trigger's own `min_role` still gates it.** Exposing an agent to a role does not thereby
expose everything that agent can reach: a Member chatting with an agent that has `run_trigger`
over an admin-only trigger is refused *in the tool result*, by name. Set the agent's **Minimum
role** to `40` and try it as a member to see the difference between the two floors.

There are three more table grants beside `query_table`, each its own trait so that a read-only
agent is the default shape: `insert_row`, `update_rows` and `delete_rows`. The last two
**require a `where`** — a table rewritten by an omitted argument is the same accident as one
emptied by it — and both refuse a call that matches more rows than the trait's maximum rather
than truncating it.

## Step 5 — An agent that changes your app's code

**You may already have one.** Creating an application creates the agent that builds it: if you made
the to-do app with a provider already connected, there is an agent called `build-todo` in the list,
scoped to that app's source and able to build it. Which agent an application gets is its
*framework's* declaration, so a React app's is a coding agent over the project directory the
framework derived. Open it and look at the two traits below — it is an ordinary agent, and yours to
edit, re-point or delete.

Build it by hand anyway, once, because the settings are worth understanding. Make a second agent,
`coder`, over the same provider, with this system prompt:

```
You maintain a React + TypeScript app. Read before you edit, build after you edit, and fix what
you broke.
```

Add the `coding` trait. One trait, one form: **where** the agent works, and **what it may do**
there.

| Setting | Value |
|---|---|
| File store | `apps` |
| Sub-directory | `todo` |
| May create and change files | ✔ |
| May run the project's `package.json` scripts | ✔ |

Reading, finding and searching come with the trait. The checkboxes are the grants: leave them
all off and you have an agent that can explain your code and nothing else, which is a thing you
may deliberately want. Tick the first and it gains `write_file` and an edit tool (`edit_file`, or
`apply_patch` for OpenAI models); tick the second and it gains `run_script`. A third, **May format
and type-check after edits**, runs the project's prettier and its `typecheck` script after each
turn that edited something, and shows the model which errors are new.

…and add one more trait, which names the application instead, because which store the source is in
is the *application's* own configuration:

| Trait | Setting | Value |
|---|---|---|
| `build_application` | Application (subdomain) | `todo` |

Save, chat, and ask: *add a "notes" column to the Tasks page*.

Watch the transcript. It will `search_files` for where the row type is used, `read_file` the page,
`edit_file` it, and `build_todo` — and if it guessed a property that is not on `TasksRow`, the
build comes back **not** as "build failed" but as the diagnostics themselves, file and line and
message, which is the most useful thing a model can be told. It reads its own type error and
fixes it.

Five things about that set:

- **`edit_file` must find exactly one place.** It forgives a lost trailing space, the wrong
  indentation or one mistyped character, in that order, but a match that is absent, or that
  appears twice, is an error the model reads and retries, showing it the closest lines or every
  place it matched. `replace_all` is the explicit opt-in for the rename-through-a-file case. And a
  file must be read before it is edited, so the model never edits a file from memory.
- **The sub-directory is a confinement**, not a convenience. A path that escapes it is refused by
  any spelling, and every path the model is shown is relative to it.
- **A tool it was not granted is a tool it never sees.** With **May create and change files**
  unticked, `write_file` and the edit tool are not declared to the model at all, so it plans around
  reading rather than trying an edit and being refused.
- **`build_application` builds, it does not publish.** An agent's build answers "does this
  compile?"; mounting what it built is still your **Build** button, which is also where you get to
  look at the diff first.
- **The script grant is not a shell.** It runs `npm run <script>` for a script your
  `package.json` already declares, and refuses anything else by listing the scripts that exist.
  The runnable set is the project's own; the model chooses from it rather than composing a
  command line. It is a **separate** checkbox from the edit grant, because running a script
  executes code the agent did not write. A real shell is the last checkbox on the form, **May use
  a shell**, off by default and offered only when the person chatting is an admin: it runs as the
  server's own user, so it can do everything the other grants can and read the server's
  configuration too. Its **Shell sandbox** setting can run each command in a `docker` or `podman`
  container instead, with only the sub-directory mounted and no network.

## Step 6 — An agent as a trigger body

Everything so far had you in front of it. `run_agent` is the action that runs an agent when
nobody is: **Triggers → New trigger**:

| Field | Value |
|---|---|
| Name | `summarise_task` |
| Event | `A row is inserted` |
| Table | `tasks` |
| Action | `run_agent` |
| Agent | `helper` |
| Prompt | `` `A task was just added: "${row.title}". How many are now outstanding?` `` |

Save, then add a task in the app. Nothing appears anywhere — an action returns a value, it does
not stream — but open **Agents → helper → Chat** and the run is in the **History** card,
described as ``trigger `summarise_task` ``, with the prompt the formula built at the top of it and
the agent's answer at the bottom.

**The prompt is a formula**, in the same language as an `only_if`, a `where` and every field
value: it reads `row`, `old`, `user` and `payload` under the same rule. Write it as a
**template literal** rather than `'…' + row.title`, because arithmetic in this language is
null-guarded — a `+` with a null column computes null, and an agent with nothing to ask is
refused rather than asked.

The action's result is the final assistant message and the **run id**, so a trigger you expose on
your application (**Applications → Todo → Edit**, tick it, **Build**) answers a
`POST /api/actions/summarise_task` with:

```json
{
  "agent": "helper",
  "run": "5f1c…",
  "answer": "Three are outstanding.",
  "conclusion": "answered"
}
```

…guarded by the trigger's own **Minimum role**, like any other exposed trigger. Nothing about
§13.2 changes because the action happens to be an agent.

**A triggered run has no user.** Nobody was there, so it carries the *trigger's* authority — admin
— and the run row records no user, which is exactly what an audit needs to be able to say. Read
that sentence twice before you point a triggered agent at a table with an ownership formula: what
protects a triggered run is the trigger's own `min_role`, not the caller's.

## Step 7 — An agent that builds the app

Every agent so far worked *on* tables you had already made, and ran triggers somebody else set
up. `admin_copilot` makes both.

**Agents → New agent**, call it `architect`, and give it one trait — `admin_copilot` — and leave
its six checkboxes exactly as they arrive:

| Checkbox | Leave it | |
|---|---|---|
| May create tables, fields, triggers and custom SQL queries | **on** | *what it may do* |
| May change existing tables, fields, triggers and custom SQL queries | **on** | |
| May drop tables and fields, and delete triggers and custom SQL queries | off | |
| May change access rules | off | |
| May work on triggers | **on** | *what it may do it to* |
| May work on applications' custom SQL queries | **on** | |

The first four are **grants** and the last two are **areas**, and they are two different
questions. A grant says what this agent may do; an area says which of the three things it may do
it *to* — the schema, the triggers, an application's SQL endpoints. Untick an area and its tools
are not offered at all, which is why an agent you only want near the schema should have both areas
off rather than a stern system prompt.

Notice what the form does *not* ask for: a table. It is the only trait that names none, because
the tables it makes do not exist while you are configuring it. It is scoped by **what it may do**
instead. Save it, open its **Chat**, and ask for a schema:

> Create the database schema for a small law firm: clients, matters, and time entries.

It calls `edit_schema` **once**, with the whole thing — three tables and the keys between them in
one ordered list. Open **Tables** and they are there, each with an `id` primary key you did not
have to ask for and each foreign key drawn to the table it points at. Ask it a question and it
answers from `describe_schema` alone:

> Which tables reference clients?

Now ask it to drop one:

> Drop the time_entries table.

It refuses, because `allow_drop` is off, and it tells you which checkbox would allow it. Tick
**May drop tables and fields, and delete triggers** on the trait, ask again, and it goes. Ask it to drop `clients`
while `matters` still points at it and it refuses again — this time naming `matters.client` as
the thing to remove first, because a foreign-key error out of the database is not something
either of you can act on.

Finally, tick **May change access rules** and ask for the rule itself:

> Make clients readable only by the user who owns it. There is an owner column.

It writes the ownership formula, and if you ask it to enforce that in the database it turns
row-level security on and emits the policies. Two different users now read different rows of the
same table — the same §7.3 mechanism [tutorial-ownership.md](tutorial-ownership.md) covers,
reached by asking for it.

## Step 8 — The same agent, writing a trigger

`architect` has four more tools, over the triggers of
[tutorial-triggers.md](tutorial-triggers.md). Ask it for one in the same breath as the schema:

> When a matter's status becomes closed, email the client to say so.

Watch what it does, because the shape is the point:

1. `describe_triggers` — what is already there, and **which actions exist**, one line each.
2. `describe_action` for `send_email`, naming `matters` as the table. That second argument
   matters: an action's settings can depend on the table it fires on, and this is where the
   "attach the file in this row" checkboxes would come from.
3. `save_trigger`, with the event, the table, an `only_if` (`status === 'closed' && old.status
   !== 'closed'` — the condition that makes it fire on the *transition* rather than on every
   update of a closed matter), and the action's settings as `configuration`.

Step 2 is the interesting one. There is no tool called `configure_send_email`, and there could
not be: actions are an open set and each declares its own settings, so the model **asks for the
settings it needs, when it needs them**, instead of every conversation carrying every action's
form. If it skips the asking and guesses, the save is refused — and the refusal hands it the
settings it should have used, so it usually gets there in the next turn anyway.

Two things to try:

- **Ask it to change one field of an existing trigger.** "Send that email to the fee earner too."
  It sends only what changed; everything you did not mention stays as it was.
- **Ask it to make the trigger runnable by staff.** It refuses: who may run a trigger is an
  *access rule*, so it needs the same **May change access rules** checkbox a role floor does.
  Without it, a trigger the agent creates is admin-only — which is the safe default and the same
  one the trigger form has.

Deleting is behind the drop grant, and it will tell you so — and will usually suggest switching
the trigger off instead, which keeps its configuration.

## Step 8b — The same agent, writing an SQL endpoint

`architect` has three more tools, over the **custom SQL queries** of
[tutorial-rest-queries.md](tutorial-rest-queries.md) — the escape hatch for the report the row
layer's read cannot express. If you have an application with a REST API, ask for one:

> Give the timesheet app an endpoint that returns each fee earner with the hours they billed
> since a date the caller passes in.

It calls `describe_applications` to find the app and its API, then `save_api_query` with the SQL,
a `since` parameter and a path. What comes back is the interesting part: **the columns the
database says the statement returns**, because saving a query *prepares* it. So the answer to "did
that work?" is Postgres's, in the same turn — and a query with a typo in a column name is refused
with Postgres's own message and nothing is stored.

Three things to notice:

- **The app's generated client already has the method.** Re-emitting it is part of the save, so
  `src/feldspar/api.ts` grows a typed `hoursByEarner(...)` before you have looked at it.
- **The endpoint is not answering yet.** A mounted app's API is built from its record when the app
  is mounted, so the agent will tell you it is served from the app's next **Build** — the same as
  when you save one in the application form.
- **Who may call it is an access rule.** With **May change access rules** off, every query the
  agent writes is admin-only, and asking for a public one is refused by name.

## Step 9 — An agent that asks another agent

The `librarian` from Step 2 reads one table. Suppose you now want an agent that talks to people
about the whole library *and* can go and dig through the code when somebody asks why a page looks
wrong. You could give one agent both sets of traits. Don't: you would be handing the agent that
answers the public a way to edit your source, and every file it reads would sit in the
conversation for the rest of the afternoon.

Give it a **sub-agent** instead. Keep `librarian` as it is, and make a second agent —
`front_desk` — whose only trait is `subagent`:

| Field | Value |
|---|---|
| Agent | `librarian` |
| When to use it | *a question needs the library's own data* |
| Step budget per delegation | leave blank — `librarian`'s own |
| Maximum delegation depth | 3 |

`front_desk` now has exactly one tool, `delegate_to_librarian`. Open its **Chat** and ask a
question about the books. It writes a briefing — a task, the context, and what it wants back —
`librarian` runs a conversation of its own to answer it, and `front_desk` replies with what came
back. Two things are worth looking at afterwards.

**Open the run history.** There are *two* runs: yours, and `librarian`'s underneath it, described
as "delegated by `front_desk`". The whole of `librarian`'s working — the query it ran, the rows it
got — is in that second transcript, and none of it is in yours. That is what delegation buys.

**Notice what `librarian` was not told.** It cannot see your conversation; the briefing is the
only thing that crossed. So the "When to use it" sentence matters more than it looks: it is what
`front_desk` reads when deciding whether to delegate at all.

Three refusals are worth provoking once, so you recognise them later:

- Give `librarian` a `subagent` trait pointing back at `front_desk`, and ask for something that
  makes it delegate. It comes back with ``` `front_desk` → `librarian` → `front_desk` ``` — a
  loop, named, rather than a run that goes round for ever.
- Set **Maximum delegation depth** to 1 on a three-agent chain and the second hop is refused with
  the chain in the message. Depth is about cost, not correctness: every level multiplies the
  tokens of the one above it.
- Set `librarian`'s **Max steps per run** to 2 and ask something that needs more. `front_desk`
  is told the sub-agent used its whole budget and did not reach a conclusion — not handed a blank
  answer it would otherwise report to you as "nothing found".

And the rule that has held all the way down this page still holds here: `librarian` runs as
**you**, so it reads the rows you may read, and if its **Minimum role** is stricter than yours,
`front_desk` is refused by name. Delegation is not a way round anything.

## The traits you have

| Trait | Configured with | What the agent gets |
|---|---|---|
| `query_table` | a table, a field allow-list, a row bound | reads rows the caller may see |
| `insert_row` | a table, the fields it may set | writes one row |
| `update_rows` | a table, the fields it may change | updates the rows a `where` selects |
| `delete_rows` | a table, a row bound | deletes the rows a `where` selects |
| `run_trigger` | one trigger | runs it, with a payload it supplies |
| `coding` | a store, a sub-directory, two grants and three bounds | browses, reads and greps the code; writes and edits it under **May create and change files**; runs one `package.json` script under **May run the project's scripts** |
| `build_application` | an application's subdomain | builds it, and gets the diagnostics |
| `admin_copilot` | four grants, two areas, and **no table** | describes and edits the schema itself, the triggers over it, and an application's custom SQL endpoints |
| `subagent` | one agent, when to use it, two bounds | hands it one task and reads back what it concluded |

Each is a grant. Adding one is a decision you can read off the agent's page later.

## Things that trip people up

- **The redacted key is a sentinel, and a *new* provider's key must be typed.** `••••••••` in an
  existing provider's form means "keep what is stored". Typing those characters into a *new*
  provider stores nothing, and Test connection then fails with the vendor's authentication error
  — which is the right failure, but only if you know what you are looking at.
- **Saving a trigger under a name that already exists *edits that trigger*.** The name is the
  identity — it is what an API path and a Run button reference — so there is no separate "create"
  and "update". An agent that may create but not change existing triggers is refused rather than
  overwriting one, which is the reason those are two checkboxes.
- **A trait is bound to a name, so renaming its table breaks it — deliberately.** Rename `tasks`
  and the agent goes red in the list with ``no table named `tasks` ``, rather than quietly losing
  the tool and answering from memory. Fix it by editing the trait. The same is true of a deleted
  trigger, a deleted application and a renamed agent: a trigger whose `run_agent` names an agent
  you deleted leaves the *trigger* not-usable, with the reason on it.
- **`max_steps` is the seatbelt, and the default is 20.** A run that hits it stops with the
  transcript intact and `conclusion: max_steps` — not an error, and not an answer either. If an
  agent keeps stopping there, it is usually looping on a tool that keeps failing; open the tool
  results in the transcript and read what it was told. Raise it on the agent's **Max steps per
  run** if the work genuinely needs more turns.
- **A tool sees only what its caller may see.** The same `query_tasks` asked by you and by a
  Member returns different rows, because both are ordinary reads under §7.3. An agent is not a
  way around ownership — which also means an agent that "cannot see" a row is often working
  perfectly.
- **A failing tool is not a failing run.** The error goes back to the model *as the tool result*,
  because an error a model can read is one it can recover from. Only a failure of the loop itself
  — the provider refusing, the key being wrong — ends a run, and that lands in the transcript as
  an `error` entry rather than a window that silently stops.
- **The conversation only grows.** Nothing summarises or truncates it yet, so a very long chat
  eventually hits the provider's context limit. Start a new conversation rather than fighting it.
- **A triggered run cannot run a trigger.** Give an agent `run_trigger` and start it from
  `run_agent`, and that one tool answers with "this needs the trigger dispatcher, and none is
  available in this context" — which the model reports rather than dies on. Chat is where an agent runs triggers.
- **`admin_copilot`'s access-rules grant is the one to think hardest about.** The other
  three change *your* schema; that one changes what **everyone else** on the deployment can
  reach — a table's role floors, who may run a trigger through the API, who may call a custom SQL
  endpoint — and unlike a dropped table it looks from the outside like nothing happened. It is off by default, it is a separate
  checkbox from dropping on purpose, and every one of the trait's tools
  refuses any conversation whose user is not an admin — so an agent you expose to a Member at
  role 80 will not hand them the table editor even if you tick every box.
- **`alter_table` and `save_trigger` leave what you do not name.** Ask for `min_role_read` alone
  and the ownership formula and the RLS flag stay exactly as they were; ask for a trigger's new
  subject line and its event, table and condition are untouched. This is deliberately *unlike* the admin
  UI's own Save button, which sends the whole settings card because it is showing you the whole
  settings card. If you want a formula cleared, say so.
- **A refused batch applies nothing.** Twelve operations, the fifth invalid, and none of the
  twelve happened — the message names operation 4 and what was wrong with it. Ask the agent to
  fix that one and resend; there is nothing half-built to clean up first.
- **An application created before you connected a provider has no builder agent.** The agent an
  application comes with needs a provider to point at, so an app made on a fresh install is
  created — and says so in the banner — without one. Connect a provider and build the agent by
  hand as Step 5 does; nothing re-runs it for you, because an application you have since edited is
  not one to quietly add an agent to.
- **Nothing here is encrypted at rest.** Provider keys sit in the primary database like every
  other configuration value. Treat a database dump accordingly.

## What next

You have an agent that reads your data, one that changes your code, and a trigger that starts one
on its own. The formula language the prompt is written in is
[tutorial-ownership.md](tutorial-ownership.md)'s; the trigger it hangs off is
[tutorial-triggers.md](tutorial-triggers.md)'s; and the project the coding agent edits is the one
[tutorial-react-todo.md](tutorial-react-todo.md) generated — the agent is using the same IDE
capabilities you have, through the same file store.

The next tutorial gives an application a **second API** beside its REST one:
[tutorial-graphql.md](tutorial-graphql.md) enables the GraphQL provider, answers "for each
department, how many employees earn under 50 000" in one round trip with the count computed by
the database, and types that query in the React app so a schema change breaks the build rather
than the page.
