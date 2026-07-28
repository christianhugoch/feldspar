// Tables list: shows every table in the catalog and creates new ones. Each row
// links to the table detail screen (settings + fields + row editor).
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
import InputGroup from "react-bootstrap/InputGroup";
import Table from "react-bootstrap/Table";

import { api } from "../api";
import type { ListOrphanTableSettingsResponse, ListTablesResponse } from "../client";
import { AlertBody, PageBody, PageHeader, StatusBadge } from "../layout";
import { roleLabel, useRoles } from "../roles";

export function Tables() {
  const [tables, setTables] = useState<ListTablesResponse | null>(null);
  const [orphans, setOrphans] = useState<ListOrphanTableSettingsResponse>([]);
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);
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

  const create = async (e: FormEvent) => {
    e.preventDefault();
    if (!name.trim()) return;
    setBusy(true);
    setError(null);
    try {
      await api.createTable({ name: name.trim() });
      setName("");
      await load();
    } catch {
      setError("Could not create the table.");
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
          <Form onSubmit={create} className="d-flex">
            <InputGroup>
              <Form.Control
                placeholder="New table name"
                value={name}
                onChange={(e) => setName(e.target.value)}
              />
              <Button type="submit" disabled={busy || !name.trim()}>
                Create table
              </Button>
            </InputGroup>
          </Form>
        }
      />
      <PageBody>
        {error && <Alert variant="danger">{error}</Alert>}

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
    </>
  );
}
