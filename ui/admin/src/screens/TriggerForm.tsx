// Create / edit a trigger: one event bound to one configured action (§10.2).
//
// The point this screen proves is the one the framework and file-store pickers
// prove: **no screen knows a specific action's settings**. The admin picks an
// action and the form renders whatever that action's `config_spec` declares, so
// an action added by a plugin gets a working configuration form with no change
// to this file.
//
// What is *not* generic is the event, and deliberately: which fields apply
// depends on the kind. A table event needs a table and can carry an `only_if`
// over the affected row; every other kind has no row, so both are hidden rather
// than offered and then refused on save. The server validates the same rules —
// this form shapes the question, it does not decide the answer, and a save it
// gets wrong comes back with the server's own message.

import { useEffect, useState, type FormEvent } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Card from "react-bootstrap/Card";
import Col from "react-bootstrap/Col";
import Form from "react-bootstrap/Form";
import Row from "react-bootstrap/Row";
import Spinner from "react-bootstrap/Spinner";

import { api, errorMessage } from "../api";
import type { ListActionsResponse, ListTriggersResponse } from "../client";
import { navigate } from "../App";
import { SettingsFields, buildConfig, readConfig } from "../settings";

type ActionInfo = ListActionsResponse[number];
type TriggerItem = ListTriggersResponse[number];

/** The event kinds, in the order the server lists them (`EVENT_KINDS`).
 *
 * Hard-coded because they are a fixed enum of the trigger model rather than a
 * registry: nothing can add one at runtime, which is exactly the difference from
 * the action list beside it (which *is* loaded from the server). The labels are
 * this screen's editorial job. */
const EVENT_KINDS: { value: string; label: string; table: boolean }[] = [
  { value: "insert", label: "A row is inserted", table: true },
  { value: "update", label: "A row is updated", table: true },
  { value: "delete", label: "A row is deleted", table: true },
  { value: "none", label: "Only when something asks (no event)", table: false },
  { value: "login", label: "A user signs in", table: false },
  { value: "startup", label: "The server starts up", table: false },
  { value: "error", label: "An error is reported", table: false },
  { value: "often", label: "Every five minutes", table: false },
  { value: "hourly", label: "Once an hour", table: false },
  { value: "daily", label: "Once a day", table: false },
  { value: "weekly", label: "Once a week", table: false },
];

/** Whether a kind is about a table row — which is what decides whether the table
 * picker and the `only_if` field apply at all. */
function isTableEvent(kind: string): boolean {
  return EVENT_KINDS.find((k) => k.value === kind)?.table ?? false;
}

