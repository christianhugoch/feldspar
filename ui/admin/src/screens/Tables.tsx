// Tables list: shows every table in the catalog and creates new ones. Each row
// links to the table detail screen (settings + fields + row editor).
//
// Creating is behind a dialog rather than an input beside the button, because
// there is more than one way to make a table: empty, or from a CSV file whose
// header and contents decide the fields (§13.1). A name box that could only make
// the first kind would have to grow a second control anyway the moment the
// second existed, and the two would be asking for the same name twice. What the
// dialog *knows* — when Create may be pressed, what a chosen file suggests the
// table be called — is in `newTable.ts`, where it can be tested without a
// browser.
//
// The read/write roles are in this list, not only on the detail screen, because
// "which of these can the public read?" is a question about the whole set. The
// orphan banner is the visible half of a storage decision: settings for a table
// that is not in the database are kept rather than deleted (design §9), so that
// a dropped-and-recreated table gets its rules back — and something has to say
// they are there, or "kept" would mean "invisible".

import { useEffect, useState, type FormEvent } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Form from "react-bootstrap/Form";
import Modal from "react-bootstrap/Modal";
import Table from "react-bootstrap/Table";

import { api, errorMessage } from "../api";
import type { ListOrphanTableSettingsResponse, ListTablesResponse } from "../client";
import { AlertBody, PageBody, PageHeader, StatusBadge } from "../layout";
import {
  EMPTY_NEW_TABLE_FORM,
  importedMessage,
  newTableError,
  tableNameFromFile,
  type NewTableForm,
} from "../newTable";
import { roleLabel, useRoles } from "../roles";

