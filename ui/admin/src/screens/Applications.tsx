// Applications list: every application in `_sc_applications`, with the actions
// that make one real — build (+mount) it, edit its configuration, delete it —
// and a link to each app's own subdomain.
//
// An application is stored configuration; building it is a separate, slow step
// that mounts it live (design §13.2). The server keeps no persisted build status,
// so a freshly loaded (or freshly created) app shows as **not built yet** rather
// than pretending a save deployed anything; a successful build flips it to built
// for this session, and a failed build surfaces the bundler's own diagnostics.

import { useEffect, useState } from "react";
import Alert from "react-bootstrap/Alert";
import Badge from "react-bootstrap/Badge";
import Button from "react-bootstrap/Button";
import Table from "react-bootstrap/Table";

import { api, errorMessage } from "../api";
import type { ListApplicationsResponse } from "../client";
import { navigate } from "../App";

type AppItem = ListApplicationsResponse[number];

/** Per-app build state, tracked client-side (the server persists none). */
type BuildStatus = "unbuilt" | "building" | "built" | "failed";

/** The URL an app is served at: `<subdomain>.<the admin's host>`. The admin runs
 * on the base domain, so its own host (with port) is what the subdomain sits on —
 * `blog.example.com` or, in local dev, `blog.localhost:3000`. */
function appUrl(subdomain: string): string {
  return `${window.location.protocol}//${subdomain}.${window.location.host}`;
}

export function Applications() {
  const [apps, setApps] = useState<AppItem[] | null>(null);
  const [status, setStatus] = useState<Record<string, BuildStatus>>({});
  const [error, setError] = useState<string | null>(null);
  // The most recent build's outcome, surfaced so the bundler's log/diagnostics
  // are visible rather than buried in a per-row badge.
  const [outcome, setOutcome] = useState<{
    ok: boolean;
    app: string;
    text: string;
  } | null>(null);

  const load = async () => {
    try {
      setApps(await api.listApplications());
    } catch {
      setError("Could not load applications.");
    }
  };

  useEffect(() => {
    void load();
  }, []);

  const build = async (app: AppItem) => {
    setStatus((s) => ({ ...s, [app.id]: "building" }));
    setOutcome(null);
    try {
      const report = await api.buildApplication(app.id);
      setStatus((s) => ({ ...s, [app.id]: "built" }));
      setOutcome({
        ok: true,
        app: app.name,
        text: report.log.trim() || "Build succeeded.",
      });
    } catch (err) {
      setStatus((s) => ({ ...s, [app.id]: "failed" }));
      setOutcome({
        ok: false,
        app: app.name,
        text: errorMessage(err, "The build failed."),
      });
    }
  };

  const remove = async (app: AppItem) => {
    if (!window.confirm(`Delete application "${app.name}"? This cannot be undone.`)) {
      return;
    }
    setError(null);
    try {
      await api.deleteApplication(app.id);
      await load();
    } catch (err) {
      setError(errorMessage(err, "Could not delete the application."));
    }
  };

  return (
    <>
      <div className="d-flex justify-content-between align-items-center mb-4">
        <h1 className="h3 mb-0">Applications</h1>
        <Button onClick={() => navigate("/applications/new")}>New application</Button>
      </div>

      {error && <Alert variant="danger">{error}</Alert>}
      {outcome && (
        <Alert
          variant={outcome.ok ? "success" : "danger"}
          onClose={() => setOutcome(null)}
          dismissible
        >
          <Alert.Heading className="h6">
            {outcome.ok ? "Build succeeded" : "Build failed"} — {outcome.app}
          </Alert.Heading>
          <pre className="mb-0 text-break" style={{ whiteSpace: "pre-wrap" }}>
            {outcome.text}
          </pre>
        </Alert>
      )}

      <Table hover responsive>
        <thead>
          <tr>
            <th>Name</th>
            <th>Subdomain</th>
            <th>Framework</th>
            <th>Build</th>
            <th className="text-end">Actions</th>
          </tr>
        </thead>
        <tbody>
          {apps?.length === 0 && (
            <tr>
              <td colSpan={5} className="text-muted">
                No applications yet.
              </td>
            </tr>
          )}
          {apps?.map((app) => {
            const state = status[app.id] ?? "unbuilt";
            return (
              <tr key={app.id}>
                <td>
                  {app.name}
                  {app.description && (
                    <div className="text-muted small">{app.description}</div>
                  )}
                </td>
                <td>
                  <a href={appUrl(app.subdomain)} target="_blank" rel="noreferrer">
                    {app.subdomain}
                  </a>
                </td>
                <td>{app.framework.name}</td>
                <td>
                  <BuildBadge status={state} />
                </td>
                <td className="text-end">
                  <Button
                    size="sm"
                    variant="outline-secondary"
                    className="me-2"
                    href={`#/applications/${encodeURIComponent(app.id)}/edit`}
                  >
                    Edit
                  </Button>
                  <Button
                    size="sm"
                    variant="outline-primary"
                    className="me-2"
                    disabled={state === "building"}
                    onClick={() => void build(app)}
                  >
                    {state === "building" ? "Building…" : "Build"}
                  </Button>
                  <Button
                    size="sm"
                    variant="outline-danger"
                    onClick={() => void remove(app)}
                  >
                    Delete
                  </Button>
                </td>
              </tr>
            );
          })}
        </tbody>
      </Table>
    </>
  );
}

/** The per-app build state as a coloured badge, including the saved-but-unbuilt
 * state a newly created (or freshly reloaded) app shows. */
function BuildBadge({ status }: { status: BuildStatus }) {
  switch (status) {
    case "built":
      return <Badge bg="success">Built</Badge>;
    case "building":
      return <Badge bg="info">Building…</Badge>;
    case "failed":
      return <Badge bg="danger">Build failed</Badge>;
    default:
      return <Badge bg="secondary">Not built yet</Badge>;
  }
}
