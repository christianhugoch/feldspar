// The database connections list: every *other* database whose tables share the
// tables list.
//
// The file-stores list with a different noun, and with the same two states that
// make that screen worth having: a connection whose host is down is **still
// here**, marked, with the reason beside it — because editing it is the repair,
// and a list that hid it would hide the only screen the repair happens on.
//
// The third state is the one peculiar to a database, and the reason the summary
// column exists: a connection can dial perfectly and contribute nothing, because
// the schema it names is empty or is not the one the tables are in. That looks
// identical to a working connection from the outside — same green badge, same
// row — and the only thing that tells them apart is counting what actually
// reached the catalog. So the row says how many tables it put in the list, and
// names the ones it could not (a table whose name Saltcorn's own database
// already uses; the primary always wins).
//
// Editing is a dialog rather than a screen of its own, unlike an LLM provider's
// form: there are six boxes and no backend-declared settings to render, so the
// form has nothing to load and nothing to branch on.

import { useEffect, useState, type FormEvent } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Form from "react-bootstrap/Form";
import Modal from "react-bootstrap/Modal";
import Table from "react-bootstrap/Table";

import { api, errorMessage } from "../api";
import type { ListDatabaseConnectionsResponse } from "../client";
import {
  EMPTY_DB_CONNECTION_FORM,
  connectionBody,
  connectionSummary,
  connectionTarget,
  dbConnectionError,
  formFromConnection,
  type DbConnectionForm,
} from "../dbConnection";
import { IconPlus } from "../icons";
import { PageBody, PageHeader, StatusBadge } from "../layout";

type ConnectionItem = ListDatabaseConnectionsResponse[number];

/** The result of pressing Test: what the server said, kept beside the dialog. */
type TestResult = { connected: boolean; error?: string | null; tables: number };

