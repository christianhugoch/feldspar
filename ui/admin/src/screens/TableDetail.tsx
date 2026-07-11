// Table detail: manage a single table's fields and edit its rows.
//
// Fields come from `listFields`; rows are arbitrary JSON (`listRows` returns
// `Array<unknown>`), so we treat each row as a record keyed by field name. The
// row editor is a single form that creates a new row or, when a row's "Edit"
// button is pressed, updates the selected one (addressed by its `id`).

import { useEffect, useMemo, useState, type FormEvent } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Card from "react-bootstrap/Card";
import Col from "react-bootstrap/Col";
import Form from "react-bootstrap/Form";
import Row from "react-bootstrap/Row";
import Table from "react-bootstrap/Table";

import { api } from "../api";
import { navigate } from "../App";
import type { ListFieldsResponse } from "../client";

/** A row as returned by `listRows` — arbitrary JSON keyed by column name. */
type RowRecord = Record<string, unknown>;

/** Render a JSON cell value as a compact string for the rows table. */
function display(value: unknown): string {
  if (value === null || value === undefined) return "";
  if (typeof value === "object") return JSON.stringify(value);
  return String(value);
}

/**
 * Best-effort parse of a form input into JSON: `5` → number, `true` → boolean,
 * plain text stays a string. The server coerces to each column's type, so this
 * only needs to turn obvious scalars into their JSON form.
 */
function parseInput(raw: string): unknown {
  const trimmed = raw.trim();
  if (trimmed === "") return null;
  try {
    return JSON.parse(trimmed) as unknown;
  } catch {
    return raw;
  }
}

export function TableDetail({ table }: { table: string }) {
  const [fields, setFields] = useState<ListFieldsResponse | null>(null);
  const [rows, setRows] = useState<RowRecord[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    setError(null);
    try {
      const [f, r] = await Promise.all([api.listFields(table), api.listRows(table)]);
      setFields(f);
      setRows(r as RowRecord[]);
    } catch {
      setError("Could not load the table.");
    }
  };

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [table]);

  return (
    <>
      <div className="d-flex align-items-center mb-4">
        <Button variant="link" className="ps-0" onClick={() => navigate("/tables")}>
          ← Tables
        </Button>
        <h1 className="h3 mb-0 ms-2">{table}</h1>
      </div>
      {error && <Alert variant="danger">{error}</Alert>}

      <Row>
        <Col lg={5} className="mb-4">
          <Fields table={table} fields={fields} onChange={load} />
        </Col>
        <Col lg={7} className="mb-4">
          <Rows table={table} fields={fields} rows={rows} onChange={load} />
        </Col>
      </Row>
    </>
  );
}