export function Tables() {
  const [tables, setTables] = useState<ListTablesResponse | null>(null);
  const [orphans, setOrphans] = useState<ListOrphanTableSettingsResponse>([]);
  const [creating, setCreating] = useState<NewTableForm | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const roles = useRoles();

  const load = async () => {
    try {
      const [t, o] = await Promise.all([api.listTables(), api.listOrphanTableSettings()]);
      setTables(t);
      setOrphans(o);
    } catch {
      setError("Could not load tables.");
    }
  };

  const forget = async (table: string) => {
    setBusy(true);
    try {
      await api.deleteTableSettings(table);
      await load();
    } catch {
      setError("Could not forget those settings.");
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => {
    void load();
  }, []);

  /**
   * Make the table the dialog describes.
   *
   * The CSV path reads the file in the browser and sends it as text: the
   * endpoint model is JSON and a CSV is text, so there is no upload route to go
   * through (the same reason `importTableCsv` takes a string). The server's own
   * refusal is shown as it came — "`!` cannot be a column name", "the rows could
   * not be imported" — because it names the thing to fix in the file, which
   * nothing here could guess.
   */
  const create = async (e: FormEvent) => {
    e.preventDefault();
    if (!creating || newTableError(creating)) return;
    const name = creating.name.trim();
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      if (creating.source === "csv" && creating.file) {
        const csv = await creating.file.text();
        const { table, inserted } = await api.createTableFromCsv({ name, csv });
        setNotice(importedMessage(table.name, inserted));
      } else {
        await api.createTable({ name });
      }
      setCreating(null);
      await load();
    } catch (err) {
      setError(errorMessage(err, "Could not create the table."));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <PageHeader
        pretitle="Data"
        title="Tables"
        actions={
          <Button onClick={() => setCreating({ ...EMPTY_NEW_TABLE_FORM })}>+ New table</Button>
        }
      />
      <PageBody>
        {error && <Alert variant="danger">{error}</Alert>}
        {notice && (
          <Alert variant="success" dismissible onClose={() => setNotice(null)}>
            {notice}
          </Alert>
        )}

        {orphans.length > 0 && (
          <Alert variant="warning">
            <AlertBody>
              <Alert.Heading className="h6">Settings without a table</Alert.Heading>
              <p className="mb-2">
                These stored settings name tables that are not in the database. They are kept in
                case the table comes back — recreating it restores its access rules — but
                nothing is using them right now.
              </p>
              <ul className="mb-0 list-unstyled">
                {orphans.map((o) => (
                  <li key={o.name} className="d-flex align-items-center gap-2 mb-1">
                    <code>{o.name}</code>
                    <span className="text-muted small">
                      read {roleLabel(o.min_role_read, roles)}, write{" "}
                      {roleLabel(o.min_role_write, roles)}
                    </span>
                    <Button
                      size="sm"
                      variant="outline-secondary"
                      disabled={busy}
                      onClick={() => void forget(o.name)}
                    >
                      Forget
                    </Button>
                  </li>
                ))}
              </ul>
            </AlertBody>
          </Alert>
        )}

        <div className="card">
          <Table hover responsive className="card-table table-vcenter">
            <thead>
              <tr>
                <th>Name</th>
                <th>Read</th>
                <th>Write</th>
                <th className="text-end">Actions</th>
              </tr>
            </thead>
            <tbody>
              {tables?.length === 0 && (
                <tr>
                  <td colSpan={4} className="text-muted">
                    No tables yet.
                  </td>
                </tr>
              )}
              {tables?.map((t) => (
                <tr key={t.name}>
                  <td>
                    {t.label && t.label !== t.name ? (
                      <>
                        {t.label} <span className="text-muted small">({t.name})</span>
                      </>
                    ) : (
                      t.name
                    )}
                    {/* Ownership marks: the roles alone no longer tell the whole
                        access story for a table with a formula, so say so here. */}
                    {t.ownership_formula && !t.ownership_error && (
                      <StatusBadge tone="blue" className="ms-2" title={t.ownership_formula}>
                        formula
                      </StatusBadge>
                    )}
                    {t.ownership_error && (
                      <StatusBadge tone="yellow" className="ms-2" title={t.ownership_error}>
                        formula error
                      </StatusBadge>
                    )}
                    {t.rls_enabled && (
                      <StatusBadge tone="secondary" className="ms-2">
                        RLS
                      </StatusBadge>
                    )}
                  </td>
                  <td>{roleLabel(t.min_role_read, roles)}</td>
                  <td>{roleLabel(t.min_role_write, roles)}</td>
                  <td className="text-end">
                    <Button
                      size="sm"
                      variant="outline-primary"
                      href={`#/tables/${encodeURIComponent(t.name)}`}
                    >
                      Open
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </Table>
        </div>
      </PageBody>

      <NewTableModal
        form={creating}
        busy={busy}
        onChange={setCreating}
        onCancel={() => setCreating(null)}
        onSubmit={create}
      />
    </>
  );
}

/**
 * The New table dialog: a name, what kind, and — for a CSV — the file.
 *
 * The file input appears only for the CSV choice rather than being greyed out
 * beside it, because an input that cannot be used is a question that should not
 * have been asked. Choosing a file into an *empty* name box fills the name in
 * from the file's own name, which is what it is called nine times in ten.
 */
function NewTableModal({
  form,
  busy,
  onChange,
  onCancel,
  onSubmit,
}: {
  form: NewTableForm | null;
  busy: boolean;
  onChange: (form: NewTableForm) => void;
  onCancel: () => void;
  onSubmit: (e: FormEvent) => void;
}) {
  const problem = form ? newTableError(form) : null;
  return (
    <Modal show={form !== null} onHide={onCancel}>
      {form && (
        <Form onSubmit={onSubmit}>
          <Modal.Header closeButton>
            <Modal.Title className="h4">New table</Modal.Title>
          </Modal.Header>
          <Modal.Body>
            <Form.Group className="mb-3" controlId="new-table-name">
              <Form.Label>Name</Form.Label>
              <Form.Control
                autoFocus
                placeholder="e.g. invoice"
                value={form.name}
                onChange={(e) => onChange({ ...form, name: e.target.value })}
              />
            </Form.Group>

            <Form.Group className="mb-3" controlId="new-table-source">
              <Form.Label>Type</Form.Label>
              <Form.Select
                value={form.source}
                onChange={(e) =>
                  onChange({ ...form, source: e.target.value as NewTableForm["source"] })
                }
              >
                <option value="blank">New database table</option>
                <option value="csv">Create from CSV</option>
              </Form.Select>
            </Form.Group>

            {form.source === "csv" && (
              <Form.Group controlId="new-table-csv">
                <Form.Label>CSV file</Form.Label>
                <Form.Control
                  type="file"
                  accept=".csv,text/csv"
                  onChange={(e) => {
                    const file = (e.target as HTMLInputElement).files?.[0] ?? null;
                    onChange({
                      ...form,
                      file,
                      // A name already typed is the admin's and is kept.
                      name: form.name || (file ? tableNameFromFile(file.name) : ""),
                    });
                  }}
                />
                <Form.Text className="text-muted">
                  The columns become the table&rsquo;s fields — named and typed from the header
                  and the values under it — and every row is imported. A row the file&rsquo;s own
                  columns will not take is reported and no table is created.
                </Form.Text>
              </Form.Group>
            )}
          </Modal.Body>
          <Modal.Footer>
            {problem && <span className="text-muted small me-auto">{problem}</span>}
            <Button variant="outline-secondary" onClick={onCancel} disabled={busy}>
              Cancel
            </Button>
            <Button type="submit" disabled={busy || problem !== null}>
              Create
            </Button>
          </Modal.Footer>
        </Form>
      )}
    </Modal>
  );
}
