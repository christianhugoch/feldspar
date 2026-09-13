// A Saltcorn UI application's Views and Pages tabs (TODO "Saltcorn UI" 9.2):
// what the application serves, each row with its pattern, table and role and a
// link that opens it on the app's subdomain, and delete.
//
// There is no Build button anywhere near this: a Saltcorn UI application's
// source is these rows, and a delete is live on the app's next request.
// Creating a view, opening its configuration and renaming it are Phase 10's;
// a rename says first what still refers to the view by its old name.

import { useEffect, useState } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Form from "react-bootstrap/Form";
import Modal from "react-bootstrap/Modal";
import Spinner from "react-bootstrap/Spinner";
import Table from "react-bootstrap/Table";

import { api, errorMessage } from "../api";
import { navigate } from "../App";
import { IconArrowLeft } from "../icons";
import { AlertBody, PageBody, PageHeader, StatusBadge } from "../layout";
import { RoleSelect } from "../roleSelect";
import { useRoles } from "../roles";
import {
  NO_PAGES,
  NO_VIEWS,
  appTabs,
  createViewBody,
  deleteConfirmation,
  nameParam,
  newViewError,
  pageRows,
  referencesReport,
  saveViewBody,
  viewEditorHref,
  viewRows,
  type AppItem,
  type AppTab,
  type Configuration,
  type NewViewForm,
  type PageItem,
  type PatternItem,
  type References,
  type ViewItem,
} from "../views";

/** The rename dialog: the view, the name being typed, and what refers to the
 * view — `null` while that is being found out. */
