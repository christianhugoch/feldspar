// A Saltcorn UI application's Views and Pages tabs (TODO "Saltcorn UI" 9.2):
// what the application serves, each row with its pattern, table and role and a
// link that opens it on the app's subdomain, and delete.
//
// There is no Build button anywhere near this: a Saltcorn UI application's
// source is these rows, and a delete is live on the app's next request.
// Creating and configuring a view is Phase 10.

import { useEffect, useState } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Spinner from "react-bootstrap/Spinner";
import Table from "react-bootstrap/Table";

import { api, errorMessage } from "../api";
import { navigate } from "../App";
import { IconArrowLeft } from "../icons";
import { AlertBody, PageBody, PageHeader, StatusBadge } from "../layout";
import { useRoles } from "../roles";
import {
  NO_PAGES,
  NO_VIEWS,
  appTabs,
  deleteConfirmation,
  nameParam,
  pageRows,
  viewRows,
  type AppItem,
  type AppTab,
  type PageItem,
  type PatternItem,
  type ViewItem,
} from "../views";

/** The tabs across the top of an application's screens: Settings, and Views
 * and Pages for an application that has them. Links rather than state, because
 * each tab is its own route. Tabler's `.nav-pills`, as the Settings screen
 * uses, with nothing but classes doing the work under the admin's CSP. */
export function ApplicationTabs({ app, active }: { app: AppItem; active: AppTab }) {
  const tabs = appTabs(app);
  if (tabs.length < 2) return null;
  return (
    <ul className="nav nav-pills mb-3">
      {tabs.map((tab) => (
        <li className="nav-item" key={tab.id}>
          <a
            className={`nav-link${tab.id === active ? " active" : ""}`}
            aria-current={tab.id === active ? "page" : undefined}
            href={tab.href}
          >
            {tab.label}
          </a>
        </li>
      ))}
    </ul>
  );
}

export function ApplicationViews({ appId, tab }: { appId: string; tab: "views" | "pages" }) {
  const roles = useRoles();
  const [app, setApp] = useState<AppItem | null>(null);
  const [views, setViews] = useState<ViewItem[] | null>(null);
  const [pages, setPages] = useState<PageItem[] | null>(null);
  const [patterns, setPatterns] = useState<PatternItem[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    try {
      const found = (await api.listApplications()).find((a) => a.id === appId);
      if (!found) {
        setLoadError("That application no longer exists.");
        return;
      }
      setApp(found);
      const [v, p] = await Promise.all([api.listViews(appId), api.listPages(appId)]);
      setViews(v);
      setPages(p);
    } catch (err) {
      setLoadError(errorMessage(err, "Could not load the application's views and pages."));
    }
  };

  useEffect(() => {
    void load();
    // The patterns only annotate the list, so a server that cannot describe
    // them still lists the views.
    api
      .listViewPatterns()
      .then(setPatterns)
      .catch(() => setPatterns(null));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [appId]);

  const remove = async (kind: "view" | "page", name: string) => {
    if (!app || !window.confirm(deleteConfirmation(kind, name, app.name))) return;
    setError(null);
    try {
      if (kind === "view") {
        await api.deleteView(appId, nameParam(name));
      } else {
        await api.deletePage(appId, nameParam(name));
      }
      await load();
    } catch (err) {
      setError(errorMessage(err, `Could not delete the ${kind}.`));
    }
  };

  const header = (
    <PageHeader
      pretitle="Application"
      title={app?.name ?? "Application"}
      actions={
        <Button variant="outline-secondary" onClick={() => navigate("/applications")}>
          <IconArrowLeft className="icon-2" />
          Applications
        </Button>
      }
    />
  );

  if (loadError) {
    return (
      <>
        {header}
        <PageBody>
          <Alert variant="danger">{loadError}</Alert>
        </PageBody>
      </>
    );
  }
  if (!app || !views || !pages) {
    return (
      <>
        {header}
        <PageBody>
          <Spinner animation="border" role="status" />
        </PageBody>
      </>
    );
  }

  const here = window.location;

  return (
    <>
      {header}
      <PageBody>
        <ApplicationTabs app={app} active={tab} />
        {error && (
          <Alert variant="danger" dismissible onClose={() => setError(null)}>
            <AlertBody>{error}</AlertBody>
          </Alert>
        )}

        {tab === "views" ? (
          <div className="card">
            <Table hover responsive className="card-table table-vcenter">
              <thead>
                <tr>
                  <th>Name</th>
                  <th>Pattern</th>
                  <th>Table</th>
                  <th>Role</th>
                  <th className="text-end">Actions</th>
                </tr>
              </thead>
              <tbody>
                {views.length === 0 && (
                  <tr>
                    <td colSpan={5} className="text-muted">
                      {NO_VIEWS}
                    </td>
                  </tr>
                )}
                {viewRows(views, patterns, roles, app.subdomain, here).map((row) => (
                  <tr key={row.name}>
                    <td>
                      <a href={row.url} target="_blank" rel="noreferrer">
                        {row.name}
                      </a>
                      {row.description && <div className="text-muted small">{row.description}</div>}
                    </td>
                    <td>
                      {row.pattern}
                      {row.patternMissing && (
                        <div>
                          <StatusBadge tone="red">not on this server</StatusBadge>
                        </div>
                      )}
                    </td>
                    <td>{row.table}</td>
                    <td>{row.role}</td>
                    <td className="text-end">
                      <Button
                        size="sm"
                        variant="outline-danger"
                        onClick={() => void remove("view", row.name)}
                      >
                        Delete
                      </Button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </Table>
          </div>
        ) : (
          <div className="card">
            <Table hover responsive className="card-table table-vcenter">
              <thead>
                <tr>
                  <th>Name</th>
                  <th>Title</th>
                  <th>Role</th>
                  <th>Home page for</th>
                  <th className="text-end">Actions</th>
                </tr>
              </thead>
              <tbody>
                {pages.length === 0 && (
                  <tr>
                    <td colSpan={5} className="text-muted">
                      {NO_PAGES}
                    </td>
                  </tr>
                )}
                {pageRows(pages, roles, app.subdomain, here).map((row) => (
                  <tr key={row.name}>
                    <td>
                      <a href={row.url} target="_blank" rel="noreferrer">
                        {row.name}
                      </a>
                    </td>
                    <td>{row.title}</td>
                    <td>{row.role}</td>
                    <td>{row.homeFor.length ? row.homeFor.join(", ") : "—"}</td>
                    <td className="text-end">
                      <Button
                        size="sm"
                        variant="outline-danger"
                        onClick={() => void remove("page", row.name)}
                      >
                        Delete
                      </Button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </Table>
          </div>
        )}
      </PageBody>
    </>
  );
}
