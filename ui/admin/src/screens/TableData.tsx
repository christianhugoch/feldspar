// Table data: a table's rows, and the form that creates and edits one.
//
// A screen of its own rather than a panel on the table page, which is the split
// Saltcorn 1 makes and for the same reason: the table page is about the table —
// its columns, its access rules, what fires on it — and is a page an admin reads
// top to bottom, while this is a grid they scroll and type in. Keeping the rows
// here also means the table page costs one `countRows` instead of every row in
// the table.
//
// Rows are arbitrary JSON (`listRows` returns `Array<unknown>`), so each is
// treated as a record keyed by field name. The editor is a single form that
// creates a new row or, when a row's "Edit" button is pressed, updates the
// selected one — addressed by the table's **primary key**, whatever it is
// called. Nothing invents a key (GOALS), so a table may have none, and then
// there is no way to say *which* row to change: the buttons go away and the
// panel says why, rather than sending an update nothing can address.

import { useEffect, useMemo, useState, type FormEvent } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Card from "react-bootstrap/Card";
import Form from "react-bootstrap/Form";
import Modal from "react-bootstrap/Modal";
import Table from "react-bootstrap/Table";

import { api } from "../api";
import { navigate } from "../App";
import { IconArrowLeft } from "../icons";
import { PageBody, PageHeader } from "../layout";
import type { BrowseFilesResponse, ListFieldsResponse, ListTablesResponse } from "../client";

/** One merged field as `listFields` reports it. */
type FieldInfo = ListFieldsResponse[number];

/** A field's kind, narrowed from the `unknown` the API types it as. */
type FieldKind = { type?: string; store?: string } | null;

/** The store a `File` field points at, or `null` for any other kind. */
function fileStoreOf(field: FieldInfo): string | null {
  const kind = field.kind as FieldKind;
  return kind && kind.type === "file" ? (kind.store ?? "") : null;
}

/** Whether a field is a non-stored calculated field (no column, computed on read). */
function isCalc(field: FieldInfo): boolean {
  return (field.kind as FieldKind)?.type === "calc";
}

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