export function TriggerForm({ triggerId }: { triggerId?: string }) {
  const [actions, setActions] = useState<ActionInfo[] | null>(null);
  const [tables, setTables] = useState<string[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [when, setWhen] = useState("insert");
  const [channel, setChannel] = useState("");
  const [onlyIf, setOnlyIf] = useState("");
  const [actionName, setActionName] = useState("");
  const [config, setConfig] = useState<Record<string, string>>({});
  const [minRole, setMinRole] = useState("");
  const [enabled, setEnabled] = useState(true);

  useEffect(() => {
    let cancelled = false;
    const run = async () => {
      try {
        const [actionList, tableList] = await Promise.all([
          api.listActions(),
          api.listTables(),
        ]);
        let existing: TriggerItem | undefined;
        if (triggerId) {
          existing = (await api.listTriggers()).find((t) => t.id === triggerId);
          if (!existing) {
            if (!cancelled) setLoadError("That trigger no longer exists.");
            return;
          }
        }
        if (cancelled) return;
        setActions(actionList);
        setTables(tableList.map((t) => t.name));
        if (existing) {
          setName(existing.name);
          setDescription(existing.description);
          setWhen(existing.when);
          setChannel(existing.channel ?? "");
          setOnlyIf(existing.only_if ?? "");
          setActionName(existing.action);
          setConfig(readConfig(existing.configuration));
          setMinRole(existing.min_role == null ? "" : String(existing.min_role));
          setEnabled(existing.enabled);
        } else {
          setActionName(actionList[0]?.name ?? "");
        }
      } catch {
        if (!cancelled) setLoadError("Could not load the actions and tables.");
      }
    };
    void run();
    return () => {
      cancelled = true;
    };
  }, [triggerId]);

  const action = actions?.find((a) => a.name === actionName);
  const spec = action?.config_spec ?? [];
  const tableEvent = isTableEvent(when);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const body = {
        name: name.trim(),
        description: description.trim(),
        when,
        // The fields a non-table event does not have are sent as null rather
        // than as the strings left over from a kind the admin changed away
        // from: what is not on the screen is not part of the save.
        channel: tableEvent && channel !== "" ? channel : null,
        only_if: tableEvent && onlyIf.trim() !== "" ? onlyIf.trim() : null,
        action: actionName,
        configuration: buildConfig(spec, config),
        min_role: minRole.trim() === "" ? null : Number(minRole),
        enabled,
      };
      if (triggerId) {
        await api.updateTrigger(triggerId, body);
      } else {
        await api.createTrigger(body);
      }
      navigate("/triggers");
    } catch (err) {
      // The server's own refusal — "no table named `books`", "`only if`:
      // unknown identifier `titel`" — is the message that says what to fix.
      setError(errorMessage(err, "Could not save the trigger."));
    } finally {
      setBusy(false);
    }
  };

  if (loadError) {
    return <Alert variant="danger">{loadError}</Alert>;
  }
  if (!actions) {
    return (
      <div className="py-5 text-center">
        <Spinner animation="border" role="status" />
      </div>
    );
  }

  return (
    <>
      <h1 className="h3 mb-4">{triggerId ? "Edit trigger" : "New trigger"}</h1>

      {error && <Alert variant="danger">{error}</Alert>}

      <Form onSubmit={(e) => void submit(e)}>
        <Row>
          <Col md={6}>
            <Form.Group className="mb-3" controlId="triggerName">
              <Form.Label>
                Name<span className="text-danger"> *</span>
              </Form.Label>
              <Form.Control
                value={name}
                required
                onChange={(e) => setName(e.target.value)}
              />
              <Form.Text muted>
                How everything else refers to this trigger — an application&apos;s API, a
                run button. Renaming it breaks those references deliberately.
              </Form.Text>
            </Form.Group>
          </Col>
          <Col md={6}>
            <Form.Group className="mb-3" controlId="triggerMinRole">
              <Form.Label>Minimum role</Form.Label>
              <Form.Control
                type="number"
                min={1}
                max={100}
                value={minRole}
                placeholder="admin only"
                onChange={(e) => setMinRole(e.target.value)}
              />
              <Form.Text muted>
                Who may run this trigger through an application&apos;s API. 1 is admin, 100
                is public. Leave blank for admin only.
              </Form.Text>
            </Form.Group>
          </Col>
        </Row>

        <Form.Group className="mb-3" controlId="triggerDescription">
          <Form.Label>Description</Form.Label>
          <Form.Control
            value={description}
            onChange={(e) => setDescription(e.target.value)}
          />
        </Form.Group>

        <Card className="mb-3">
          <Card.Header>When</Card.Header>
          <Card.Body>
            <Row>
              <Col md={tableEvent ? 6 : 12}>
                <Form.Group className="mb-3" controlId="triggerWhen">
                  <Form.Label>Event</Form.Label>
                  <Form.Select value={when} onChange={(e) => setWhen(e.target.value)}>
                    {EVENT_KINDS.map((kind) => (
                      <option key={kind.value} value={kind.value}>
                        {kind.label}
                      </option>
                    ))}
                  </Form.Select>
                </Form.Group>
              </Col>
              {tableEvent && (
                <Col md={6}>
                  <Form.Group className="mb-3" controlId="triggerChannel">
                    <Form.Label>
                      Table<span className="text-danger"> *</span>
                    </Form.Label>
                    <Form.Select
                      value={channel}
                      onChange={(e) => setChannel(e.target.value)}
                    >
                      <option value="">—</option>
                      {tables.map((table) => (
                        <option key={table} value={table}>
                          {table}
                        </option>
                      ))}
                    </Form.Select>
                  </Form.Group>
                </Col>
              )}
            </Row>

            {tableEvent && (
              <Form.Group className="mb-0" controlId="triggerOnlyIf">
                <Form.Label>Only if</Form.Label>
                <Form.Control
                  as="textarea"
                  rows={2}
                  className="font-monospace"
                  value={onlyIf}
                  placeholder="pages > 100"
                  onChange={(e) => setOnlyIf(e.target.value)}
                />
                <Form.Text muted>
                  A JavaScript expression over the affected row&apos;s fields,{" "}
                  <code>row</code>, <code>old</code> and <code>user</code>. The action runs
                  only when it is true. Leave blank to always run.
                </Form.Text>
              </Form.Group>
            )}
          </Card.Body>
        </Card>

        <Card className="mb-3">
          <Card.Header>Do</Card.Header>
          <Card.Body>
            <Form.Group className="mb-3" controlId="triggerAction">
              <Form.Label>Action</Form.Label>
              <Form.Select
                value={actionName}
                onChange={(e) => {
                  setActionName(e.target.value);
                  // A different action has different settings; carrying the old
                  // values over would post settings the new one never declared.
                  setConfig({});
                }}
              >
                {actions.map((a) => (
                  <option key={a.name} value={a.name}>
                    {a.name}
                  </option>
                ))}
              </Form.Select>
              {action && <Form.Text muted>{action.description}</Form.Text>}
            </Form.Group>

            {/* Rendered from the server's `config_spec`: this screen has no
                knowledge of any particular action's settings. */}
            <SettingsFields
              spec={spec}
              values={config}
              idPrefix="trigger-cfg"
              onChange={(field, value) =>
                setConfig((prev) => ({ ...prev, [field]: value }))
              }
            />
            <Form.Text muted>
              Values are formulas over the event: <code>row.title</code>,{" "}
              <code>user.email</code>, <code>payload.n</code>.
            </Form.Text>
          </Card.Body>
        </Card>

        <Form.Check
          type="checkbox"
          id="triggerEnabled"
          className="mb-3"
          label="Enabled"
          checked={enabled}
          onChange={(e) => setEnabled(e.target.checked)}
        />

        <div className="d-flex gap-2">
          <Button type="submit" disabled={busy}>
            {busy ? "Saving…" : "Save"}
          </Button>
          <Button
            variant="outline-secondary"
            disabled={busy}
            onClick={() => navigate("/triggers")}
          >
            Cancel
          </Button>
        </div>
      </Form>
    </>
  );
}
