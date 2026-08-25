// Triggers list: every stored trigger, what fires it, what it runs, and whether
// it can run at all (§10.2).
//
// Two things this screen exists to make visible, both of them states a list of
// names could not show:
//
//   - A trigger can be **stored but not usable** — its table dropped, its action
//     provided by a plugin that is gone, its formula no longer resolving. It is
//     not in the live set and will not fire, but it is still here, with the
//     reason, and still editable, because editing it is the repair.
//   - A trigger with **no intrinsic event** (`none`) only ever runs because
//     something asks it to, so this is where the asking happens: Run posts a
//     payload and shows the action's result — or its error, which is the point
//     of testing one.

import { useEffect, useState } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Table from "react-bootstrap/Table";

import { api, errorMessage } from "../api";
import type { ListTriggersResponse } from "../client";
import { navigate } from "../App";
import { IconPlus } from "../icons";
import { AlertBody, PageBody, PageHeader, StatusBadge } from "../layout";

type TriggerItem = ListTriggersResponse[number];

/** What one trigger runs, in one line: the action and the table it targets, if
 * its configuration names one. Deliberately generic — the screen knows no
 * action's settings, so it shows the `table` setting when there is one rather
 * than reaching into a particular action's shape. */
/** Days as the server numbers them: 0 = Monday … 6 = Sunday. */
const DAYS = [
  "Monday",
  "Tuesday",
  "Wednesday",
  "Thursday",
  "Friday",
  "Saturday",
  "Sunday",
];

/** A periodic trigger's timing in words, or `null` for a kind that has none.
 *
 * Read from the same three fields the form writes, so what the list says and
 * what the scheduler does come from one description. Everything is UTC (§10.2's
 * decision 6), and the label says so — an admin who reads "03:00" and assumes
 * local time is an admin whose nightly job runs at the wrong hour. */
function scheduleSummary(trigger: TriggerItem): string | null {
  const minute = trigger.minute ?? 0;
  const hour = trigger.hour ?? 0;
  const pad = (n: number) => String(n).padStart(2, "0");
  switch (trigger.when) {
    case "often":
      return "every 5 minutes";
    case "hourly":
      return `at :${pad(minute)} past the hour`;
    case "daily":
      return `at ${pad(hour)}:${pad(minute)} UTC`;
    case "weekly":
      return `${DAYS[trigger.day_of_week ?? 0]} at ${pad(hour)}:${pad(minute)} UTC`;
    default:
      return null;
  }
}

/** The last run as a local, readable time — or a dash for one that has never
 * run, which for a scheduled trigger is itself worth seeing. */
function lastRun(trigger: TriggerItem): string {
  if (!trigger.last_run_at) return "—";
  const at = new Date(trigger.last_run_at);
  return Number.isNaN(at.getTime()) ? trigger.last_run_at : at.toLocaleString();
}

export function targetSummary(trigger: TriggerItem): string {
  // A workflow body runs a program rather than an action, and has no
  // configuration to read a table out of (§10.3). "workflow" alone tells one
  // trigger from another not at all, so the list says how big the program is and
  // which version is live — the two facts that make a row identifiable.
  const action = trigger.action ?? "";
  if (trigger.body === "workflow") {
    if (trigger.workflow_version == null) return "workflow";
    const steps = trigger.workflow_steps ?? 0;
    return `workflow · ${steps} step${steps === 1 ? "" : "s"} · v${trigger.workflow_version}`;
  }
  const config = trigger.configuration;
  if (!config || typeof config !== "object") return action;
  const table = (config as Record<string, unknown>).table;
  return typeof table === "string" && table !== ""
    ? `${action} → ${table}`
    : action;
}

