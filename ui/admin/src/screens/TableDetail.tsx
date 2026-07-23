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
import Modal from "react-bootstrap/Modal";
import Row from "react-bootstrap/Row";
import Table from "react-bootstrap/Table";

import { api, errorMessage } from "../api";
import { navigate } from "../App";
import type {
  BrowseFilesResponse,
  CreateFieldRequest,
  ListFieldsResponse,
  ListFieldTypesResponse,
  ListTablesResponse,
} from "../client";
import { roleOptions, useRoles, type Roles } from "../roles";
import { SettingsFields, buildConfig } from "../settings";

/** One merged field as `listFields` reports it. */
type FieldInfo = ListFieldsResponse[number];

/** A field's kind, narrowed from the `unknown` the API types it as. */
type FieldKind = {
  type?: string;
  store?: string;
  folder?: string | null;
  target_table?: string;
} | null;

/** The store a `File` field points at, or `null` for any other kind. */
function fileStoreOf(field: FieldInfo): string | null {
  const kind = field.kind as FieldKind;
  return kind && kind.type === "file" ? (kind.store ?? "") : null;
}

/** A one-line description of a field's kind for the fields table. */
function kindLabel(kind: unknown): string {
  const k = kind as FieldKind;
  if (!k || !k.type || k.type === "plain") return "—";
  if (k.type === "file") return `file → ${k.store || "?"}`;
  if (k.type === "key") return `key → ${k.target_table || "?"}`;
  return k.type;
}

/** The human heading for a field-type category in the picker. */
function categoryLabel(category: string): string {
  if (category === "basic") return "Basic types";
  if (category === "rich") return "Rich types";
  return "Field kinds";
}

/** One table as `listTables` reports it, including its overlay settings. */
type TableSummary = ListTablesResponse[number];

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
  const [fieldTypes, setFieldTypes] = useState<ListFieldTypesResponse | null>(null);
  const [rows, setRows] = useState<RowRecord[] | null>(null);
  const [settings, setSettings] = useState<TableSummary | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    setError(null);
    try {
      // The settings come from the tables listing rather than a per-table
      // endpoint: the list already carries every overlay field, so a second
      // endpoint would be a second thing to keep in step with it.
      const [f, ft, r, t] = await Promise.all([
        api.listFields(table),
        api.listFieldTypes(),
        api.listRows(table),
        api.listTables(),
      ]);
      setFields(f);
      setFieldTypes(ft);
      setRows(r as RowRecord[]);
      setSettings(t.find((candidate) => candidate.name === table) ?? null);
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
        <h1 className="h3 mb-0 ms-2">{settings?.label || table}</h1>
      </div>
      {error && <Alert variant="danger">{error}</Alert>}

      <Settings table={table} settings={settings} onChange={load} />

      <Row>
        <Col lg={5} className="mb-4">
          <Fields table={table} fields={fields} fieldTypes={fieldTypes} onChange={load} />
        </Col>
        <Col lg={7} className="mb-4">
          <Rows table={table} fields={fields} rows={rows} onChange={load} />
        </Col>
      </Row>
    </>
  );
}

/**
 * The table's settings: the `_sc_tables` overlay (design §9).
 *
 * Everything here is *added* to what the database already says about the table —
 * nothing on this card restates a column, a type or a key, because those are the
 * database's to state and the overlay's to leave alone.
 *
 * The two roles are the point of the card. Until they can be set, every table is
 * admin-only and an application cannot serve anyone but its admin.
 */
