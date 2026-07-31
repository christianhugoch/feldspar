# Saltcorn v2 — Agents TODO

Ordered, checkable task list for the sixth milestone after the MVP. Earlier lists are archived
in [docs/TODO-mvp.md](./docs/TODO-mvp.md) (the MVP),
[docs/TODO-post-mvp-1.md](./docs/TODO-post-mvp-1.md) (file stores + the React framework),
[docs/TODO-post-mvp-2.md](./docs/TODO-post-mvp-2.md) (the `_sc_tables`/`_sc_fields` overlays,
rich types and File fields), [docs/TODO-post-mvp-3.md](./docs/TODO-post-mvp-3.md) (ownership
formulae, calculated fields and row-level security),
[docs/TODO-post-mvp-4.md](./docs/TODO-post-mvp-4.md) (actions and triggers) and
[docs/TODO-post-mvp-5.md](./docs/TODO-post-mvp-5.md) (the file-store IDE); scope and rationale
remain in [docs/GOALS.md](./docs/GOALS.md) and
[docs/TECHNICAL_DESIGN.md](./docs/TECHNICAL_DESIGN.md) (**§11**, rewritten for this milestone).

**Milestone definition of done:** an admin connects an **LLM provider** (an Anthropic key, or any
OpenAI-compatible endpoint speaking the Responses API), creates an **agent** — a name, that
provider, a system prompt and a set of enabled **traits**, each configured from a form the admin
UI renders from the trait's own declaration — and opens the **chat panel** in the admin UI. The
agent answers with its text streaming in. Asked about the data it queries a table it was given
and reports what it found; asked to do something it runs a trigger it was given. Pointed at a
file store it reads and greps the project, edits a file, builds the application and reports the
diagnostics from a build it broke. The same agent is reachable from a **trigger**, so a row
insert can start it, and the run it produces is readable afterwards in the same history the chat
panel shows.

**Not the copilot.** The app-building traits — create tables, create fields, create views,
create triggers — and the staged AppConstructor are *explicitly out of scope* (§11.6). This
milestone builds the machinery they will be written against: the provider seam, the agent record,
the `AgentTrait` extension point, the loop, the run storage and the chat UI. If this milestone
ends with a trait that constructs a table, something has gone wrong.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

---

## Decisions taken up front

1. **`rig-core` for the provider calls, behind our own object-safe seam.** The request was for a
   cross-vendor crate and `rig-core` (0.41) is the one that has both required APIs with depth:
   `providers::openai::responses_api` (the **Responses** API — reasoning items, tool choice,
   structured outputs, streaming) and a maintained `providers::anthropic` (content blocks, cache
   control, thinking). It defaults to `rustls`, matching §16's no-OpenSSL posture. The surveyed
   alternatives and why each lost are recorded in §11.1: **`llm`/`rllm`** — the crate named in the
   request, and its `LLMProvider` trait is object-safe, but it speaks Chat Completions only;
   **`litellm-rust`, `multi_llm`, `tiycore`, `llm-sdk-rs`** — younger takes on the same surface,
   neither API as deep; **hand-writing two clients** — the serious alternative, which loses on SSE
   framing, partial-JSON tool-argument assembly and each vendor's error shapes, not on the happy
   path.
2. **Only the provider call is rig's.** Not its `Agent`, not its tool registry, not its RAG. The
   loop is ours because it persists to `_sc_runs`, runs every tool as the chatting user, and
   streams to a browser. Rig's `CompletionModel` is not object-safe anyway (associated types,
   `impl Future`, `Clone`), so a `Box<dyn>` chosen from stored configuration needs a seam
   regardless — `sc-llm` is that seam and not much more. **Re-surveyed before Phase 2** now that
   0.41 has split the runtime into a separate `rig-agent` crate, and the decision stands: rig
   erases tools and vector stores but *not* models, so `Agent<M>` would need a unified model enum
   that makes `sc-llm`'s adapters redundant; its `ConversationMemory` appends per turn where
   `_sc_runs` writes per step; and it has nothing to say about the agent record, the trait specs
   or the validation, which are most of this milestone's code. §11.1 records the survey in full.
   **One piece of its design is taken** — see decision 8.
3. **Streaming is the only shape.** A non-streaming call is a stream collected to the end; the
   reverse is not true, and the chat needs deltas from the first turn. One path also means the
   tool-call assembly providers each do differently is written and tested once.
4. **An agent is its own record, not a trigger's configuration.** `_sc_agents` holds it;
   `run_agent` is the single registered `Action` that runs one. So an agent becomes a trigger body
   through machinery that already exists (§10.2) and stays editable as an agent.
