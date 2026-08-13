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
import { IconArrowLeft } from "../icons";
import { PageBody, PageHeader } from "../layout";
import { OptionalRoleSelect } from "../roleSelect";
import { useRoles } from "../roles";
import { SettingsFields, buildConfig, readConfig } from "../settings";

type ActionInfo = ListActionsResponse[number];
type TriggerItem = ListTriggersResponse[number];

/** The event kinds, in the order the server lists them (`EVENT_KINDS`).
 *
 * Hard-coded because they are a fixed enum of the trigger model rather than a
 * registry: nothing can add one at runtime, which is exactly the difference from
 * the action list beside it (which *is* loaded from the server). The labels are
 * this screen's editorial job. */
const EVENT_KINDS: {
  value: string;
  label: string;
  table: boolean;
  /** Which timing inputs this kind takes — exactly the attributes the server's
   * `Schedule::of` reads for it. A kind that does not take one *refuses* it on
   * save, so offering it would be offering a field that cannot be saved. */
  timing: TimingField[];
}[] = [
  { value: "insert", label: "A row is inserted", table: true, timing: [] },
  { value: "update", label: "A row is updated", table: true, timing: [] },
  { value: "delete", label: "A row is deleted", table: true, timing: [] },
  {
    value: "none",
    label: "Only when something asks (no event)",
    table: false,
    timing: [],
  },
  { value: "login", label: "A user signs in", table: false, timing: [] },
  { value: "startup", label: "The server starts up", table: false, timing: [] },
  { value: "error", label: "An error is reported", table: false, timing: [] },
  { value: "often", label: "Every five minutes", table: false, timing: [] },
  { value: "hourly", label: "Once an hour", table: false, timing: ["minute"] },
  { value: "daily", label: "Once a day", table: false, timing: ["hour", "minute"] },
  {
    value: "weekly",
    label: "Once a week",
    table: false,
    timing: ["day_of_week", "hour", "minute"],
  },
];

/** The three timing inputs a periodic trigger can have. */
type TimingField = "minute" | "hour" | "day_of_week";

/** Days as the server numbers them: 0 = Monday … 6 = Sunday. The admin sees the
 * name, so the convention only has to be consistent. */
const DAYS = [
  "Monday",
  "Tuesday",
  "Wednesday",
  "Thursday",
  "Friday",
  "Saturday",
  "Sunday",
];

/** Whether a kind is about a table row — which is what decides whether the table
 * picker and the `only_if` field apply at all. */
function isTableEvent(kind: string): boolean {
  return EVENT_KINDS.find((k) => k.value === kind)?.table ?? false;
}

/** The timing inputs a kind takes; empty for everything that is not periodic. */
function timingFields(kind: string): TimingField[] {
  return EVENT_KINDS.find((k) => k.value === kind)?.timing ?? [];
}

/**
 * The trigger editor, for a new trigger or an existing one.
 *
 * `table` pre-selects the table a new trigger fires on — what "Create trigger"
 * on a table's own page means. It applies only to a new trigger: an existing
 * one's table is its own, and the form reads it from the stored trigger.
 */