function Settings({
  table,
  settings,
  onChange,
}: {
  table: string;
  settings: TableSummary | null;
  onChange: () => void;
}) {
  const roles = useRoles();
  const [label, setLabel] = useState("");
  const [description, setDescription] = useState("");
  const [read, setRead] = useState(1);
  const [write, setWrite] = useState(1);
  const [formula, setFormula] = useState("");
  const [rls, setRls] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  // Re-seed the form whenever the loaded table changes — including after a save,
  // so what is shown is what the server stored rather than what was typed.
  useEffect(() => {
    setLabel(settings?.label ?? "");
    setDescription(settings?.description ?? "");
    setRead(settings?.min_role_read ?? 1);
    setWrite(settings?.min_role_write ?? 1);
    setFormula(settings?.ownership_formula ?? "");
    setRls(settings?.rls_enabled ?? false);
    setSaved(false);
  }, [settings]);

  const save = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api.updateTable(table, {
        label: label.trim(),
        description: description.trim(),
        min_role_read: read,
        min_role_write: write,
        ownership_formula: formula.trim(),
        rls_enabled: rls,
      });
      setSaved(true);
      onChange();
    } catch (err) {
      // The server's validation message names what to fix — an unknown
      // identifier, a broken Ⱶ-path, a formula RLS cannot enforce.
      setError(err instanceof Error ? err.message : "Could not save the settings.");
    } finally {
      setBusy(false);
    }
  };

  const forget = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.deleteTableSettings(table);
      onChange();
    } catch {
      setError("Could not forget the settings.");
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card className="mb-4">
      <Card.Header>Settings</Card.Header>
      <Card.Body>
        {error && <Alert variant="danger">{error}</Alert>}
        {saved && !error && (
          <Alert variant="success" className="py-2">
            Saved. The new rules apply immediately — no restart.
          </Alert>
        )}
        <Form onSubmit={save}>
          <Row>
            <Col md={6}>
              <Form.Group className="mb-3" controlId="tableLabel">
                <Form.Label>Label</Form.Label>
                <Form.Control
                  value={label}
                  placeholder={table}
                  onChange={(e) => setLabel(e.target.value)}
                />
                <Form.Text muted>Shown instead of the table name. Blank uses the name.</Form.Text>
              </Form.Group>
            </Col>
            <Col md={6}>
              <Form.Group className="mb-3" controlId="tableDescription">
                <Form.Label>Description</Form.Label>
                <Form.Control
                  value={description}
                  onChange={(e) => setDescription(e.target.value)}
                />
              </Form.Group>
            </Col>
          </Row>
          <Row>
            <Col md={6}>
              <RoleSelect
                id="tableReadRole"
                label="Who can read rows"
                value={read}
                roles={roles}
                onChange={setRead}
              />
            </Col>
            <Col md={6}>
              <RoleSelect
                id="tableWriteRole"
                label="Who can create, update and delete rows"
                value={write}
                roles={roles}
                onChange={setWrite}
              />
            </Col>
          </Row>
          <Form.Group className="mb-3" controlId="tableOwnershipFormula">
            <Form.Label>Ownership formula</Form.Label>
            <Form.Control
              as="textarea"
              rows={2}
              className="font-monospace"
              value={formula}
              placeholder="owner === user.id"
              onChange={(e) => setFormula(e.target.value)}
            />
            <Form.Text muted>
              A JavaScript expression over the row&apos;s fields, <code>user</code>, the
              operation flags (<code>_read</code>, <code>_write</code>, …) and Ⱶ-joinfields.
              Rows it grants are reachable below the roles above. Blank means roles only.
            </Form.Text>
            {settings?.ownership_error && (
              <Alert variant="warning" className="py-2 mt-2 mb-0">
                Stored formula is not in effect (it grants nothing):{" "}
                {settings.ownership_error}
              </Alert>
            )}
          </Form.Group>
          {settings?.rls_available && (
            <Form.Group className="mb-3" controlId="tableRlsEnabled">
              <Form.Check
                type="switch"
                label="Enforce with database row-level security"
                checked={rls}
                onChange={(e) => setRls(e.target.checked)}
              />
              <Form.Text muted>
                The formula becomes Postgres RLS policies enforced by the database itself.
                Needs a formula the database can evaluate.
              </Form.Text>
            </Form.Group>
          )}
          <div className="d-flex gap-2 mt-2">
            <Button type="submit" size="sm" disabled={busy}>
              Save settings
            </Button>
            {settings?.configured && (
              <Button size="sm" variant="outline-secondary" disabled={busy} onClick={forget}>
                Forget settings
              </Button>
            )}
          </div>
          {settings?.configured && (
            <Form.Text muted className="d-block mt-2">
              Forgetting returns the table to admin-only. It never touches the table or its rows.
            </Form.Text>
          )}
        </Form>
      </Card.Body>
    </Card>
  );
}

/** A select over the roles the server offers, with the current value included. */
function RoleSelect({
  id,
  label,
  value,
  roles,
  onChange,
}: {
  id: string;
  label: string;
  value: number;
  roles: Roles;
  onChange: (role: number) => void;
}) {
  return (
    <Form.Group className="mb-3" controlId={id}>
      <Form.Label>{label}</Form.Label>
      <Form.Select value={value} onChange={(e) => onChange(Number(e.target.value))}>
        {roleOptions(value, roles).map((option) => (
          <option key={option.role} value={option.role}>
            {option.name} ({option.role})
          </option>
        ))}
      </Form.Select>
    </Form.Group>
  );
}