5. **A tool runs as the caller, an action runs as the admin.** Every trait's tool goes through
   `sc-api`'s rows module with the chatting user, so §7.3's ownership and RLS apply and an agent
   is not a way around them. A trigger-started run carries the trigger's authority instead, and
   the difference is set at exactly one place — where the run is created.
6. **No shell.** There is no `run_command` trait. The IDE milestone declined to hand an admin a
   terminal and that decision must not arrive by the back door; `run_project_script` (an existing
   `package.json` script, by name) is the bounded version that ships.
7. **Tests split as before.** Rust for `sc-llm` (against a stub HTTP server, never a real vendor),
   `sc-agent` (the loop against a scripted fake provider) and `sc-core-traits` (real Postgres,
   real file stores). `vitest` for `ui/admin`'s chat model against a stubbed socket. **No test may
   require an API key or spend a token**; the fake provider is a first-class part of the crate, not
   a test fixture bolted on.
8. **The loop is a steppable machine, not an `async fn`.** `rig-agent`'s `AgentRun` is sans-IO and
   serialisable: it decides, the driver does the IO. Ours is built in that shape over `sc-llm`'s
   types, so the run state *is* what `_sc_runs` stores, a reload resumes by construction, and
   §10.3's engine gets the same machine instead of a second one.

---

## Phase 1 — `sc-llm`: the provider seam, and the providers to configure

- [x] New crate `sc-llm` at layer 6 (depends on `sc-error`, `sc-types`, `sc-catalog` for its
      storage). Add `rig-core` to the workspace dependencies with `default-features = false` plus
      `reqwest` + `rustls`, and reconcile the tree's `reqwest` version with rig's 0.13 so there is
      one HTTP client in the build, not two.
- [x] The vocabulary of §11.1 as this crate's own types: `LlmRequest` (system, messages, tools,
      max tokens, temperature), `LlmMessage` (`User` | `Assistant { content, tool_calls }` |
      `ToolResult`), `ToolSpec` (name, description, JSON-Schema parameters), `LlmDelta`
      (`Text` | `Reasoning` | `ToolCall` | `Stop { reason, usage }`), and the object-safe
      `LlmProvider { fn model(&self) -> &str; async fn stream(&self, req) -> Result<LlmStream> }`.
      Nothing rig exposes may appear in a public signature.
- [x] Two adapters. `openai_responses` over `providers::openai::responses_api` with a configurable
      `base_url`, so *any* OpenAI-compatible Responses endpoint is a field value rather than a code
      change. `anthropic` over `providers::anthropic`. Both map deltas onto `LlmDelta` and
      assemble partial tool-call arguments into complete JSON before emitting a `ToolCall`.
- [x] `LlmStream::collect` — the whole stream to one assistant message plus usage. The
      non-streaming callers (`run_agent`, every test that does not care about deltas) use it, and
      it is the only place the two shapes meet.
- [x] **Secrets: `FormField::secret`.** A `bool` on the declaration, so it reaches every consumer
      at once. Where a record carrying a spec-declared config is serialised, a secret value is
      replaced by a fixed sentinel (**not** a truncation — a prefix is still a leak), and a save
      that submits the sentinel unchanged **keeps the stored value**. Do the redaction where the
      record is serialised, never in a screen. `sc-files`' backend specs get the flag too, since
      an S3 key is the same problem.
- [x] `_sc_llm_providers` storage, following `_sc_file_stores`: `name` (unique), `backend`,
      `description`, `config` (`Attrs`), validated against the backend's `config_spec` on save and
      on load, strict on read. `connect_provider(def, model) -> Arc<dyn LlmProvider>`.
- [x] Admin API + UI: `listLlmProviders` / `saveLlmProvider` / `deleteLlmProvider` /
      `llmProviderBackends` (specs, so the form is generic), and an `LlmProviders` list +
      `LlmProviderForm` screen modelled on `FileStores` / `FileStoreForm` — including a **Test
      connection** button that sends one trivial prompt and reports the provider's own error text,
      because a wrong key must be discoverable here rather than inside a chat transcript.
- [x] Tests: Rust — each adapter against a **stub HTTP server** replaying a recorded SSE body
      (text, a reasoning block, a tool call split across chunks, a `Stop` with usage), asserting
      the deltas and that a tool call is emitted only once its arguments parse; a provider error
      (401, a malformed body, a truncated stream) surfaces as an error rather than an empty
      stream; `collect` reassembles what `stream` emitted; the secret sentinel round-trips through
      save/read/save without changing the stored key, and never appears in a listing response.