type Renaming = {
  view: ViewItem;
  name: string;
  references: References | null;
  error: string | null;
};

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
  const [creating, setCreating] = useState<NewViewForm | null>(null);
  const [createError, setCreateError] = useState<string | null>(null);
  const [renaming, setRenaming] = useState<Renaming | null>(null);
  const [busy, setBusy] = useState(false);

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

  const create = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!creating) return;
    const problem = newViewError(creating, patterns);
    if (problem) {
      setCreateError(problem);
      return;
    }
    setBusy(true);
    setCreateError(null);
    try {
      const created = await api.createView(appId, createViewBody(creating));
      setCreating(null);
      navigate(`/applications/${encodeURIComponent(appId)}/views/${encodeURIComponent(created.name)}`);
    } catch (err) {
      setCreateError(errorMessage(err, "Could not create the view."));
    } finally {
      setBusy(false);
    }
  };

  /** Open the rename dialog, and find out what refers to the view before any
   * name is changed (10.4). */
  const startRename = async (view: ViewItem) => {
    setRenaming({ view, name: view.name, references: null, error: null });
    try {
      const references = await api.viewReferences(appId, nameParam(view.name));
      setRenaming((r) => (r && r.view.name === view.name ? { ...r, references } : r));
    } catch (err) {
      const message = errorMessage(err, "Could not find what refers to the view.");
      setRenaming((r) => (r && r.view.name === view.name ? { ...r, error: message } : r));
    }
  };

  const rename = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!renaming) return;
    const { view, name } = renaming;
    setBusy(true);
    try {
      await api.saveView(
        appId,
        nameParam(view.name),
        saveViewBody(view, (view.configuration ?? {}) as Configuration, name.trim()),
      );
      setRenaming(null);
      await load();
    } catch (err) {
      setRenaming({ ...renaming, error: errorMessage(err, "Could not rename the view.") });
    } finally {
      setBusy(false);
    }
  };

  const header = (
    <PageHeader
      pretitle="Application"
      title={app?.name ?? "Application"}
      actions={
        <>
          {tab === "views" && app && (
            <Button
              onClick={() => {
                setCreateError(null);
                setCreating({
                  name: "",
                  description: "",
                  viewpattern: patterns?.[0]?.name ?? "List",
                  table_name: app.tables[0] ?? "",
                  min_role: 100,
                });
              }}
            >
              New view
            </Button>
          )}
          <Button variant="outline-secondary" onClick={() => navigate("/applications")}>
            <IconArrowLeft className="icon-2" />
            Applications
          </Button>
        </>
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
                    <td className="text-end text-nowrap">
                      <a className="btn btn-sm btn-outline-primary me-1" href={viewEditorHref(appId, row.name)}>
                        Configure
                      </a>
                      <Button
                        size="sm"
                        variant="outline-secondary"
                        className="me-1"
                        onClick={() => {
                          const view = views.find((v) => v.name === row.name);
                          if (view) void startRename(view);
                        }}
                      >
                        Rename
                      </Button>
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
        <Modal show={creating !== null} onHide={() => setCreating(null)}>
          <Form onSubmit={create}>
            <Modal.Header closeButton>
              <Modal.Title className="h4">New view</Modal.Title>
            </Modal.Header>
            <Modal.Body>
              {createError && <Alert variant="danger">{createError}</Alert>}
              {creating && (
                <>
                  <Form.Group className="mb-3" controlId="newViewName">
                    <Form.Label>Name</Form.Label>
                    <Form.Control
                      value={creating.name}
                      autoFocus
                      required
                      onChange={(e) => setCreating({ ...creating, name: e.target.value })}
                    />
                    <Form.Text muted>Also its address: /view/&lt;name&gt; on the app's subdomain.</Form.Text>
                  </Form.Group>
                  <Form.Group className="mb-3" controlId="newViewPattern">
                    <Form.Label>Pattern</Form.Label>
                    <Form.Select
                      value={creating.viewpattern}
                      onChange={(e) => setCreating({ ...creating, viewpattern: e.target.value })}
                    >
                      {(patterns ?? []).map((p) => (
                        <option key={p.name} value={p.name}>
                          {p.label || p.name}
                        </option>
                      ))}
                    </Form.Select>
                    {patterns?.find((p) => p.name === creating.viewpattern)?.description && (
                      <Form.Text muted>
                        {patterns.find((p) => p.name === creating.viewpattern)?.description}
                      </Form.Text>
                    )}
                  </Form.Group>
                  <Form.Group className="mb-3" controlId="newViewTable">
                    <Form.Label>Table</Form.Label>
                    <Form.Select
                      value={creating.table_name}
                      onChange={(e) => setCreating({ ...creating, table_name: e.target.value })}
                    >
                      <option value="">—</option>
                      {app.tables.map((t) => (
                        <option key={t} value={t}>
                          {t}
                        </option>
                      ))}
                    </Form.Select>
                    <Form.Text muted>One of the application's tables.</Form.Text>
                  </Form.Group>
                  <RoleSelect
                    id="newViewRole"
                    label="Minimum role"
                    roles={roles}
                    value={creating.min_role}
                    onChange={(min_role) => setCreating({ ...creating, min_role })}
                  />
                  <Form.Group className="mb-3" controlId="newViewDescription">
                    <Form.Label>Description</Form.Label>
                    <Form.Control
                      value={creating.description}
                      onChange={(e) => setCreating({ ...creating, description: e.target.value })}
                    />
                  </Form.Group>
                </>
              )}
            </Modal.Body>
            <Modal.Footer>
              <Button variant="secondary" type="button" onClick={() => setCreating(null)}>
                Cancel
              </Button>
              <Button type="submit" disabled={busy}>
                Create and configure
              </Button>
            </Modal.Footer>
          </Form>
        </Modal>

        <Modal show={renaming !== null} onHide={() => setRenaming(null)}>
          <Form onSubmit={rename}>
            <Modal.Header closeButton>
              <Modal.Title className="h4">Rename {renaming?.view.name}</Modal.Title>
            </Modal.Header>
            <Modal.Body>
              {renaming?.error && <Alert variant="danger">{renaming.error}</Alert>}
              {renaming && (
                <>
                  <Form.Group className="mb-3" controlId="renameView">
                    <Form.Label>New name</Form.Label>
                    <Form.Control
                      value={renaming.name}
                      autoFocus
                      required
                      onChange={(e) => setRenaming({ ...renaming, name: e.target.value })}
                    />
                  </Form.Group>
                  {renaming.references === null ? (
                    !renaming.error && <Spinner animation="border" size="sm" role="status" />
                  ) : (
                    referencesReport(renaming.view.name, renaming.references).map((line) => (
                      <p key={line} className="mb-2">
                        {line}
                      </p>
                    ))
                  )}
                </>
              )}
            </Modal.Body>
            <Modal.Footer>
              <Button variant="secondary" type="button" onClick={() => setRenaming(null)}>
                Cancel
              </Button>
              <Button
                type="submit"
                disabled={
                  busy ||
                  !renaming ||
                  renaming.references === null ||
                  !renaming.name.trim() ||
                  renaming.name.trim() === renaming.view.name
                }
              >
                Rename
              </Button>
            </Modal.Footer>
          </Form>
        </Modal>
      </PageBody>
    </>
  );
}
