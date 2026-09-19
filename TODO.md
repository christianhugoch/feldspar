# Saltcorn v2 — Streams: dataflows as an entity

Ordered, checkable task list for the twenty-seventh milestone after the MVP. Earlier lists are
archived in [docs/TODO-mvp.md](./docs/TODO-mvp.md) (the MVP) and
`docs/TODO-post-mvp-1.md` … [docs/TODO-post-mvp-26.md](./docs/TODO-post-mvp-26.md) (most
recently: Saltcorn UI, the builder and the library, and the coding agent rebuilt for cheap
models). Scope and rationale are in [docs/GOALS.md](./docs/GOALS.md) — three sentences, quoted
in full in §1 — and this list is where they become a design.

Everything Saltcorn holds today is **at rest**: a table has rows, a file has bytes, a model has
a fit. The one thing that moves is an event, and every event this system knows how to raise it
raises itself — a write, a login, a clock. Nothing can be told about the world from outside
except by something calling in over HTTP. That is the gap: a temperature sensor publishing to
an MQTT broker, a market feed, a queue of jobs from another system. They are not rows and they
are not requests. They are **dataflows**, and GOALS makes them an entity.

**Milestone definition of done:** an admin creates a stream from the built-in MQTT provider,
fills in the broker and a topic filter, and the stream's Observe screen shows elements arriving
live with their declared shape. A trigger with `when = stream` on that stream fires once per
element and inserts a row. An application exposes the stream and its generated client observes
it over an authenticated WebSocket, with the element type in its TypeScript. Saving a stream
starts it, disabling it stops it, and a broker that goes away is reconnected to without a
restart. The same scenario — minus the broker — passes in `cargo test` against a scripted
provider.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

---

# The specification

### 1. What GOALS says, and what it decides

> - streams - a new entity type representing dataflows
>
> **Stream providers:** can provide a stream, when the configuration fields are filled in. The
> stream provider declares its configuration fields, and then as a function of these
> configuration fields can declare the stream element type (e.g. JSON, with known keys each
> having a known part; text (which encoding?) or binary) and some way of observing the elements
> of the stream. Stream providers are supplied by plugin modules, and there is a built-in stream
> provider for MQTT.
>
> **Streams:** created from stream providers. Can be observed in the admin UI, by applications
> through their API (e.g. with an authenticated websocket interface the application can connect
> to), or can become the triggering event of a trigger. In the Data Layer part of the admin UI,
> there is a Streams link, linking to a list of the created streams, each with links to Editing,
> Observing and Deleting. At the top is a link to create a new stream.

(Quoted as written, but for one spelling; "each having a known part" is read as *type*, which is
what §4 makes it.)

Read against what is already here, that is a shape this tree has built four times: a **provider
is code declaring its settings as [`FormField`]s**, an **entity is a row that is its own
definition** (§9), and the admin UI **renders a provider it has never heard of**. A stream is a
`Model` with a `ModelProvider` replaced by a subscription, or a `Trigger` whose event comes from
outside. The design below reuses those two skeletons wherever it can and says out loud the three
places a flow is genuinely not a fit or a row:

- **An element is not stored.** §4.
- **Nobody may block the flow.** §7.
- **A subscription is process-local and long-lived**, where every other extension point in this
  tree is a call that returns. §6.

### 2. Where everything lives

A new crate, **`sc-stream`**, at layer 6 — `sc-model`'s exact placement, for `sc-model`'s exact
reason: a module supplies providers (layer 6 is where a module host can reach it) and the rows
it stores go through the [`Catalog`] (which fixes it above layer 4). It holds: the
[`StreamProvider`] trait and its registry, the element and its type, the `_fd_streams` store, the
validation, and the supervisor that keeps subscriptions running.

The three seams, each inverted the way its neighbours' already are:

| Seam | Declared in | Implemented in | Installed by |
| --- | --- | --- | --- |
| `StreamProviderHost` — a module's providers | `sc-stream` | `sc-module::stream_providers` | `sc-server` at boot and on module change |
| `StreamSink` — where a delivered element goes | `sc-stream` | `sc-server::streams` | `sc-server` at boot |
| `StreamObserver` — a stream set that changed | `sc-stream` | `sc-server` (mount registry) | `sc-server` at boot |

