// Tables list: shows every table in the catalog and creates new ones. Each row
// links to the table detail screen (fields + row editor).

import { useEffect, useState, type FormEvent } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Form from "react-bootstrap/Form";
import InputGroup from "react-bootstrap/InputGroup";
import Table from "react-bootstrap/Table";

import { api } from "../api";
import type { ListTablesResponse } from "../client";

export function Tables() {
  const [tables, setTables] = useState<ListTablesResponse | null>(null);
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = async () => {
    try {
      setTables(await api.listTables());
    } catch {
      setError("Could not load tables.");
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
      <h1 className="h3 mb-4">Tables</h1>
      {error && <Alert variant="danger">{error}</Alert>}

      <Form onSubmit={create} className="mb-4">
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

      <Table hover responsive>
        <thead>
          <tr>
            <th>Name</th>
            <th className="text-end">Actions</th>
          </tr>
        </thead>
        <tbody>
          {tables?.length === 0 && (
            <tr>
              <td colSpan={2} className="text-muted">
                No tables yet.
              </td>
            </tr>
          )}
          {tables?.map((t) => (
            <tr key={t.name}>
              <td>{t.name}</td>
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
    </>
  );
}