- [x] **Done when** an admin saves an Anthropic provider and an OpenAI-compatible one, presses
      Test connection on each and gets an answer, and re-opening the form shows the key redacted
      and saving again does not destroy it.

## Phase 2 — `sc-agent`: the agent, the trait, the loop

- [x] New crate `sc-agent` at layer 7. The `Agent` record of §11.2 (`id`, `name`, `description`,
      `provider`, `model`, `system_prompt`, `traits: Vec<EnabledTrait>`, `min_role`, `attributes`)
      and `_sc_agents` storage in the shape `_sc_triggers` uses: the row *is* the definition, reads
      are strict, a missing or ill-shaped column is an error naming the agent.
- [x] The `AgentTrait` trait of §11.2 — `name`, `description`, `config_spec`, `validate_config`,
      `tools(cfg)`, `call(cfg, tool, args, ctx)`, `on_turn(cfg, turn)` — and `AgentRegistry`, the
      twin of `ActionRegistry` (a `BTreeMap`, duplicate names refused, this crate registering
      nothing itself).
- [x] **A trait may be enabled twice.** `traits` is a list of `(trait, config)` pairs, not a map:
      "query `books`" and "query `orders`" are one trait, twice. Each enabled trait derives its
      tool names from its own configuration, and a collision between two of them is refused **on
      save**, where it is fixable, not discovered when the model picks the wrong tool.
- [x] Validation on save **and on load**, in one function, exactly as a trigger's is (§10.2): the
      provider resolves, each trait name resolves, each configuration validates against its
      `config_spec` and then its `validate_config`, `min_role` is on the 1–100 scale, tool names do
      not collide. An agent that fails is dropped from the live set **with its reason kept**, and
      stays stored, listed and editable — editing it is the repair.
- [x] `_sc_runs`, created here with the shape the workflow engine will also use: `id`, `kind`
      (`agent` now, `workflow` later), `subject`, `context` (JSON — the message history and
      accumulated usage), `state`, `user`, `created_at`/`updated_at`. Written after **every** step,
      so a reload resumes and a durable engine later needs no second mechanism.