`sc-stream` therefore depends on nothing above layer 4 and knows nothing about triggers,
applications or sockets — which is what lets the supervisor be tested with a sink that appends to
a `Vec`.

`sc-action` gains one enum variant (§8) and **no dependency**: a stream event reaches the
dispatcher because `sc-server` hands it one, not because `sc-action` knows what a stream is.

### 3. The provider trait

```rust
#[async_trait]
pub trait StreamProvider: Send + Sync {
    fn name(&self) -> &str;
    fn label(&self) -> &str;
    fn description(&self) -> &str;
    /// The settings an admin fills in, as data.
    fn config_spec(&self) -> Vec<FormField>;
    /// The element type **as a function of the configuration** (GOALS).
    fn element_type(&self, config: &Attrs) -> Result<ElementType>;
    /// Start observing. Elements go to `sink` until `stop` is dropped.
    async fn subscribe(&self, config: &Attrs, sink: Arc<dyn StreamSink>) -> Result<Subscription>;
}
```

Three deliberate echoes of [`ModelProvider`], and one deliberate difference.

- **`element_type` takes the configuration**, exactly as `ModelProvider::outcome` does, and for
  the same reason: MQTT with `payload = json` and four declared keys is a different element type
  from the same provider with `payload = text`, and making those two providers would be making
  four. It is fallible, because a configuration can be incoherent (`json` with a key of no type)
  and the admin should hear that while looking at the form.
- **`config_spec` is data**, so the Stream form is the Model form, the Trigger form and the agent
  trait form: one spec-rendered `<Form>` over whatever the picked provider declares, secrets
  redacted by [`redact_attrs`] and restored by [`merge_secrets`] on save.
- **The registry is a `BTreeMap`, rebuilt not mutated**, refusing a duplicate name and naming
  both sources — `ModelRegistry`'s text, because the situation is `ModelRegistry`'s situation.
- **The difference: `subscribe` returns rather than blocks, and hands back a [`Subscription`]** —
  an owned handle whose `Drop` stops the flow. A provider that needs a task spawns it; a provider
  that polls is given a poll loop by the supervisor (§12). Returning the handle rather than
  taking a `&mut self` loop is what makes "stop this stream" a `drop`, which is what makes the
  supervisor's restart path three lines instead of a protocol.

### 4. The element, and its envelope

An element that arrives is delivered as an **envelope**, and the envelope is a wire contract in
the sense `Event::error`'s payload already is — a trigger's `only_if` reads it, an application's
client is typed from it, and it must not change silently:

```json
{ "stream": "boiler", "value": { "temperature": 31.2 }, "received_at": "2026-09-17T09:00:00Z",
  "source": { "topic": "house/boiler/temp", "qos": 0, "retain": false } }
```

- `value` is the element itself, shaped by the stream's [`ElementType`]: an object for `Json`, a
  string for `Text`, base64 for `Binary`.
- `source` is the provider's own metadata, free JSON, absent when a provider has none. MQTT's
  topic lives here rather than beside `value` because "which topic" is a fact about *this
  provider*, and a formula that reads it has already accepted it is talking to MQTT.
- `received_at` is when *this server* saw it, not a claim about when it was produced. A provider
  that knows the producer's timestamp puts it in `source`.

```rust
pub enum ElementType {
    /// An object with known keys, each of a known basic type (GOALS). Unknown
    /// keys are carried through rather than dropped; a declared key that is
    /// absent is `null`.
    Json { keys: Vec<ElementField> },   // ElementField { name, r#type: BasicType, required }
    /// Characters, in a named encoding. `utf8` is the default and the only one
    /// this milestone decodes; anything else is refused at save time rather
    /// than mis-decoded at 3am.
    Text { encoding: String },
    /// Bytes. `value` is base64 in the envelope, and the Observe screen shows a
    /// hex head rather than pretending it is text.
    Binary,
}
```

GOALS asks "(which encoding?)" and the answer is: **the declaration carries one, the runtime
implements UTF-8, and a non-UTF-8 declaration is refused**. Guessing is the failure mode that
produces a stream of replacement characters nobody notices for a week.

