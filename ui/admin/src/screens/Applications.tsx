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
import Button from "react-bootstrap/Button";
import Table from "react-bootstrap/Table";

import { api, errorMessage } from "../api";
import type { ListApplicationsResponse } from "../client";
import { ideUrl, navigate } from "../App";
import { IconPlus } from "../icons";
import { AlertBody, PageBody, PageHeader, StatusBadge } from "../layout";
import { takeNotice, type Notice } from "../notice";

type AppItem = ListApplicationsResponse[number];

/** Per-app build state, tracked client-side (the server persists none). */
type BuildStatus = "unbuilt" | "building" | "built" | "failed";

/** The URL an app is served at: `<subdomain>.<the admin's host>`. The admin runs
 * on the base domain, so its own host (with port) is what the subdomain sits on —
 * `blog.example.com` or, in local dev, `blog.localhost:3000`. */
function appUrl(subdomain: string): string {
  return `${window.location.protocol}//${subdomain}.${window.location.host}`;
}

/** The file-manager route for a directory in a store. */
function filesUrl(store: string, path: string): string {
  const dir = path ? `/${path.split("/").map(encodeURIComponent).join("/")}` : "";
  return `#/files/${encodeURIComponent(store)}${dir}`;
}

export function Applications() {
  const [apps, setApps] = useState<AppItem[] | null>(null);
  const [status, setStatus] = useState<Record<string, BuildStatus>>({});
  const [error, setError] = useState<string | null>(null);
  // The most recent outcome, surfaced so a tool's log or diagnostics are visible
  // rather than buried in a per-row badge. A build fills this in directly; a
  // scaffold happens on the form, which leaves its message for this screen to
  // pick up — one banner for both, because to an admin they are the same news
  // about the same app.
  const [outcome, setOutcome] = useState<Notice | null>(null);

  const load = async () => {
    try {
      setApps(await api.listApplications());
    } catch {
      setError("Could not load applications.");
    }
  };

  useEffect(() => {
    void load();
    setOutcome(takeNotice());
  }, []);

  const build = async (app: AppItem) => {
    setStatus((s) => ({ ...s, [app.id]: "building" }));
    setOutcome(null);
    try {
      const report = await api.buildApplication(app.id);
      setStatus((s) => ({ ...s, [app.id]: "built" }));
      setOutcome({
        ok: true,
        title: `Build succeeded — ${app.name}`,
        text: report.log.trim() || "Build succeeded.",
      });
    } catch (err) {
      setStatus((s) => ({ ...s, [app.id]: "failed" }));
      setOutcome({
        ok: false,
        title: `Build failed — ${app.name}`,
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
      <PageHeader
        pretitle="Deploy"
        title="Applications"
        actions={
          <Button onClick={() => navigate("/applications/new")}>
            <IconPlus className="icon-2" />
            New application
          </Button>
        }
      />
      <PageBody>
        {error && <Alert variant="danger">{error}</Alert>}
        {outcome && (
          <Alert
            variant={outcome.ok ? "success" : "danger"}
            onClose={() => setOutcome(null)}
            dismissible
          >
            <AlertBody>
              <Alert.Heading className="h6">{outcome.title}</Alert.Heading>
              <pre className="mb-0 text-break text-pre-wrap">{outcome.text}</pre>
            </AlertBody>
          </Alert>
        )}

        <div className="card">
          <Table hover responsive className="card-table table-vcenter">
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
                    <td>
                      {app.framework.name}
                      {app.source && (
                        // The loop an admin actually works in is edit-file →
                        // build → view, so the source directory is one click
                        // from the row. Where an app's source *is* comes from
                        // the server (§2.4), so this link works the same for a
                        // framework that states its paths and one that derives
                        // them.
                        <div className="small">
                          <a href={filesUrl(app.source.store, app.source.path)}>
                            {app.source.store}/{app.source.path || ""}
                          </a>{" "}
                          <a href={ideUrl(app.source.store)}>(edit code)</a>
                        </div>
                      )}
                    </td>
                    <td>
                      <BuildBadge status={state} />
                    </td>
                    <td className="text-end">
                      <div className="btn-list justify-content-end flex-nowrap">
                        <Button
                          size="sm"
                          variant="outline-secondary"
                          href={`#/applications/${encodeURIComponent(app.id)}/edit`}
                        >
                          Edit
                        </Button>
                        <Button
                          size="sm"
                          variant="outline-primary"
                          disabled={state === "building"}
                          onClick={() => void build(app)}
                        >
                          {state === "building" ? "Building…" : "Build"}
                        </Button>
                        <Button size="sm" variant="outline-danger" onClick={() => void remove(app)}>
                          Delete
                        </Button>
                      </div>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </Table>
        </div>
      </PageBody>
    </>
  );
}

/** The per-app build state as a coloured badge, including the saved-but-unbuilt
 * state a newly created (or freshly reloaded) app shows. */
function BuildBadge({ status }: { status: BuildStatus }) {
  switch (status) {
    case "built":
      return <StatusBadge tone="green">Built</StatusBadge>;
    case "building":
      return <StatusBadge tone="blue">Building…</StatusBadge>;
    case "failed":
      return <StatusBadge tone="red">Build failed</StatusBadge>;
    default:
      return <StatusBadge tone="secondary">Not built yet</StatusBadge>;
  }
}
