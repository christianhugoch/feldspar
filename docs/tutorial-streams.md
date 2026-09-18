# Tutorial: Streams — checking the MQTT provider against a real broker

Everything else Saltcorn holds is at rest: a table has rows, a file has bytes, a model has a
fit. A **stream** is the thing that moves — a temperature sensor publishing to a broker, a
market feed, a queue of jobs from another system. You create one from a **stream provider**,
fill in the settings it declares, and from then on its elements can be watched in the admin UI,
handed to a trigger, or read by an application over a WebSocket.

Saltcorn has one built-in stream provider, and it is the one GOALS names: **MQTT**.

> **This page is the broker half.** The tests that ship with Saltcorn cover the MQTT provider's
> settings, its element type and its payload decoder offline — no broker, no container, no
> network. What no test can assert is that a real broker's publishes arrive, so that part is a
> recipe you run by hand, and it is what this page is for. The full walkthrough — creating the
> stream on the Streams screen, watching it on Observe, firing a trigger from it and reading it
> from an application's client — arrives with those screens.

## Step 1 — A broker, in one command

[Mosquitto](https://mosquitto.org) is the small one, and it needs no configuration file to
listen on the loopback:

```
docker run --rm -it -p 1883:1883 eclipse-mosquitto:2 \
  mosquitto -c /mosquitto-no-auth.conf
```

That image ships two configurations; `/mosquitto-no-auth.conf` is the one that accepts anonymous
connections on `1883`, which is what you want on your own machine and nowhere else. If you would
rather not use Docker, `apt install mosquitto mosquitto-clients` gives you the same broker as a
service on `localhost:1883` and the two command-line tools below.

Leave it running in its own terminal. Everything after this is in another one.

## Step 2 — Publish something, and prove the broker works

`mosquitto_pub` is the publisher (`apt install mosquitto-clients`, or
`docker run --rm -it --network host eclipse-mosquitto:2 mosquitto_pub …`):

```
mosquitto_pub -h localhost -t house/boiler/temp -m '{"temperature": 31.2, "unit": "C"}'
```

Before pointing Saltcorn at it, watch it with `mosquitto_sub` in a third terminal, so that a
stream which shows nothing later is a question about Saltcorn rather than about the broker:

```
mosquitto_sub -h localhost -t 'house/+/temp' -v
```

Publish again and the subscriber prints the topic and the payload. That `house/+/temp` is a
**topic filter**, and it is the same string Saltcorn wants: `+` matches exactly one level,
`#` matches every level after it and can only be the last one.

## Step 3 — The stream

Create a stream from the `mqtt` provider with these settings:

| Setting | Value |
| --- | --- |
| Broker host | `localhost` |
| Port | `1883` |
| Connect over TLS | off |
| Client id | *(blank)* |
| Username / Password | *(blank)* |
| Topic filter | `house/+/temp` |
| Quality of service | `0` |
| Start a clean session | on |
| Payload | `json` |
| Declared keys | `temperature` (float, required), `unit` (text) |

Three of those are worth a sentence.

**The payload setting is what the element type is.** The same broker and the same filter are a
stream of objects with declared keys, a stream of text or a stream of bytes depending only on
this one picker — which is why the provider computes the element type from the configuration
rather than having one. With `json` you list the keys and their types, and that list is what the
Observe screen's columns, a trigger's `payload.value.temperature` and an application's generated
TypeScript are all built from. A `json` stream that declares no keys is refused when you save
it, because an element with no declared shape has nothing for any of those three to read.

**A blank client id is not a random one.** Saltcorn uses `feldspar-{the stream's name}`, and it
is stable on purpose: a client that invents a new id on every reconnect leaves the broker holding
a session per attempt, so a stream that flaps for a day becomes a broker nobody can administer.
Set one explicitly if your broker's access control keys off it.

**A clean session is the default**, and it means the broker does not hand over everything
published while the stream was down. That is deliberate: `received_at` on each element is when
*this server* saw it, so a resumed session delivers an hour of readings all stamped with the
moment the connection came back. What you do get from before you connected is a **retained**
message, because the broker sends those to every new subscriber — `mosquitto_pub -r` publishes
one, and it is a good way to see an element arrive the instant a stream starts.

## Step 4 — Watch it

Publish a few:

```
mosquitto_pub -h localhost -t house/boiler/temp -m '{"temperature": 31.2, "unit": "C"}'
mosquitto_pub -h localhost -t house/tank/temp   -m '{"temperature": 58.0, "unit": "C"}'
mosquitto_pub -h localhost -t house/boiler/temp -m '{"temperature": 30.9}'
```

All three arrive. The third declares no `unit`, and because that key is not required the element
carries `"unit": null` rather than being refused — a declared shape is a floor on what an
element has, not a ceiling, so an extra key a publisher adds later is carried through too.

Now send something that is not what the stream declared:

```
mosquitto_pub -h localhost -t house/heartbeat/temp -m 'ok'
mosquitto_pub -h localhost -t house/boiler/temp -m '{"temperature": "warm"}'
```

Neither is delivered. Both are counted — the stream's `malformed` counter goes up, and that
counter is on the Streams list — and the server log says so **once a minute at most**, with the
number it held back:

```
feldspar: stream `boiler`: a payload on `house/heartbeat/temp` does not match the declared
element type and was not delivered: a json element's payload is not JSON: expected value at
line 1 column 1
```

This is the case that matters on a wildcard filter, and it is not even misbehaviour: `house/#`
matching four sensors and one heartbeat string is an ordinary thing to write. A publisher sending
the wrong shape at 50 Hz is one configuration mistake, not fifty log lines a second, and the
suppressed count in the next minute's line is what tells you which of the two you have.

## Step 5 — Stop the broker

In the broker's terminal, press `Ctrl-C`. The stream's status goes to `failed` with the reason,
and its attempt count starts climbing: Saltcorn retries with a doubling delay capped at a
minute, for ever. Start the broker again and the stream is `running` within a minute, without a
restart and without touching the row — and the subscription is re-sent, which is why the
reconnection is Saltcorn's job rather than the MQTT client's.

## What to know before you point this at something real

- **One process, one subscription.** Two Saltcorn servers against one database both subscribe, so
  a trigger on the stream fires twice. That is a real limitation of this milestone, not a bug to
  find later. MQTT's own shared subscriptions are the escape hatch: a filter of
  `$share/feldspar/house/+/temp` asks the broker to give each element to exactly one member of
  the group, and Saltcorn accepts that filter as it stands.
- **Elements are not stored.** A stream is a flow, and nothing keeps its elements: the Observe
  screen's history is a small in-memory ring, labelled "since this server started". What makes a
  stream durable is a trigger that writes a row — which is a table you can query, back up and
  give away.
- **TLS is `use_tls` plus the right port**, conventionally `8883`. Saltcorn verifies the broker
  against the machine's own root certificates, with the same rustls the HTTPS listener uses; it
  does not turn verification off, and there is no setting that does.
- **The password is a secret**, so it is redacted wherever the stream is read back and an edit
  that does not retype it keeps the stored one.