export function DbConnections() {
  const [connections, setConnections] = useState<ConnectionItem[] | null>(null);
  const [editing, setEditing] = useState<DbConnectionForm | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = async () => {
    try {
      setConnections(await api.listDatabaseConnections());
    } catch (err) {
      setError(errorMessage(err, "Could not load the database connections."));
    }
  };

  useEffect(() => {
    void load();
  }, []);

  const save = async (form: DbConnectionForm) => {
    setBusy(true);
    setError(null);
    try {
      const body = connectionBody(form);
      if (form.id) await api.updateDatabaseConnection(form.id, body);
      else await api.createDatabaseConnection(body);
      setEditing(null);
      await load();
    } catch (err) {
      // The server names what is wrong — a name already taken, a schema that is
      // not an identifier — so surface its message rather than a generic one.
      setError(errorMessage(err, "Could not save the database connection."));
    } finally {
      setBusy(false);
    }
  };

  const remove = async (connection: ConnectionItem) => {
    if (
      !window.confirm(
        `Remove the database connection "${connection.name}"?\n\n` +
          "Its tables leave the tables list. Nothing in that database is changed or deleted.",
      )
    ) {
      return;
    }
    setError(null);
    try {
      await api.deleteDatabaseConnection(connection.id);
      await load();
    } catch (err) {
      setError(errorMessage(err, "Could not remove the database connection."));
    }
  };

  return (
    <>
      <PageHeader
        pretitle="Data"
        title="Database connections"
        actions={
          <Button onClick={() => setEditing({ ...EMPTY_DB_CONNECTION_FORM })}>
            <IconPlus className="icon-2" />
            New connection
          </Button>
        }
      />
      <PageBody>
        {error && <Alert variant="danger">{error}</Alert>}

        <div className="card">
          <Table hover responsive className="card-table table-vcenter">
            <thead>
              <tr>
                <th>Name</th>
                <th>Connects to</th>
                <th>State</th>
                <th className="text-end">Actions</th>
              </tr>
            </thead>
            <tbody>
              {connections?.length === 0 && (
                <tr>
                  <td colSpan={4} className="text-muted">
                    No database connections. Saltcorn is using its own database only; add a
                    connection to list another PostgreSQL database's tables beside it.
                  </td>
                </tr>
              )}
              {connections?.map((connection) => (
                <tr key={connection.id}>
                  <td>
                    {connection.name}
                    {connection.description && (
                      <div className="text-muted small">{connection.description}</div>
                    )}
                  </td>
                  <td className="text-break small">{connectionTarget(connection)}</td>
                  <td>
                    <StatusBadge tone={connection.connected ? "green" : "red"}>
                      {connection.connected ? "Connected" : "Not connected"}
                    </StatusBadge>
                    <div className="text-muted small">{connectionSummary(connection)}</div>
                    {connection.shadowed.length > 0 && (
                      <div className="text-muted small">
                        Not listed, because Saltcorn's own database already has a table of that
                        name: {connection.shadowed.join(", ")}
                      </div>
                    )}
                  </td>
                  <td className="text-end">
                    <div className="btn-list justify-content-end flex-nowrap align-items-center">
                      <Button
                        size="sm"
                        variant="outline-secondary"
                        onClick={() => setEditing(formFromConnection(connection))}
                      >
                        Edit
                      </Button>
                      <Button
                        size="sm"
                        variant="outline-danger"
                        onClick={() => void remove(connection)}
                      >
                        Remove
                      </Button>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </Table>
        </div>

        <p className="text-muted small mt-3">
          A connected database's tables appear in the tables list with the connection's name
          beside them. Saltcorn reads and writes their rows; it never changes their schema, and
          removing a connection changes nothing in the database it pointed at. Passwords are
          stored in the Saltcorn database and are never sent back to this screen — an existing
          one shows as ••••••••. They are not encrypted at rest, so treat database access as
          password access.
        </p>
      </PageBody>

      <ConnectionModal
        form={editing}
        busy={busy}
        onChange={setEditing}
        onCancel={() => setEditing(null)}
        onSubmit={save}
      />
    </>
  );
}

/**
 * The connect/edit dialog: where to dial, as whom, and which schema.
 *
 * Test is beside Save rather than behind it because the alternative is the
 * failure this whole screen is arranged against — saving a connection that
 * cannot dial, and finding out from an unchanged tables list. It sends what the
 * form currently holds and saves nothing.
 */
function ConnectionModal({
  form,
  busy,
  onChange,
  onCancel,
  onSubmit,
}: {
  form: DbConnectionForm | null;
  busy: boolean;
  onChange: (form: DbConnectionForm) => void;
  onCancel: () => void;
  onSubmit: (form: DbConnectionForm) => void;
}) {
  const [tested, setTested] = useState<TestResult | null>(null);
  const [testing, setTesting] = useState(false);

  // A dialog opened on a different connection must not still be showing the
  // last one's test result.
  useEffect(() => {
    setTested(null);
  }, [form?.id, form === null]);

  if (!form) return null;
  const problem = dbConnectionError(form);
  const set = (over: Partial<DbConnectionForm>) => onChange({ ...form, ...over });

  const test = async () => {
    setTesting(true);
    setTested(null);
    try {
      setTested(await api.testDatabaseConnection(connectionBody(form)));
    } catch (err) {
      setTested({
        connected: false,
        error: errorMessage(err, "Could not test the connection."),
        tables: 0,
      });
    } finally {
      setTesting(false);
    }
  };

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (!problem) onSubmit(form);
  };

  return (
    <Modal show onHide={onCancel} centered>
      <Form onSubmit={submit}>
        <Modal.Header closeButton>
          <Modal.Title>
            {form.id ? "Edit database connection" : "Connect a PostgreSQL database"}
          </Modal.Title>
        </Modal.Header>
        <Modal.Body>
          <Form.Group className="mb-3">
            <Form.Label>Name</Form.Label>
            <Form.Control
              autoFocus
              value={form.name}
              onChange={(e) => set({ name: e.target.value })}
              placeholder="reporting"
            />
            <Form.Text className="text-muted">
              Shown beside every table this connection brings into the tables list.
            </Form.Text>
          </Form.Group>

          <Form.Group className="mb-3">
            <Form.Label>Description</Form.Label>
            <Form.Control
              value={form.description}
              onChange={(e) => set({ description: e.target.value })}
            />
          </Form.Group>

          <div className="row">
            <Form.Group className="mb-3 col-8">
              <Form.Label>Host</Form.Label>
              <Form.Control
                value={form.host}
                onChange={(e) => set({ host: e.target.value })}
                placeholder="db.example.com"
              />
            </Form.Group>
            <Form.Group className="mb-3 col-4">
              <Form.Label>Port</Form.Label>
              <Form.Control
                value={form.port}
                onChange={(e) => set({ port: e.target.value })}
                placeholder="5432"
              />
            </Form.Group>
          </div>

          <Form.Group className="mb-3">
            <Form.Label>Database</Form.Label>
            <Form.Control
              value={form.database}
              onChange={(e) => set({ database: e.target.value })}
            />
          </Form.Group>

          <div className="row">
            <Form.Group className="mb-3 col-6">
              <Form.Label>Username</Form.Label>
              <Form.Control
                value={form.username}
                onChange={(e) => set({ username: e.target.value })}
              />
            </Form.Group>
            <Form.Group className="mb-3 col-6">
              <Form.Label>Password</Form.Label>
              <Form.Control
                type="password"
                value={form.password}
                onChange={(e) => set({ password: e.target.value })}
              />
            </Form.Group>
          </div>

          <Form.Group className="mb-3">
            <Form.Label>Schema</Form.Label>
            <Form.Control
              value={form.schema}
              onChange={(e) => set({ schema: e.target.value })}
              placeholder="public"
            />
            <Form.Text className="text-muted">
              One schema per connection: its tables are the ones that join the tables list.
            </Form.Text>
          </Form.Group>

          {problem && <div className="text-muted small">{problem}</div>}
          {tested && (
            <Alert variant={tested.connected ? "success" : "danger"} className="mb-0 mt-2">
              {tested.connected
                ? `Connected. ${tested.tables} ${tested.tables === 1 ? "table" : "tables"} in schema ${form.schema || "public"}.`
                : (tested.error ?? "Could not connect.")}
            </Alert>
          )}
        </Modal.Body>
        <Modal.Footer>
          <Button variant="secondary" onClick={onCancel} disabled={busy}>
            Cancel
          </Button>
          <Button
            variant="outline-secondary"
            onClick={() => void test()}
            disabled={busy || testing || !!problem}
          >
            {testing ? "Testing…" : "Test"}
          </Button>
          <Button type="submit" disabled={busy || !!problem}>
            {form.id ? "Save" : "Connect"}
          </Button>
        </Modal.Footer>
      </Form>
    </Modal>
  );
}