**An element is not stored.** No `_fd_stream_elements` table, no retention setting. A stream is a
flow, and what makes it durable is a trigger that writes a row — which is a thing the admin
already knows how to build and can see, query, back up and give away. The Observe screen's
history is a small in-memory ring (§9), explicitly labelled as "since this server started".

### 5. `_fd_streams`

§9's rule applies with nothing to argue about: there is nothing to introspect a stream from, so
the row *is* the definition. Columns, following `_fd_models` and `_fd_triggers`:

| Column | Why |
| --- | --- |
| `id` uuid pk | §9 |
| `name` text, **unique** | what a trigger's channel, an application's `StreamRef` and a socket path name. Renaming breaks those references deliberately, as a trigger's does |
| `description` text | §9 requires one |
| `provider` text | the registered provider name |
| `configuration` json | the provider's settings; secrets stored as given |
| `min_role` int, nullable | the floor for **observing** it through an application. `None` is admin-only — the trigger rule, for the trigger reason: a flow nobody has thought about the access of is not public |
| `attributes` json | sparse (§9): `enabled`, and per-provider bookkeeping |

`element_type` is **not** a column. It is a pure function of `provider` + `configuration` (§3),
and a stored copy would be a second answer that drifts the day a provider's declaration changes.
It is computed on read and cached in the running stream.

Reading is strict, as `load_model` is: a missing column or a wrong shape is an `Error::invalid`
naming the stream and the column.

### 6. Running a stream: the supervisor

`StreamSupervisor` holds one **running stream** per enabled row: the resolved provider, the
element type, the live [`Subscription`], a status, and counters. It is `sc-server`'s
`ModelServices` in role and `Scheduler` in shape — one supervising task, spawned at boot by
`install_streams`.

- **Status** is `starting | running | failed { error, since, attempt } | stopped`, held in memory
  only. There is no `_fd_errors` yet (§16 plans one), so a failure is `tracing::warn!` plus the
  status the admin sees, and the day the error log lands the supervisor is one of its callers.
- **Reconnection is the supervisor's, not the provider's.** A `subscribe` that returns `Err`, and
  a subscription that reports it has ended, are both restarted with exponential backoff capped at
  a minute, counting attempts. Written once here rather than once per provider, because "retry
  properly" is the part every provider gets subtly wrong.
- **A stream set that changed is reloaded, not restarted.** `reload()` diffs the rows against the
  running set by id: started for a new or newly-enabled row, stopped for a deleted or disabled
  one, and **stopped and started** for one whose `provider` or `configuration` changed. A stream
  whose row is untouched keeps its connection — an admin renaming a description must not drop a
  broker session. `StreamObserver` is what tells the mount registry the set moved, the way
  `TriggerObserver` already does.
- **One process, one subscription.** Two servers against one database both subscribe, so a
  stream trigger fires twice. That is a real limitation and it is §13's, not a bug to be
  discovered: `sc-bus` does not exist, and until it does a flow is process-local. The doc says
  so, and MQTT's own shared subscriptions (`$share/`) are the escape hatch an admin has today.

### 7. Delivery: broadcast, and the rule that nobody may block

One [`tokio::sync::broadcast`] channel per running stream. The sink `sc-server` installs
publishes onto it and returns; every consumer — the admin Observe socket, each application
socket, the trigger bridge — is a receiver.

**Nothing back-pressures the flow.** A broker does not wait for an admin's browser, and a
consumer that cannot keep up is *the consumer's* problem:

- A lagging socket receiver gets `RecvError::Lagged(n)` and is **told**: the Observe socket sends
  `{"type":"lagged","dropped":n}` rather than silently showing a gap.
- A trigger that runs slower than its stream produces has its extra firings **dropped, with a
  counter**, exactly as `Scheduler` drops missed occurrences — "five queued copies of a report
  nobody read is worse than one late one", and an unbounded queue in front of a trigger is a
  memory leak with a delay built in.
- Each running stream carries `elements`, `dropped_for_triggers` and `last_element_at`, shown on
  the Streams list. A stream that is dropping is a thing you can see.