/**
 * The fields (columns) panel: list and add (design §3.4).
 *
 * The "add field" type input is a pick-list assembled from `listFieldTypes` —
 * basic types, rich types and the Key/File kinds in one list — and choosing a
 * type renders **its** declared attribute form beneath, driven entirely by the
 * type's `config_spec` with no per-type code here. That is the same "settings as
 * data" move a file-store backend's form is built on: a rich type or a kind added
 * to the server's registry gets a working form with no change to this file.
 */
function Fields({
  table,
  fields,
  fieldTypes,
  onChange,
}: {
  table: string;
  fields: ListFieldsResponse | null;
  fieldTypes: ListFieldTypesResponse | null;
  onChange: () => void;
}) {
  const [name, setName] = useState("");
  const [typeName, setTypeName] = useState("");
  const [nullable, setNullable] = useState(true);
  // Attribute-form values (rich type attributes, or a kind's parameters),
  // keyed by spec-field name. Reset whenever the chosen type changes.
  const [attrs, setAttrs] = useState<Record<string, string>>({});
  // A Key's stored SQL type must match the column it references; a File's is
  // always text, so the picker only asks for this when a Key is chosen.
  const [keyStorage, setKeyStorage] = useState("int8");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const selected = fieldTypes?.find((t) => t.name === typeName) ?? null;
  const basicTypes = useMemo(
    () => (fieldTypes ?? []).filter((t) => t.category === "basic"),
    [fieldTypes],
  );

  // Default the picker to the first type once the list loads.
  useEffect(() => {
    if (!typeName && fieldTypes && fieldTypes.length > 0) {
      setTypeName(fieldTypes[0].name);
    }
  }, [fieldTypes, typeName]);

  // A different type has different attributes, so clear what was entered.
  useEffect(() => {
    setAttrs({});
  }, [typeName]);

  const add = async (e: FormEvent) => {
    e.preventDefault();
    if (!name.trim() || !selected) return;
    setBusy(true);
    setError(null);
    try {
      const body: CreateFieldRequest = { name: name.trim(), type: selected.name, required: !nullable };
      if (selected.category === "kind") {
        // A kind carries its parameters in `kind`, and needs a storage type:
        // text for a File, the chosen SQL type for a Key.
        body.type = selected.name === "file" ? "text" : keyStorage;
        body.kind = { type: selected.name, ...buildConfig(selected.config_spec, attrs) };
      } else if (selected.category === "rich") {
        body.attributes = buildConfig(selected.config_spec, attrs);
      }
      await api.createField(table, body);
      setName("");
      setAttrs({});
      onChange();
    } catch (err) {
      setError(errorMessage(err, "Could not add the field."));
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
              <th>Kind</th>
              <th>Nullable</th>
            </tr>
          </thead>
          <tbody>
            {fields?.map((f) => (
              <tr key={f.name}>
                <td>{f.name}</td>
                <td>
                  <code>{f.type}</code>
                </td>
                <td>{kindLabel(f.kind)}</td>
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
            <Form.Label>Type</Form.Label>
            <Form.Select value={typeName} onChange={(e) => setTypeName(e.target.value)}>
              {["basic", "rich", "kind"].map((category) => {
                const items = (fieldTypes ?? []).filter((t) => t.category === category);
                if (items.length === 0) return null;
                return (
                  <optgroup key={category} label={categoryLabel(category)}>
                    {items.map((t) => (
                      <option key={t.name} value={t.name}>
                        {t.label}
                      </option>
                    ))}
                  </optgroup>
                );
              })}
            </Form.Select>
          </Form.Group>

          {/* A Key needs a storage type matching the column it references. */}
          {selected?.name === "key" && (
            <Form.Group className="mb-2" controlId="fieldKeyStorage">
              <Form.Label>Stored as</Form.Label>
              <Form.Select value={keyStorage} onChange={(e) => setKeyStorage(e.target.value)}>
                {basicTypes.map((t) => (
                  <option key={t.name} value={t.name}>
                    {t.label}
                  </option>
                ))}
              </Form.Select>
              <Form.Text muted>Match the type of the field this key references.</Form.Text>
            </Form.Group>
          )}

          {/* The chosen type's own attributes / a kind's parameters, rendered
              from its spec — no per-type code lives here. */}
          <SettingsFields
            spec={selected?.config_spec ?? []}
            values={attrs}
            onChange={(key, v) => setAttrs((a) => ({ ...a, [key]: v }))}
            idPrefix="field-attr"
          />

          <Form.Check
            className="mb-3"
            id="fieldNullable"
            type="checkbox"
            label="Nullable"
            checked={nullable}
            onChange={(e) => setNullable(e.target.checked)}
          />
          <Button type="submit" size="sm" disabled={busy || !name.trim() || !selected}>
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