- [x] The loop: build the request (system prompt plus whatever `on_turn` appended, the run's
      messages, every enabled trait's tools) → stream → dispatch tool calls **sequentially in the
      order the model asked** → append results → repeat, bounded by `max_steps` (default 20). A
      tool that fails returns its error **as the tool result**, because an error the model can read
      is one it can recover from; only a failure of the loop itself ends the run.
- [x] The run's caller travels with it and every tool executes as that user (decision 5). The
      constructor takes the caller; there is no default and no ambient admin.
- [x] A **`FakeProvider`** in the crate (behind a `testing` feature, not `#[cfg(test)]`, so
      `sc-core-traits` and `sc-server` can use it): scripted turns — "emit this text", "call this
      tool with these arguments", "then say this" — which is what makes the loop, every trait and
      the chat socket testable without a vendor.
- [x] Tests: Rust — a single-turn run; a run that calls one tool and continues; two tool calls in
      one assistant message, executed in order; a tool that errors, with the loop continuing and
      the error visible in the transcript; `max_steps` stopping a provider that never stops asking;
      the run row's context after each step matching what the loop holds; save/load round-trip and
      an agent naming a missing provider or an unknown trait leaving the live set with a reason.
- [x] **Done when** an agent with no traits at all holds a two-turn conversation through the
      library against the fake provider and against a real one, and its run row can be reloaded
      into the same message history.

## Phase 3 — `sc-core-traits`: connecting tables and actions

- [ ] New crate `sc-core-traits` at layer 9, beside `sc-core-actions` and for the same reason: a
      trait that writes a row goes **through `sc-api::rows`**, so the write is coerced, validated,
      `File`-field-checked and observed by triggers exactly like an API caller's.
- [ ] `query_table` — configured with one table, an optional field allow-list and a maximum row
      count. Its tool takes a `where` object, an optional ordering and a `limit` bounded by the
      configuration. The tool's description and JSON schema are **generated from the table's own
      fields**, so the model is told what it may filter on rather than guessing.
- [ ] `insert_row`, `update_rows`, `delete_rows` as three separate opt-in traits over a configured
      table, so a read-only agent is the default shape and each grant is a deliberate act with a
      form field attached. `delete_rows` requires a `where`, for §10.1's reason.
- [ ] `run_trigger` — one configured trigger as one tool, taking the event payload. The trigger's
      own `min_role` still gates it, so exposing the agent to a role does not thereby expose
      everything it can reach. This is the trait that connects an agent to all of §10 — and to
      workflows, unchanged, once §10.3 lands.
- [ ] Each trait's `validate_config` checks what the spec cannot: that the table exists and is
      addressable by primary key, that the trigger exists, that a named field is real.
- [ ] Tests: Rust against a real Postgres — a query trait returning rows the caller may see and
      **not** returning rows an ownership formula hides (the same table read by two callers gives
      two answers); a write trait's insert firing the table's own trigger; `delete_rows` refusing a
      missing `where`; `run_trigger` refused for a caller below the trigger's `min_role`; a trait
      configured against a dropped table leaving its agent invalid with a reason.
- [ ] **Done when** an agent given `query_table` over a real table and `run_trigger` over a real
      trigger answers a question about the data and then performs the action, with the whole
      exchange in its run's context.

## Phase 4 — The chat interface in the admin UI

- [ ] Typed admin endpoints for everything that is a request/response pair: `listAgents`,
      `saveAgent`, `deleteAgent`, `agentTraits` (the registry's specs, so the form is generic),
      `listRuns(agent)`, `getRun(id)`, `deleteRun(id)`.
- [ ] **The turn itself is a WebSocket**, `/admin/agent-chat`, admin-only through the same session
      middleware as the language-server route and following its precedent (§12.1): a chat turn is
      bidirectional — deltas out while a new message or an abort may come in — and the typed
      endpoint model describes pairs. Client sends `{ start | message | abort }`; server sends
      `{ text | reasoning | tool_call | tool_result | done | error }`.
- [ ] **A failure is an event, not a dropped connection.** A provider that refuses, a key that is
      wrong, a tool that panics: each renders in the transcript, because a chat window that
      silently stops is unfixable by the person watching it. Aborting closes the provider stream
      and leaves the run in a state the history can show.
- [ ] `Agents` (list) and `AgentForm` — provider, model, system prompt, `min_role`, the sparse
      attributes, and the trait picker: add a trait, and its `config_spec` renders through the
      existing `SettingsFields`, once per enabled instance.
- [ ] `AgentChat` — transcript of user and assistant messages, streaming text, tool calls as
      collapsible entries naming the tool with its arguments and result, a composer, a stop
      button, and the agent's run history with an old run reopening read-only.
- [ ] Tests: Rust — the socket refuses a non-admin, refuses an agent above the caller's role,
      and drives a whole turn against the `FakeProvider` including a tool call and an abort;
      a provider error arrives as an `error` event on an open socket. `vitest` — the chat model
      against a stubbed socket: deltas appended in order, a tool call and its result paired,
      an error rendered rather than swallowed, abort leaving the transcript intact.
- [ ] **Done when** an admin creates an agent in the UI, chats with it, watches the text stream and
      a tool call expand with its result, stops a long answer, reloads the page and finds the
      conversation in the history.

## Phase 5 — Coding traits: building code in a file store

- [ ] The coding traits work inside **one configured file store**, optionally rooted at a
      subdirectory, through the `FileStore` trait and §9's access rules — the same capability the
      file manager and the IDE already have, handed to a model. A path that escapes the configured
      root is an error, exactly as it is for the byte-level methods.
- [ ] `read_file`, `write_file`, `list_files`, and `edit_file` — **exact-string replacement**, the
      edit that can be verified before it is applied: a match that is absent or ambiguous is an
      error the model can read and retry, whereas a fuzzy edit is a corrupted file nobody noticed.
- [ ] `search_files` — a **server-side** search over the store (a literal or a regex, with a file
      glob and a bounded result count). This is also the endpoint the IDE's find-in-files wanted
      (carried past the last milestone), so expose it as an admin endpoint too and switch the IDE's
      search over to it.
- [ ] `build_application` — builds the application whose source is that store, returning the
      build's **diagnostics** as the tool result. A failed build is the most useful thing the model
      can be told, so the report is passed through structurally, not reduced to "build failed".
- [ ] `run_project_script` — `npm run <script>` for a script that **already exists** in the
      project's `package.json`, so the runnable set is the project's own and the model chooses from
      it rather than composing a command. Bounded timeout and captured output, both streams, as
      `run_build` carries them. **No `run_command`, no shell** (decision 6).
- [ ] Tests: Rust against a real local store — read/write/list round-trip; `edit_file` applying a
      unique match, refusing an absent one and refusing an ambiguous one; a path escaping the
      configured subdirectory refused; `search_files` finding a literal and a regex across
      directories with the result bound respected; `build_application` on a deliberately broken
      project returning the diagnostics with file and line; `run_project_script` refusing a script
      not in `package.json` and running one that is. `vitest` — the IDE's search using the new
      endpoint.
- [ ] **Done when** an agent pointed at the React tutorial's store is asked to add a field to the
      to-do app, greps for where the type is declared, edits the file, builds, reads the type error
      it caused, fixes it and builds clean — with every step visible in the chat transcript.

## Phase 6 — The agent as a trigger body, and documentation

- [ ] `run_agent`: a registered `Action` in `sc-core-traits` taking an agent name and a **prompt
      formula** evaluated in the event's scope (§10.1), so a row insert starts an agent with a
      prompt derived from the row. It runs with the trigger's authority (decision 5) and returns
      the final assistant message plus the run id. It does **not** stream — an action returns a
      value, and a caller who wants deltas is a chat client.
- [ ] `validate_config` resolves the agent by name and the formula in the event's scope, on save
      and on load, so a trigger naming a deleted agent leaves the live set with a reason instead of
      failing when a row is inserted.
- [ ] An application may expose such a trigger as `POST {mount}/actions/{name}` under the trigger's
      own `min_role` (§13.2), with no change to §13.2 — confirm this by test rather than by
      assertion.
- [ ] A new tutorial, `docs/tutorial-agents.md`: connect a provider, build a data agent over the
      to-do app's table, chat with it, add `run_trigger`, then point a second agent at the store
      and have it change the app's code. Written as the other tutorials are — the reader's screen,
      and a "trips people up" section (the redacted key, a trait configured against a renamed
      table, `max_steps`, and the fact that a tool sees only what its caller may see).
- [ ] §11 revised to describe what was **built** where it deviates from what was planned — the
      rig APIs that turned out to be wrong or missing, the socket protocol as it settled, and any
      trait whose configuration changed shape. This is the step the last two milestones proved is
      easy to skip and expensive to skip.
- [ ] Tests: `repo_hygiene.rs` asserts the new tutorial still teaches each step of the loop and
      that §11 still records each deviation.
- [ ] **Done when** an insert on a table fires a trigger that runs an agent, the row's data reaches
      the prompt, and the run is readable afterwards in the chat panel's history.

---

## Carried past this milestone

- **Streaming from `run_agent`.** A triggered run's deltas are not observable while it runs, only
  afterwards from its run row. Watching one live wants the chat socket to be able to attach to a
  run it did not start.
- **Conversation compaction.** A long chat grows its context until the provider refuses. The run
  row holds everything; nothing summarises or truncates it yet.
- **Prompt caching.** Both providers support it and rig exposes Anthropic's cache control; using
  it well means deciding what is stable across turns, which is a measurement, not a design.
- **Parallel tool execution.** Tool calls run sequentially (Phase 2). Running independent reads
  concurrently needs a way to know which are independent.
- **The rest of the trait catalogue** §11 names from v1: MCP client, subagent handoff, long-term
  memory, web search, plan approval, model picker, preload data, generate-and-run code.

## Explicitly OUT of scope for this milestone

- **The copilot and the AppConstructor** (§11.6), and every app-building trait — creating tables,
  fields, views, triggers or applications. This is the milestone's sharpest boundary.
- **A `run_command` / shell trait** (decision 6), and giving an agent the IDE's terminal, which
  does not exist either.
- **Encryption at rest for provider keys.** They sit in the primary database like every other
  configuration value; §11.1 says so rather than implying a protection a database dump would
  disprove.
- **Embeddings, vector stores and RAG**, and image or file inputs to the model.
- **Agent-chat as an application-facing view pattern.** Chat is an admin surface this milestone;
  a ChatGPT-like view for end users is §11's other front-end and waits for view patterns.
- **The durable workflow engine** (§10.3). `_sc_runs` is created in its shape, deliberately, but
  nothing resumes or retries.
- Everything still listed as out of scope in [docs/TODO-mvp.md](./docs/TODO-mvp.md),
  [docs/TODO-post-mvp-1.md](./docs/TODO-post-mvp-1.md),
  [docs/TODO-post-mvp-2.md](./docs/TODO-post-mvp-2.md),
  [docs/TODO-post-mvp-3.md](./docs/TODO-post-mvp-3.md),
  [docs/TODO-post-mvp-4.md](./docs/TODO-post-mvp-4.md) and
  [docs/TODO-post-mvp-5.md](./docs/TODO-post-mvp-5.md)