The channel's capacity and the per-stream element-rate cap are config, defaulting to something
survivable (1 024 buffered, 1 000 elements/second), and the cap is enforced by counting and
dropping, never by pausing the provider.

### 8. Triggers: `EventKind::Stream`

One variant added to `EventKind` (`"stream"`, twelfth in `EVENT_KINDS`), `channel` = the stream's
name, `payload` = the envelope (§4). Everything else about triggers is untouched, which is the
point: `only_if` (`payload.value.temperature > 30`), `min_role`, the enabled flag, the cascade
chain and bound, the admin's Run button and an application's exposure all already work on an
`Event`.

Two rules the validation has to state:

- **`channel` is required** for a stream trigger, as it is for a table event. `sc-action`
  validates that it is present and non-empty and stops there — it cannot resolve a stream name
  without a dependency it should not have. `sc-server`'s trigger endpoint and form offer the
  live stream list and refuse an unknown name, which is where the admin is anyway.
- **A stream event has no row**, so the row-shaped bindings (`row`, `old`) are absent and
  `only_if` reads `payload`. A formula written against `row` fails the same way it already does
  for a `startup` trigger.

`sc-server::streams` holds the bridge: the sink it installs calls `TriggerDispatcher::fire` for
an event whose channel is this stream, in a spawned task, subject to §7's drop rule.

### 9. Observing in the admin UI

