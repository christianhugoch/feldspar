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
  every step of every run is written to `_sc_runs` as it happens, so a chat panel that was closed
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

Make a second agent, `coder`, over the same provider, with this system prompt:

```
You maintain a React + TypeScript app. Read before you edit, build after you edit, and fix what
you broke.
```

Add six traits. Every one of them takes the same two settings, so an admin who has configured one
has configured all of them:

| Trait | File store | Sub-directory |
|---|---|---|
| `list_files` | `apps` | `todo` |
| `search_files` | `apps` | `todo` |
| `read_file` | `apps` | `todo` |
| `edit_file` | `apps` | `todo` |
| `write_file` | `apps` | `todo` |
| `run_project_script` | `apps` | `todo` |

…and one more that names the application instead, because which store the source is in is the
*application's* own configuration:

| Trait | Setting | Value |
|---|---|---|
| `build_application` | Application (subdomain) | `todo` |

Save, chat, and ask: *add a "notes" column to the Tasks page*.

Watch the transcript. It will `search_files` for where the row type is used, `read_file` the page,
`edit_file` it, and `build_todo` — and if it guessed a property that is not on `TasksRow`, the
build comes back **not** as "build failed" but as the diagnostics themselves, file and line and
message, which is the most useful thing a model can be told. It reads its own type error and
fixes it.

Four things about that set:

- **`edit_file` is exact-string replacement.** A match that is absent, or that appears twice, is
  an error the model reads and retries — never a fuzzy edit, which is a corrupted file nobody
  notices. `replace_all` is the explicit opt-in for the rename-through-a-file case.
- **The sub-directory is a confinement**, not a convenience. A path that escapes it is refused by
  any spelling, and every path the model is shown is relative to it.
- **`build_application` builds, it does not publish.** An agent's build answers "does this
  compile?"; mounting what it built is still your **Build** button, which is also where you get to
  look at the diff first.
- **There is no shell.** `run_project_script` runs `npm run <script>` for a script your
  `package.json` already declares, and refuses anything else by listing the scripts that exist.
  The runnable set is the project's own; the model chooses from it rather than composing a
  command line.

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

## The traits you have

| Trait | Configured with | What the agent gets |
|---|---|---|
| `query_table` | a table, a field allow-list, a row bound | reads rows the caller may see |
| `insert_row` | a table, the fields it may set | writes one row |
| `update_rows` | a table, the fields it may change | updates the rows a `where` selects |
| `delete_rows` | a table, a row bound | deletes the rows a `where` selects |
| `run_trigger` | one trigger | runs it, with a payload it supplies |
| `list_files` / `read_file` / `search_files` | a store and a sub-directory | browses, reads, greps |
| `write_file` / `edit_file` | a store and a sub-directory | writes and edits |
| `build_application` | an application's subdomain | builds it, and gets the diagnostics |
| `run_project_script` | a store, a sub-directory, a timeout | runs one `package.json` script |

Each is a grant. Adding one is a decision you can read off the agent's page later.

## Things that trip people up

- **The redacted key is a sentinel, and a *new* provider's key must be typed.** `••••••••` in an
  existing provider's form means "keep what is stored". Typing those characters into a *new*
  provider stores nothing, and Test connection then fails with the vendor's authentication error
  — which is the right failure, but only if you know what you are looking at.
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
- **Nothing here is encrypted at rest.** Provider keys sit in the primary database like every
  other configuration value. Treat a database dump accordingly.

## What next

You have an agent that reads your data, one that changes your code, and a trigger that starts one
on its own. The formula language the prompt is written in is
[tutorial-ownership.md](tutorial-ownership.md)'s; the trigger it hangs off is
[tutorial-triggers.md](tutorial-triggers.md)'s; and the project the coding agent edits is the one
[tutorial-react-todo.md](tutorial-react-todo.md) generated — the agent is using the same IDE
capabilities you have, through the same file store.