export function TriggerForm({ triggerId, table }: { triggerId?: string; table?: string }) {
  const roles = useRoles();
  const [actions, setActions] = useState<ActionInfo[] | null>(null);
  const [tables, setTables] = useState<string[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [when, setWhen] = useState("insert");
  const [channel, setChannel] = useState(table ?? "");
  const [onlyIf, setOnlyIf] = useState("");
  const [actionName, setActionName] = useState("");
  const [config, setConfig] = useState<Record<string, string>>({});
  const [minRole, setMinRole] = useState<number | null>(null);
  const [enabled, setEnabled] = useState(true);
  // The periodic timing, held as strings so an empty box stays empty rather than
  // becoming a 0 the admin did not type.
  const [timing, setTiming] = useState<Record<TimingField, string>>({
    minute: "",
    hour: "",
    day_of_week: "",
  });

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
          setMinRole(existing.min_role ?? null);
          setEnabled(existing.enabled);
          setTiming({
            minute: existing.minute == null ? "" : String(existing.minute),
            hour: existing.hour == null ? "" : String(existing.hour),
            day_of_week:
              existing.day_of_week == null ? "" : String(existing.day_of_week),
          });
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
  const timingUsed = timingFields(when);

  /** One timing value for the save: null unless this kind uses it *and* the box
   * has something in it. Same rule the table and `only_if` follow — what is not
   * on the screen is not part of the save — and it matters more here, because
   * the server refuses a timing value on a kind that has no use for it. */
  const timingValue = (field: TimingField): number | null =>
    timingUsed.includes(field) && timing[field].trim() !== ""
      ? Number(timing[field])
      : null;

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
        min_role: minRole,
        enabled,
        minute: timingValue("minute"),
        hour: timingValue("hour"),
        day_of_week: timingValue("day_of_week"),
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
    return (
      <PageBody>
        <Alert variant="danger">{loadError}</Alert>
      </PageBody>
    );
  }
  if (!actions) {
    return (
      <PageBody>
        <div className="py-5 text-center">
          <Spinner animation="border" role="status" />
        </div>
      </PageBody>
    );
  }

  return (
    <>
      <PageHeader
        pretitle="Automation"
        title={triggerId ? "Edit trigger" : "New trigger"}
        actions={
          <Button variant="outline-secondary" onClick={() => navigate("/triggers")}>
            <IconArrowLeft className="icon-2" />
            Back
          </Button>
        }
      />
      <PageBody>
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
              <OptionalRoleSelect
                id="triggerMinRole"
                label="Minimum role"
                value={minRole}
                roles={roles}
                blank="Admin only"
                onChange={setMinRole}
              >
                Who may run this trigger through an application&apos;s API: the least
                privileged role still allowed.
              </OptionalRoleSelect>
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

              {timingUsed.length > 0 && (
                <Row>
                  {timingUsed.includes("day_of_week") && (
                    <Col md={4}>
                      <Form.Group className="mb-3" controlId="triggerDayOfWeek">
                        <Form.Label>Day</Form.Label>
                        <Form.Select
                          value={timing.day_of_week}
                          onChange={(e) =>
                            setTiming((t) => ({ ...t, day_of_week: e.target.value }))
                          }
                        >
                          {DAYS.map((day, index) => (
                            <option key={day} value={String(index)}>
                              {day}
                            </option>
                          ))}
                        </Form.Select>
                      </Form.Group>
                    </Col>
                  )}
                  {timingUsed.includes("hour") && (
                    <Col md={4}>
                      <Form.Group className="mb-3" controlId="triggerHour">
                        <Form.Label>Hour (UTC)</Form.Label>
                        <Form.Control
                          type="number"
                          min={0}
                          max={23}
                          placeholder="0"
                          value={timing.hour}
                          onChange={(e) =>
                            setTiming((t) => ({ ...t, hour: e.target.value }))
                          }
                        />
                      </Form.Group>
                    </Col>
                  )}
                  {timingUsed.includes("minute") && (
                    <Col md={4}>
                      <Form.Group className="mb-3" controlId="triggerMinute">
                        <Form.Label>Minute past the hour</Form.Label>
                        <Form.Control
                          type="number"
                          min={0}
                          max={59}
                          placeholder="0"
                          value={timing.minute}
                          onChange={(e) =>
                            setTiming((t) => ({ ...t, minute: e.target.value }))
                          }
                        />
                      </Form.Group>
                    </Col>
                  )}
                  <Col xs={12}>
                    <Form.Text muted>
                      Schedules are in <strong>UTC</strong>, so they mean the same
                      instant wherever the server runs and are not moved by daylight
                      saving. An empty box is 0.
                    </Form.Text>
                  </Col>
                </Row>
              )}

              {tableEvent && (
                <Form.Group className="mb-0" controlId="triggerOnlyIf">
                  <Form.Label>Only if</Form.Label>
                  <Form.Control
                    as="textarea"
                    rows={2}
                    className="font-monospace"
                    value={onlyIf}
                    placeholder="Example: pages > 100"
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
      </PageBody>
    </>
  );
}