/** The fields (columns) panel: list and add. */
function Fields({
  table,
  fields,
  onChange,
}: {
  table: string;
  fields: ListFieldsResponse | null;
  onChange: () => void;
}) {
  const [name, setName] = useState("");
  const [sqlType, setSqlType] = useState("text");
  const [nullable, setNullable] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const add = async (e: FormEvent) => {
    e.preventDefault();
    if (!name.trim()) return;
    setBusy(true);
    setError(null);
    try {
      await api.createField(table, { name: name.trim(), sql_type: sqlType.trim(), nullable });
      setName("");
      onChange();
    } catch {
      setError("Could not add the field.");
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card>
      <Card.Header>Fields</Card.Header>
      <Card.Body>
        {error && <Alert variant="danger">{error}</Alert>}
        <Table size="sm" className="mb-3">
          <thead>
            <tr>
              <th>Name</th>
              <th>Type</th>
              <th>Nullable</th>
            </tr>
          </thead>
          <tbody>
            {fields?.map((f) => (
              <tr key={f.name}>
                <td>{f.name}</td>
                <td>
                  <code>{f.sql_type}</code>
                </td>
                <td>{f.nullable ? "yes" : "no"}</td>
              </tr>
            ))}
          </tbody>
        </Table>
        <Form onSubmit={add}>
          <Form.Group className="mb-2" controlId="fieldName">
            <Form.Label>Name</Form.Label>
            <Form.Control value={name} onChange={(e) => setName(e.target.value)} />
          </Form.Group>
          <Form.Group className="mb-2" controlId="fieldType">
            <Form.Label>SQL type</Form.Label>
            <Form.Control value={sqlType} onChange={(e) => setSqlType(e.target.value)} />
          </Form.Group>
          <Form.Check
            className="mb-3"
            id="fieldNullable"
            type="checkbox"
            label="Nullable"
            checked={nullable}
            onChange={(e) => setNullable(e.target.checked)}
          />
          <Button type="submit" size="sm" disabled={busy || !name.trim()}>
            Add field
          </Button>
        </Form>
      </Card.Body>
    </Card>
  );
}

/** The rows panel: the row editor plus the rows table. */
function Rows({
  table,
  fields,
  rows,
  onChange,
}: {
  table: string;
  fields: ListFieldsResponse | null;
  rows: RowRecord[] | null;
  onChange: () => void;
}) {
  const [values, setValues] = useState<Record<string, string>>({});
  const [editingId, setEditingId] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Columns to show and edit: every declared field. `id` is server-generated,
  // so it is shown in the table but never an editable input.
  const editable = useMemo(
    () => (fields ?? []).filter((f) => f.name !== "id"),
    [fields],
  );
  const columns = useMemo(() => {
    const names = (fields ?? []).map((f) => f.name);
    return names.includes("id") ? names : ["id", ...names];
  }, [fields]);

  const reset = () => {
    setValues({});
    setEditingId(null);
  };

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    const body: RowRecord = {};
    for (const f of editable) {
      body[f.name] = parseInput(values[f.name] ?? "");
    }
    try {
      if (editingId !== null) {
        await api.updateRow(table, editingId, body);
      } else {
        await api.createRow(table, body);
      }
      reset();
      onChange();
    } catch {
      setError(editingId !== null ? "Could not update the row." : "Could not create the row.");
    } finally {
      setBusy(false);
    }
  };

  const edit = (row: RowRecord) => {
    const next: Record<string, string> = {};
    for (const f of editable) {
      next[f.name] = display(row[f.name]);
    }
    setValues(next);
    setEditingId(row.id === undefined || row.id === null ? null : String(row.id));
  };

  const remove = async (row: RowRecord) => {
    if (row.id === undefined || row.id === null) return;
    setError(null);
    try {
      await api.deleteRow(table, String(row.id));
      if (editingId === String(row.id)) reset();
      onChange();
    } catch {
      setError("Could not delete the row.");
    }
  };

  return (
    <Card>
      <Card.Header>Rows</Card.Header>
      <Card.Body>
        {error && <Alert variant="danger">{error}</Alert>}

        <Form onSubmit={submit} className="mb-4">
          {editable.length === 0 && (
            <p className="text-muted">Add a field before creating rows.</p>
          )}
          {editable.map((f) => (
            <Form.Group className="mb-2" controlId={`row-${f.name}`} key={f.name}>
              <Form.Label>{f.name}</Form.Label>
              <Form.Control
                value={values[f.name] ?? ""}
                onChange={(e) => setValues({ ...values, [f.name]: e.target.value })}
              />
            </Form.Group>
          ))}
          {editable.length > 0 && (
            <div className="d-flex gap-2">
              <Button type="submit" size="sm" disabled={busy}>
                {editingId !== null ? "Save changes" : "Add row"}
              </Button>
              {editingId !== null && (
                <Button size="sm" variant="secondary" onClick={reset} type="button">
                  Cancel
                </Button>
              )}
            </div>
          )}
        </Form>

        <Table size="sm" hover responsive>
          <thead>
            <tr>
              {columns.map((c) => (
                <th key={c}>{c}</th>
              ))}
              <th className="text-end">Actions</th>
            </tr>
          </thead>
          <tbody>
            {rows?.length === 0 && (
              <tr>
                <td colSpan={columns.length + 1} className="text-muted">
                  No rows yet.
                </td>
              </tr>
            )}
            {rows?.map((row, i) => (
              <tr key={(row.id as string | undefined) ?? i}>
                {columns.map((c) => (
                  <td key={c}>{display(row[c])}</td>
                ))}
                <td className="text-end">
                  <Button size="sm" variant="outline-secondary" className="me-2" onClick={() => edit(row)}>
                    Edit
                  </Button>
                  <Button size="sm" variant="outline-danger" onClick={() => remove(row)}>
                    Delete
                  </Button>
                </td>
              </tr>
            ))}
          </tbody>
        </Table>
      </Card.Body>
    </Card>
  );
}