export function Triggers() {
  const [triggers, setTriggers] = useState<TriggerItem[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** The result of the last Run, or its failure — shown until the next one. */
  const [ran, setRan] = useState<{ name: string; result: string } | null>(null);

  const load = async () => {
    try {
      setTriggers(await api.listTriggers());
    } catch {
      setError("Could not load triggers.");
    }
  };

  useEffect(() => {
    void load();
  }, []);

  const remove = async (trigger: TriggerItem) => {
    if (
      !window.confirm(
        `Delete the trigger "${trigger.name}"?\n\n` +
          "Its configuration is deleted with it. Switch it off instead if you " +
          "only want it to stop firing.",
      )
    ) {
      return;
    }
    setError(null);
    try {
      await api.deleteTrigger(trigger.id);
      await load();
    } catch (err) {
      setError(errorMessage(err, "Could not delete the trigger."));
    }
  };

  const run = async (trigger: TriggerItem) => {
    setError(null);
    setRan(null);
    try {
      const { result } = await api.runTrigger(trigger.id, {});
      setRan({ name: trigger.name, result: JSON.stringify(result, null, 2) });
      // A workflow body does not return a value: it answers a run id and its
      // state (§10.3, phase 3.4), and what an admin wants next is to watch it.
      if (trigger.body === "workflow") navigate(`/triggers/${encodeURIComponent(trigger.id)}/runs`);
    } catch (err) {
      // The action's own failure, which is what a test run is for — surfaced as
      // the error it is, not as a result that happens to be empty.
      setError(errorMessage(err, `Running "${trigger.name}" failed.`));
    }
  };

  return (
    <>
      <PageHeader
        pretitle="Automation"
        title="Triggers"
        actions={
          <Button onClick={() => navigate("/triggers/new")}>
            <IconPlus className="icon-2" />
            New trigger
          </Button>
        }
      />
      <PageBody>
        {error && <Alert variant="danger">{error}</Alert>}
        {ran && (
          <Alert variant="success" onClose={() => setRan(null)} dismissible>
            <AlertBody>
              <div className="mb-1">
                <strong>{ran.name}</strong> ran. Result:
              </div>
              <pre className="mb-0 small text-break text-pre-wrap">{ran.result}</pre>
            </AlertBody>
          </Alert>
        )}

        <div className="card">
          <Table hover responsive className="card-table table-vcenter">
            <thead>
              <tr>
                <th>Name</th>
                <th>Event</th>
                <th>Runs</th>
                <th>Last run</th>
                <th>Status</th>
                <th className="text-end">Actions</th>
              </tr>
            </thead>
            <tbody>
              {triggers?.length === 0 && (
                <tr>
                  <td colSpan={6} className="text-muted">
                    No triggers yet.
                  </td>
                </tr>
              )}
              {triggers?.map((trigger) => (
                <tr key={trigger.id}>
                  <td>
                    {trigger.name}
                    {trigger.description && (
                      <div className="text-muted small">{trigger.description}</div>
                    )}
                  </td>
                  <td>
                    {trigger.when}
                    {trigger.channel && (
                      <div className="text-muted small">on {trigger.channel}</div>
                    )}
                    {scheduleSummary(trigger) && (
                      <div className="text-muted small">{scheduleSummary(trigger)}</div>
                    )}
                    {trigger.only_if && (
                      <div className="text-muted small font-monospace text-break">
                        if {trigger.only_if}
                      </div>
                    )}
                  </td>
                  <td className="text-break">{targetSummary(trigger)}</td>
                  {/* Only a periodic trigger has a schedule to have missed, so
                      it is the only one where "when did this last run?" is a
                      question the list can answer usefully. */}
                  <td className="small">
                    {scheduleSummary(trigger) ? lastRun(trigger) : ""}
                  </td>
                  <td>
                    <StatusCell trigger={trigger} />
                  </td>
                  <td className="text-end">
                    <div className="btn-list justify-content-end flex-nowrap">
                      <Button
                        size="sm"
                        variant="outline-secondary"
                        href={`#/triggers/${encodeURIComponent(trigger.id)}/edit`}
                      >
                        Edit
                      </Button>
                      {/* A workflow's steps are not on the trigger form: they
                          are a version of their own, drawn on a canvas, and its
                          runs are durable things an admin comes looking for
                          (§10.3). Both hang off the trigger's id. */}
                      {trigger.body === "workflow" && (
                        <>
                          <Button
                            size="sm"
                            variant="outline-secondary"
                            href={`#/triggers/${encodeURIComponent(trigger.id)}/workflow`}
                          >
                            Steps
                          </Button>
                          <Button
                            size="sm"
                            variant="outline-secondary"
                            href={`#/triggers/${encodeURIComponent(trigger.id)}/runs`}
                          >
                            Runs
                          </Button>
                        </>
                      )}
                      {/* Only a `none` trigger is meaningful to run by hand:
                          every other kind needs its own occurrence (a row, a
                          login) to say anything about, and would fail on the
                          row it does not have. */}
                      {trigger.when === "none" && (
                        <Button
                          size="sm"
                          variant="outline-primary"
                          disabled={!!trigger.error || !trigger.enabled}
                          onClick={() => void run(trigger)}
                        >
                          Run
                        </Button>
                      )}
                      <Button
                        size="sm"
                        variant="outline-danger"
                        onClick={() => void remove(trigger)}
                      >
                        Delete
                      </Button>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </Table>
        </div>
      </PageBody>
    </>
  );
}

/** Usable, switched off, or broken with the reason — the state that makes a
 * trigger that is not firing fixable instead of merely puzzling. */
function StatusCell({ trigger }: { trigger: TriggerItem }) {
  if (trigger.error) {
    return (
      <>
        <StatusBadge tone="red">Not usable</StatusBadge>
        <div className="text-danger small text-break mt-1">{trigger.error}</div>
      </>
    );
  }
  if (!trigger.enabled) {
    return <StatusBadge tone="secondary">Off</StatusBadge>;
  }
  return <StatusBadge tone="green">Enabled</StatusBadge>;
}