A **Streams** entry in the Data Layer section of the sidebar (`ui/admin/src/App.tsx`'s `NAV`),
between Triggers and Files — a stream is a source of events, so it belongs beside the thing that
listens to them rather than beside the models. Three screens behind it, which is exactly what
GOALS lists: a list (name, provider, status, elements, last element; New at the top; Edit,
Observe and Delete per row), a form (name, description, provider picker, the provider's
spec-rendered settings, `min_role`, enabled), and **Observe**.

Observe is a WebSocket, `GET /api/streams/{id}/observe`, and it is the admin chat socket's
sibling in every respect that matters: admin-only, decided **before** the upgrade, refused with a
status because a browser cannot read a failed handshake's body; everything after the upgrade is
JSON text frames. It sends `{"type":"ready","element_type":{…},"status":…}`, then
`{"type":"element","envelope":{…}}` per element, plus `lagged` and `status` frames. It starts by
replaying the ring buffer (the last 100 envelopes this process saw) so a screen opened on a slow
stream is not blank, and says that is what it is doing.

The rendering follows the element type: a table of declared keys for `Json`, a text tail for
`Text`, a hex head for `Binary`. Pause and Clear are client-side only.

### 10. Observing from an application

An application exposes streams the way it exposes triggers: `StreamRef(name)` on
`Application`, `exposes_stream(name)`, on the same principle — a stream is server-side
configuration, and it becomes reachable from outside only because an app said so.

The route is mounted **beside** the endpoint set, not in it: `EndpointSet` is a typed
request/response model (§13.1) and a socket has no shape in it, which is the same split the
IDE's language server and the admin chat already made. It is `GET {mount}/streams/{name}/observe`,
authenticated by the app's own session cookie, enforcing the stream's `min_role`, refusing an
unexposed or unknown name with a status.

The **generated client** does get it, because that is where the element type earns its keep:
`generate_client` emits an `observeStream_{name}()` returning a typed subscription whose element
is the envelope with `value` typed from [`ElementType`]. An app's client is emitted at build time
(§13.1), so this costs nothing to keep in step; the admin SPA's checked-in client gets the admin
socket's types the same way its chat types are written.

### 11. The MQTT provider

Built in, in `sc-stream::providers::mqtt`, on [`rumqttc`] (pure Rust, tokio, rustls — the
`reqwest`/`axum-server` rule that this tree links one TLS stack). Behind a default-on feature
`mqtt`, as `smartcore` is, so a build can drop it.

Settings: `host`, `port` (1883/8883), `use_tls`, `client_id` (defaulting to a stable
`feldspar-{stream name}`, because a random one per reconnect leaks broker sessions), `username`,
`password` (**secret**), `topic` (a filter, wildcards allowed), `qos` (0/1/2), `clean_session`,
and `payload` — `json | text | binary`, which is what `element_type` reads. With `json`, a
repeating group of declared keys and their types; with `text`, an encoding.

`source` carries `topic`, `qos` and `retain`. A payload that will not parse as the declared type
is **not** delivered: it is counted (`malformed`) and warned once per minute per stream, because
a broker with one misbehaving publisher must not fill the log or the trigger queue.

Tests are offline: the payload decoder, `element_type` over each `payload` setting, the settings
validation, and the topic-filter check are unit-testable without a broker, and are. A live broker
test needs a broker and is a human's (`docs/tutorial-streams.md` says how), the same call this
tree already makes for a vendor API key.

### 12. Stream providers from modules

A module exports `streamproviders` beside its `actions`, `table_providers` and `modelproviders`:

```js
streamproviders: {
  poll_feed: {
    description: "An RSS feed, polled",
    config_fields: [{ name: "url", type: "String", required: true },
                    { name: "interval_s", type: "Integer", default: 60 }],
    element_type: ({ configuration }) => ({ kind: "json", keys: [ … ] }),
    poll: async ({ configuration, cursor }) => ({ elements: [ … ], cursor: "…" }),
  },
}
```

**Poll, not push**, and this is the one place a module provider is shaped differently from a Rust
one. A module call is request/response on a Deno worker (`ModuleHost::call`); there is no channel
from a worker back into the host, and building one is a milestone of its own. So `sc-stream`
supplies the loop: `PollingProvider` wraps a poll-shaped kind, calls it every `interval_s`,
carries the opaque `cursor` between calls, and turns what comes back into elements. A module that
wants a push subscription is out of scope and §OUT says so.

Everything else is `ModuleModelProviders`' text word for word: built whole on every module
change, routed to the worker the module is loaded on, names re-checked on this side, a provider
whose `element_type` cannot be read supplies nothing and the issue stays on the module's card.

### 13. Tests

- **A scripted provider** (`sc-stream::testing::ScriptedProvider`) is this milestone's
  `FakeProvider`: a configured list of elements, optionally on a timer, optionally failing on the
  nth subscribe so the backoff path is a test rather than a hope. Everything below rides on it.
- Live-database tests for the store (`sc-test-harness`, as `model_store.rs` does).
- Supervisor tests: start/stop/reload diffing, restart on a changed configuration, no restart on
  an unchanged one, backoff on a failing subscribe, status and counters.
- Delivery tests: a lagging receiver is told; a slow trigger drops and counts rather than growing.
- A trigger test: an element fires a trigger that inserts a row, `only_if` over `payload`, and
  the cascade bound still holds.
- Socket tests in `sc-server/tests/`: the admin observe socket (auth before upgrade, the ready
  frame, replay, elements) and an application's (exposure, `min_role`, an unknown name).
- A client-generation test: the emitted TypeScript for each `ElementType`.
- UI unit tests for the pure parts (`ui/admin/src/streams.ts`), as every other screen has.

---

# The work

## Phase 1 — `sc-stream`: the crate, the element, the provider

- [x] 1.1 New crate `crates/sc-stream` at layer 6 (workspace member, `sc-error`, `sc-types`,
      `sc-catalog`, `sc-db`, `sc-query`, `uuid`, `chrono`, `tokio`, `async-trait`), with the
      module-level docs §2 asks for: what a stream is, why an element is not stored, and the
      three seams.
- [x] 1.2 `ElementType`, `ElementField`, and their JSON round trip (§4). Validation: a `Json`
      type with no keys, a duplicate key, or a `Text` encoding that is not `utf8` is refused,
      naming it.
- [x] 1.3 `Element`/`Envelope`: construction from a provider's raw payload against an
      `ElementType` (object, string, bytes → base64), `received_at`, and `source`. The envelope
      JSON is asserted field by field in a test — it is a wire contract.
- [x] 1.4 `StreamProvider`, `Subscription` (a `Drop`-stops handle), `StreamSink`, and
      `StreamRegistry` (`BTreeMap`, duplicates refused naming both sources, `register_host` for
      module-supplied kinds). `builtin_providers()`.
- [x] 1.5 `testing::ScriptedProvider` (§13) and its own tests: elements arrive in order, dropping
      the subscription stops them, the nth subscribe fails on demand.

## Phase 2 — `_fd_streams`

- [x] 2.1 `store.rs`: `STREAMS_TABLE`, the §5 columns, `bootstrap_streams(catalog)` (called from
      `install_streams` at boot, where `bootstrap_models` is called from — a stream is no use to
      a `feldspar` command that is not serving), `Stream`/`StreamId`, and
      `save_stream` / `load_stream` / `load_stream_by_name` / `list_streams` / `delete_stream`,
      read strictly.
- [x] 2.2 `validate.rs`: the provider exists; the configuration validates against its
      `config_spec` (`validate_attrs`); `element_type(config)` succeeds; the name is unique and
      is a legal identifier (it becomes a socket path segment); `min_role` is a known role.
      `save_stream` calls it first.
- [x] 2.3 Secrets: `redact_attrs` on the way out of the store's read-for-display path and
      `merge_secrets` on save, so a password survives an edit that did not retype it. Test with a
      scripted provider that declares a secret.
- [x] 2.4 `delete_stream` refuses while a trigger names the stream as its channel, listing the
      triggers — the refusal `delete_llm_model` already makes, for the same reason. The caller
      passes the referents in, as it does there.
- [x] 2.5 Live-database tests (`sc-test-harness`): round trip, strict read of a damaged row,
      uniqueness, the delete refusal, secret merge.

## Phase 3 — The supervisor

- [x] 3.1 `RunningStream` and `StreamStatus` (§6), with `elements`, `dropped_for_triggers`,
      `malformed` and `last_element_at` counters.
- [x] 3.2 `StreamSupervisor::start`/`stop`/`reload(catalog)`: the id-diff, the
      stop-and-start-on-changed-configuration rule, and leaving an untouched stream's connection
      alone. Tests over `ScriptedProvider`.
- [x] 3.3 Reconnection with capped exponential backoff and an attempt count; a failing subscribe
      leaves `failed` with the error and keeps retrying; a subscription that ends is restarted.
      Test with the nth-failure scripted provider, with the clock injected so it runs in
      milliseconds (`Scheduler::tick`'s rule: the clock is a parameter).
- [x] 3.4 The broadcast channel per stream, `subscribe_elements()` for consumers, the element
      rate cap, and the `Lagged` path (§7). Tests: a slow receiver is told how many it lost; a
      stream over its cap drops and counts.
- [x] 3.5 `StreamObserver` (§2's third seam) called after a reload that changed the set.

## Phase 4 — The MQTT provider

- [x] 4.1 `rumqttc` as a workspace dependency, rustls-only, with the comment the other pinned
      network crates carry saying why this TLS stack. Feature `mqtt`, default on.
- [x] 4.2 `providers::mqtt`: the §11 `config_spec` (password `secret()`, `payload` as a picker,
      the repeating key group for `json`), `element_type` over each `payload` setting, and the
      settings validation (a topic filter's wildcards, a port, a `json` payload with no keys).
- [x] 4.3 `subscribe`: connect, subscribe to the filter at the chosen QoS, decode each publish
      against the element type, build the envelope with `source = {topic, qos, retain}`, and hand
      it to the sink. The event loop runs in a task the `Subscription` owns and stops on drop.
- [x] 4.4 A malformed payload is counted and warned at most once a minute per stream, never
      delivered. Unit test over the decoder for all three payload kinds.
- [x] 4.5 `docs/tutorial-streams.md` gains the "check it against a real broker" recipe (mosquitto
      in one command, `mosquitto_pub`), since the live test is a human's.

## Phase 5 — Triggers on streams

- [x] 5.1 `EventKind::Stream` in `sc-action`: the variant, `as_str`/`parse`, `EVENT_KINDS` (now
      12), and `Event::stream(name, envelope)` writing the payload out at the constructor as
      `Event::error` does.
- [x] 5.2 Trigger validation: `channel` required and non-empty for a stream event; no `only_if`
      row bindings. Existing trigger tests extended.
- [x] 5.3 `sc-server/src/streams.rs`: `StreamServices` (the registry, the supervisor, the row cap
      and config) and `install_streams` at boot — bootstrap, load, start, and install the sink
      that (a) broadcasts and (b) fires the trigger dispatcher per element in a spawned task,
      with §7's drop-and-count rule. `StreamServices` rides on `AppMounts` with the other five.
- [x] 5.4 A `SIGHUP` reload reloads the stream set, and `reload.rs`'s doc list of what still
      wants a restart is updated (it currently names the trigger set and the agents).
- [x] 5.5 Test: an element fires a trigger that inserts a row; `only_if` over `payload.value`
      filters; a trigger slower than its stream drops rather than queues; the cascade bound
      still holds from a stream-originated event.

## Phase 6 — Admin API and the observe socket

- [x] 6.1 `sc-api/src/admin.rs`: `listStreamProviders` (each provider's `config_spec`, and
      `element_type` resolved against a `?configuration=` when one is given — `listModelProviders`'
      arrangement, for its reason), `listStreams`, `getStream`, `saveStream`, `deleteStream`,
      `streamStatus`. The empty-provider-list case is an object with a sentence, not a bare
      array.
- [x] 6.2 Handlers in `sc-server/src/handlers.rs`, with the `*_json` / `*_from_body` pair every
      backup-able record has. Save and delete call the supervisor's `reload` afterwards, so the
      flow follows the row without a restart.
- [x] 6.3 `GET /api/streams/{id}/observe` in `router.rs`: admin-only, decided before the upgrade
      and refused with a status (the chat socket's rule, cited); then `ready` (element type +
      status), the ring replay, `element`, `lagged` and `status` frames. The ring buffer (last
      100) lives on the running stream.
- [x] 6.4 The trigger endpoints offer the stream list for a `stream` event's channel and refuse
      an unknown name (§8).
- [x] 6.5 `sc-server/tests/streams_admin_api.rs`: CRUD, validation refusals, redaction of a
      secret in a read, the delete refusal with a trigger, and the socket (unauthenticated
      refused; authenticated gets `ready`, replay and live elements).

## Phase 7 — The admin UI

- [x] 7.1 `ui/admin/src/client.ts` regenerated for the new endpoints (checked-in artifact +
      drift test), and the socket's frame types written beside the chat's. (The client was
      regenerated with the endpoints in 6.1; the frames are in `streams.ts`, and a Rust test pins
      the route both ends spell.)
- [x] 7.2 `ui/admin/src/streams.ts`: the pure parts — the form's fields from the picked
      provider's spec, the status label and colour, the counters' formatting, the envelope →
      table/text/hex rendering per element type. Unit tests (`streams.test.ts`).
- [x] 7.3 `ui/admin/src/screens/Streams.tsx`: the list (New at the top; Edit, Observe, Delete per
      row, with status and counters), the form, and the Observe screen (live tail, pause, clear,
      the lagged notice, the "since this server started" label). (Three files, as
      `LlmProviders`/`LlmProviderForm` already split: `Streams.tsx`, `StreamForm.tsx`,
      `StreamObserve.tsx`.)
- [x] 7.4 The **Streams** entry in `NAV` between Triggers and Files, with an icon in `icons.tsx`
      and the comment saying why it sits there (§9).
- [x] 7.5 The trigger form's event picker offers Stream, and its channel box becomes the stream
      picker for that event.

## Phase 8 — Applications

- [x] 8.1 `StreamRef` and `streams: Vec<StreamRef>` on `Application`, `with_stream`,
      `exposes_stream`, and the application form's picker — `TriggerRef`'s shape, word for word.
- [x] 8.2 `GET {mount}/streams/{name}/observe` mounted beside the endpoint set: the app session
      cookie, the stream's `min_role`, refusals for unknown, unexposed and unauthorised, and the
      same frame protocol as the admin socket.
- [x] 8.3 `generate_client`: `observeStream_{name}()` per exposed stream, typed from the
      element type, emitted into the app's source tree at build time. A test per `ElementType`
      over the emitted TypeScript.
- [x] 8.4 `sc-server/tests/app_streams.rs`: an app that exposes one observes it; one that does
      not gets a refusal; a below-`min_role` user gets a refusal; the generated client compiles
      (the existing client-emission test's harness).

## Phase 9 — Module stream providers

- [x] 9.1 `StreamProviderKind` / `StreamProviderHost` in `sc-stream`, and `PollingProvider`: the
      interval loop, the opaque cursor, elements validated against the declared element type, and
      a poll that throws leaving the supervisor to back off rather than spinning.
- [x] 9.2 `ModuleHost::stream_poll` and `stream_element_type`, and
      `sc-module/src/stream_providers.rs` (`ModuleStreamProviders`), built whole on every module
      change and routed to the module's worker — `ModuleModelProviders`' shape.
- [x] 9.3 `sc-server` registers module providers into the registry at boot and on module change,
      and a module change reloads the supervisor (a stream whose provider went away becomes
      `failed` with a sentence naming the module, not a panic).
- [x] 9.4 An example provider in `plugins/rss` (a polled feed) and a test that a stream over it
      delivers elements — the offline half, with a local file as the feed.

## Phase 10 — Documentation

- [ ] 10.1 `docs/TECHNICAL_DESIGN.md`: a new **§14.3 Streams** beside models and files (the
      entity, the provider seam, the element type, the supervisor, delivery and the no-blocking
      rule, MQTT, module providers, and the one-process limitation), the `_fd_streams` row in
      §9's table, the entity in §9.2's relationships, `StreamProvider` in §2.1's extension-point
      table, `sc-stream` in §2's crate tree, `EventKind::Stream` in §10.2, the app socket in
      §13.2, and the crate's layer in §3.
- [ ] 10.2 `docs/tutorial-streams.md`: create an MQTT stream, observe it, fire a trigger that
      stores an element, expose it to an application and read it from the client — with the
      mosquitto recipe (4.5) and the honest note about two servers subscribing twice.
- [ ] 10.3 The CHANGELOG entry for the milestone, and a walk of the definition of done against a
      real broker recorded there (the parts `cargo test` cannot assert).

---

## Explicitly OUT of scope for this milestone

- **Storing elements.** §4: a flow is made durable by a trigger that writes a row. A retention
  window would be a second, worse table with no schema anybody chose.
- **Push subscriptions from a module.** §12: a Deno worker has no channel back into the host.
  Polling covers the feed-shaped cases, which is most of them.
- **Producing to a stream** (an action that publishes to MQTT). GOALS says "observing the
  elements of the stream"; a `publish` action is a natural sibling and a different feature, and
  it belongs with the other outbound actions rather than here.
- **A stream as a table provider** ("the last value per topic, as rows"). Tempting, and it is a
  materialisation policy — the same `None | Snapshot | Synced` question §8.3 already defers.
- **Cross-process coordination.** §6: one process, one subscription, until `sc-bus` exists.
- **Streams in a backup.** Models are not in one either; when the backup grows to cover the
  entities added since it was written, it covers both, and doing one without the other just moves
  the asymmetry.
- **The admin copilot learning streams.** `admin_copilot` can create tables and triggers; a
  stream is a third noun for it, and it wants the tool surface designed rather than extended in
  passing.
- **Backfill or replay.** A subscription starts where it starts. MQTT's retained messages are the
  only "before you connected" this milestone delivers, and only because the broker sends them.

## Carried past this milestone

- From TODO-post-mvp-26: the two items `cargo test` cannot do — running the agent eval against a
  real provider (11.4) and walking the agent milestone's definition of done by hand (12.3). Both
  need an API key and spend money; `docs/AGENT_EVAL.md` has the command and the heading the
  numbers go under.
- From TODO-post-mvp-25: page groups, HTML-file pages, copilot layout generation, uploading from
  the builder, v1's help topics, formula-editor completions, replacing CKEditor 4, a menu editor,
  cloning pages and views, sharing library items, collaborative editing, builder i18n, and the
  builder in a plugin pattern's mode.
- From TODO-post-mvp-24: `room`/`workflow-room` and realtime, tags, file upload from an Edit
  view, themes as plugins, i18n, a v1 `db` module for plugins, and externalising inline handlers
  to drop `'unsafe-inline'` from Saltcorn UI's CSP.