export function TableData({ table }: { table: string }) {
  const [fields, setFields] = useState<ListFieldsResponse | null>(null);
  const [rows, setRows] = useState<RowRecord[] | null>(null);
  const [label, setLabel] = useState<string>(table);
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    setError(null);
    try {
      const [f, r, t] = await Promise.all([
        api.listFields(table),
        api.listRows(table),
        api.listTables(),
      ]);
      setFields(f);
      setRows(r as RowRecord[]);
      setLabel(
        (t as ListTablesResponse).find((candidate) => candidate.name === table)?.label || table,
      );
    } catch {
      setError("Could not load the rows.");
    }
  };

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [table]);

  return (
    <>
      <PageHeader
        pretitle="Table data"
        title={label}
        actions={
          <Button
            variant="outline-secondary"
            onClick={() => navigate(`/tables/${encodeURIComponent(table)}`)}
          >
            <IconArrowLeft className="icon-2" />
            {label}
          </Button>
        }
      />
      <PageBody>
        {error && <Alert variant="danger">{error}</Alert>}
        <Rows table={table} fields={fields} rows={rows} onChange={load} />
      </PageBody>
    </>
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

  // The column a row is addressed by. A composite key has no single value to
  // put in a URL, so it is treated as "not addressable" here for the same
  // reason no key is — the row endpoints take one id (§13.1).
  const pk = useMemo(() => {
    const keys = (fields ?? []).filter((f) => f.primary_key);
    return keys.length === 1 ? keys[0].name : null;
  }, [fields]);

  // Columns to show and edit: every declared field except a calculated one,
  // which is computed on read and refused on write. The key **is** editable —
  // a text or UUID key is a value somebody types — except while editing a row,
  // where it is what identifies the row being changed.
  const editable = useMemo(() => (fields ?? []).filter((f) => !isCalc(f)), [fields]);
  const columns = useMemo(() => (fields ?? []).map((f) => f.name), [fields]);

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
      const value = parseInput(values[f.name] ?? "");
      // A blank box on a column that fills itself in means "let it" — an
      // identity key numbers itself, a UUID key generates itself — and sending
      // an explicit null instead would be refused by the NOT NULL every key
      // column has. Whether it does is read off the column rather than assumed
      // from the key, because a key of any other type is one somebody types.
      if (f.generated && value === null) continue;
      body[f.name] = value;
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

  /** The row's key as a string, or `null` when the table has no single key. */
  const keyOf = (row: RowRecord): string | null => {
    if (pk === null) return null;
    const value = row[pk];
    return value === undefined || value === null ? null : String(value);
  };

  const edit = (row: RowRecord) => {
    const next: Record<string, string> = {};
    for (const f of editable) {
      next[f.name] = display(row[f.name]);
    }
    setValues(next);
    setEditingId(keyOf(row));
  };

  const remove = async (row: RowRecord) => {
    const key = keyOf(row);
    if (key === null) return;
    setError(null);
    try {
      await api.deleteRow(table, key);
      if (editingId === key) reset();
      onChange();
    } catch {
      setError("Could not delete the row.");
    }
  };

  return (
    <Card>
      <Card.Header>{editingId !== null ? "Edit row" : "New row"}</Card.Header>
      <Card.Body>
        {error && <Alert variant="danger">{error}</Alert>}
        {pk === null && (fields?.length ?? 0) > 0 && (
          <Alert variant="warning">
            This table has no single-column primary key, so a row cannot be picked out to
            change or delete. Rows can still be added and read. Give one field the
            <strong> Primary key</strong> tick on the table page to edit them.
          </Alert>
        )}

        <Form onSubmit={submit} className="mb-4">
          {editable.length === 0 && (
            <p className="text-muted">Add a field before creating rows.</p>
          )}
          {editable.map((f) => {
            const store = fileStoreOf(f);
            return (
              <Form.Group className="mb-2" controlId={`row-${f.name}`} key={f.name}>
                <Form.Label>{f.name}</Form.Label>
                {store !== null ? (
                  // A File field is a path in the field's store, so it is picked
                  // from that store rather than typed — the same browse endpoints
                  // the file manager uses.
                  <FileFieldInput
                    store={store}
                    value={values[f.name] ?? ""}
                    onChange={(v) => setValues({ ...values, [f.name]: v })}
                  />
                ) : (
                  <Form.Control
                    value={values[f.name] ?? ""}
                    // The key addresses the row being edited; changing it here
                    // would mean "move this row to another key", which is not
                    // what the form is for.
                    disabled={f.name === pk && editingId !== null}
                    placeholder={f.generated ? "assigned by the database if left blank" : ""}
                    onChange={(e) => setValues({ ...values, [f.name]: e.target.value })}
                  />
                )}
              </Form.Group>
            );
          })}
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
              <tr key={keyOf(row) ?? i}>
                {columns.map((c) => (
                  <td key={c}>{display(row[c])}</td>
                ))}
                <td className="text-end">
                  {keyOf(row) !== null && (
                    <>
                      <Button
                        size="sm"
                        variant="outline-secondary"
                        className="me-2"
                        onClick={() => edit(row)}
                      >
                        Edit
                      </Button>
                      <Button size="sm" variant="outline-danger" onClick={() => remove(row)}>
                        Delete
                      </Button>
                    </>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </Table>
      </Card.Body>
    </Card>
  );
}

/**
 * A row input for a `File` field: the chosen path, plus a "Choose" button that
 * opens a browser over the field's store (design §3.4).
 *
 * A File field is a path *within* a store, so a free-text box would let a typo
 * point it anywhere; browsing the store instead means every value is a path that
 * exists in the store the field is bound to. It reuses `browseFiles` — the same
 * endpoint the file manager is built on — rather than a new surface.
 */
function FileFieldInput({
  store,
  value,
  onChange,
}: {
  store: string;
  value: string;
  onChange: (value: string) => void;
}) {
  const [show, setShow] = useState(false);
  const [dir, setDir] = useState("");
  const [entries, setEntries] = useState<BrowseFilesResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!show) return;
    let cancelled = false;
    setError(null);
    setEntries(null);
    api
      .browseFiles(store, { dir })
      .then((list) => {
        if (!cancelled) setEntries(list);
      })
      .catch(() => {
        if (!cancelled) setError(`Could not browse the store “${store}”.`);
      });
    return () => {
      cancelled = true;
    };
  }, [show, dir, store]);

  const open = () => {
    setDir("");
    setShow(true);
  };

  const parent = dir.includes("/") ? dir.slice(0, dir.lastIndexOf("/")) : "";

  return (
    <>
      <div className="d-flex gap-2">
        <Form.Control
          value={value}
          placeholder={`path in ${store}`}
          onChange={(e) => onChange(e.target.value)}
        />
        <Button size="sm" variant="outline-secondary" type="button" onClick={open}>
          Choose…
        </Button>
      </div>

      <Modal show={show} onHide={() => setShow(false)}>
        <Modal.Header closeButton>
          <Modal.Title className="h6">
            {store}
            {dir && ` / ${dir}`}
          </Modal.Title>
        </Modal.Header>
        <Modal.Body>
          {error && <Alert variant="danger">{error}</Alert>}
          {!entries && !error && <p className="text-muted mb-0">Loading…</p>}
          {entries && (
            <div className="list-group">
              {dir !== "" && (
                <button
                  type="button"
                  className="list-group-item list-group-item-action"
                  onClick={() => setDir(parent)}
                >
                  ← up
                </button>
              )}
              {entries.length === 0 && <div className="list-group-item text-muted">Empty.</div>}
              {entries.map((entry) => (
                <button
                  key={entry.path}
                  type="button"
                  className="list-group-item list-group-item-action d-flex justify-content-between"
                  onClick={() => {
                    if (entry.is_dir) {
                      setDir(entry.path);
                    } else {
                      onChange(entry.path);
                      setShow(false);
                    }
                  }}
                >
                  <span>
                    {entry.is_dir ? "📁 " : "📄 "}
                    {entry.name}
                  </span>
                  {!entry.is_dir && entry.size != null && (
                    <span className="text-muted small">{entry.size} B</span>
                  )}
                </button>
              ))}
            </div>
          )}
        </Modal.Body>
      </Modal>
    </>
  );
}
