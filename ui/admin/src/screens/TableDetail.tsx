// Table detail: everything about one table, top to bottom.
//
// The order is the one Saltcorn 1 settled on, and it is the order an admin
// works in rather than the order the API is grouped in:
//
//   1. **Fields** — what the table *is*. Nothing below it makes sense until the
//      columns do, so it comes first and gets the full width.
//   2. **Table data** — one shallow strip of tiles: how many rows there are, the
//      way through to them, CSV out and CSV in, and a menu for the rare and
//      irreversible. The rows themselves live on their own screen
//      (`TableData`), which is what keeps this page cheap to open: one
//      `countRows` rather than every row in the table.
//   3. **Triggers on this table** — what happens when its rows change. Filtered
//      from the trigger list by channel, because a trigger's channel *is* its
//      table (§10.2).
//   4. **Edit table properties** — the `_sc_tables` overlay: labels, roles,
//      ownership. Last because it is the part an admin sets once.
//
// Fields come from `listFields`; the settings come from the tables listing,
// which already carries every overlay field.

import { useEffect, useMemo, useState, type FormEvent, type ReactNode } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Card from "react-bootstrap/Card";
import Col from "react-bootstrap/Col";
import Dropdown from "react-bootstrap/Dropdown";
import Form from "react-bootstrap/Form";
import Modal from "react-bootstrap/Modal";
import Row from "react-bootstrap/Row";
import Table from "react-bootstrap/Table";

import { api, errorMessage } from "../api";
import { navigate } from "../App";
import {
  IconArrowLeft,
  IconDots,
  IconDownload,
  IconPencil,
  IconPlus,
  IconUpload,
} from "../icons";
import { PageBody, PageHeader, StatusBadge } from "../layout";
import type {
  ListFieldsResponse,
  ListFieldTypesResponse,
  ListTablesResponse,
  ListTriggersResponse,
} from "../client";
import {
  createFieldBody,
  fieldForm,
  fieldFormError,
  newFieldForm,
  updateFieldBody,
  type FieldForm,
  type FieldItem,
  type FieldKind,
} from "../fieldForm";
import { keyStorage, reconcileKey, type KeyKind } from "../keyField";
import { roleOptions, useRoles, type Roles } from "../roles";
import { SettingsFields } from "../settings";

/** A one-line description of a field's kind for the fields table. */
function kindLabel(kind: unknown): string {
  const k = kind as FieldKind;
  if (!k || !k.type || k.type === "plain") return "—";
  if (k.type === "file") return `file → ${k.store || "?"}`;
  if (k.type === "key") return `key → ${k.target_table || "?"}`;
  if (k.type === "calc") return `calc: ${k.expression || "?"}`;
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

/** One trigger as `listTriggers` reports it. */
type TriggerItem = ListTriggersResponse[number];

/** The events that happen *to a table's rows* — the ones whose channel is a
 * table name, and so the ones this page can show as "on this table" (§10.2). */
const TABLE_EVENTS = ["insert", "update", "delete"];

/**
 * The triggers that fire on `table`'s rows, out of every trigger there is.
 *
 * Both halves matter. The kind must be a **row** event, because a `never` or a
 * `daily` trigger whose *action configuration* happens to name this table is
 * not a trigger on it — it is a trigger that writes to it, which is a different
 * question. And the channel must be this table: for the row events the channel
 * *is* the table (§10.2), so this is the whole binding, and there is nothing
 * else to look at.
 */
export function triggersOnTable(
  triggers: ListTriggersResponse,
  table: string,
): ListTriggersResponse {
  return triggers.filter((t) => TABLE_EVENTS.includes(t.when) && t.channel === table);
}

export function TableDetail({ table }: { table: string }) {
  const [fields, setFields] = useState<ListFieldsResponse | null>(null);
  const [fieldTypes, setFieldTypes] = useState<ListFieldTypesResponse | null>(null);
  const [rowCount, setRowCount] = useState<number | null>(null);
  const [settings, setSettings] = useState<TableSummary | null>(null);
  const [triggers, setTriggers] = useState<TriggerItem[] | null>(null);
  // Every table in the catalog: what a Key field's target is chosen from. This
  // table is included — a key onto its own table (a parent link) is legitimate.
  const [tables, setTables] = useState<ListTablesResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    setError(null);
    try {
      // The settings come from the tables listing rather than a per-table
      // endpoint: the list already carries every overlay field, so a second
      // endpoint would be a second thing to keep in step with it.
      const [f, ft, c, t, tr] = await Promise.all([
        api.listFields(table),
        api.listFieldTypes(),
        api.countRows(table),
        api.listTables(),
        api.listTriggers(),
      ]);
      setFields(f);
      setFieldTypes(ft);
      setRowCount(c.count);
      setTables(t);
      setTriggers(tr);
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
      <PageHeader
        pretitle="Table"
        title={settings?.label || table}
        actions={
          <Button variant="outline-secondary" onClick={() => navigate("/tables")}>
            <IconArrowLeft className="icon-2" />
            Tables
          </Button>
        }
      />
      <PageBody>
        {error && <Alert variant="danger">{error}</Alert>}

        <Fields
          table={table}
          fields={fields}
          fieldTypes={fieldTypes}
          tables={tables}
          onChange={load}
        />

        <TableData
          table={table}
          rowCount={rowCount}
          configured={settings?.configured ?? false}
          onChange={load}
        />

        <Triggers table={table} triggers={triggers} onChange={load} />

        <Settings table={table} settings={settings} onChange={load} />
      </PageBody>
    </>
  );
}

/**
 * The table-data strip: how much data there is, and the four things one does
 * with the data rather than with the table.
 *
 * Deliberately **shallow** — a row of tiles, not a panel. Everything on it is a
 * single click that leaves this page or moves a file, so there is nothing to
 * lay out beyond the tiles themselves, and its height is what keeps the fields
 * above it and the properties below it on the same screen.
 */
function TableData({
  table,
  rowCount,
  configured,
  onChange,
}: {
  table: string;
  rowCount: number | null;
  configured: boolean;
  onChange: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  /** One message per row an import refused, each naming its line. */
  const [rejected, setRejected] = useState<string[]>([]);

  /**
   * Save the table's rows as a CSV file.
   *
   * The document arrives as text in the endpoint's JSON (the endpoint model has
   * no bytes shape, and CSV is text), so the download is made here from a blob
   * and an object URL. The link is clicked and revoked in the same turn — there
   * is nothing to leave in the document, and the SPA is the only thing that
   * ever wanted the URL.
   */
  const download = async () => {
    setBusy(true);
    setError(null);
    setNotice(null);
    setRejected([]);
    try {
      const { filename, csv } = await api.exportTableCsv(table);
      const url = URL.createObjectURL(new Blob([csv], { type: "text/csv;charset=utf-8" }));
      const link = document.createElement("a");
      link.href = url;
      link.download = filename;
      link.click();
      URL.revokeObjectURL(url);
    } catch (err) {
      setError(errorMessage(err, "Could not export the rows."));
    } finally {
      setBusy(false);
    }
  };

  /**
   * Read a chosen CSV file and import it.
   *
   * Both numbers are reported, because both are true of a real file: an import
   * is not a transaction (each row stands on its own), so "42 rows added, 3
   * refused" is the answer, and the three come back with their line numbers so
   * they can be found in the file.
   */
  const upload = async (file: File) => {
    setBusy(true);
    setError(null);
    setNotice(null);
    setRejected([]);
    try {
      const csv = await file.text();
      const { inserted, errors } = await api.importTableCsv(table, { csv });
      setNotice(
        `${inserted} row${inserted === 1 ? "" : "s"} added from ${file.name}` +
          (errors.length > 0 ? `, ${errors.length} refused.` : "."),
      );
      setRejected(errors);
      onChange();
    } catch (err) {
      // The server's own refusal — a header naming a column the table does not
      // have, a file that is not CSV — says what to fix.
      setError(errorMessage(err, "Could not import the file."));
    } finally {
      setBusy(false);
    }
  };

  /**
   * Drop the table, its columns, its rows and its settings row.
   *
   * Confirmed in the browser because it is the one irreversible thing on this
   * screen — and the server's own refusal (another table's key still points
   * here) is shown as it came, since it names the fields to remove first.
   */
  const dropTable = async () => {
    if (!window.confirm(`Drop the table "${table}" and every row in it? This cannot be undone.`)) {
      return;
    }
    try {
      await api.dropTable(table);
      navigate("/tables");
    } catch (err) {
      setError(errorMessage(err, "Could not drop the table."));
    }
  };

  const forget = async () => {
    if (
      !window.confirm(
        `Forget the settings for "${table}"?\n\n` +
          "It returns to admin-only. The table and its rows are not touched.",
      )
    ) {
      return;
    }
    setError(null);
    try {
      await api.deleteTableSettings(table);
      onChange();
    } catch (err) {
      setError(errorMessage(err, "Could not forget the settings."));
    }
  };

  return (
    <Card className="mb-4">
      <Card.Header>Table data</Card.Header>
      <Card.Body>
        {error && <Alert variant="danger">{error}</Alert>}
        {notice && (
          <Alert
            variant={rejected.length > 0 ? "warning" : "success"}
            onClose={() => setNotice(null)}
            dismissible
          >
            <div className="flex-fill">
              <div>{notice}</div>
              {rejected.length > 0 && (
                <ul className="small mb-0 mt-2">
                  {rejected.map((message) => (
                    <li key={message}>{message}</li>
                  ))}
                </ul>
              )}
            </div>
          </Alert>
        )}
        <div className="d-flex flex-wrap align-items-center justify-content-around gap-3 text-center">
          <div>
            <div className="h1 mb-0">{rowCount ?? "—"}</div>
            <div className="text-muted">{rowCount === 1 ? "Row" : "Rows"}</div>
          </div>

          <Tile
            label="Edit"
            icon={<IconPencil />}
            onClick={() => navigate(`/tables/${encodeURIComponent(table)}/data`)}
          />

          <Tile
            label="Download CSV"
            icon={<IconDownload />}
            disabled={busy}
            onClick={() => void download()}
          />

          <UploadTile disabled={busy} onFile={(file) => void upload(file)} />

          <Dropdown align="end">
            <Dropdown.Toggle
              variant="outline-secondary"
              size="sm"
              className="data-menu-toggle btn-icon"
              id="table-data-menu"
              aria-label="More table actions"
            >
              <IconDots className="icon-2" />
            </Dropdown.Toggle>
            <Dropdown.Menu>
              {configured && (
                <Dropdown.Item onClick={() => void forget()}>Forget settings</Dropdown.Item>
              )}
              <Dropdown.Item className="text-danger" onClick={() => void dropTable()}>
                Drop table
              </Dropdown.Item>
            </Dropdown.Menu>
          </Dropdown>
        </div>
      </Card.Body>
    </Card>
  );
}

/** One tile of the table-data strip: a large icon over its label (`admin.css`). */
function Tile({
  label,
  icon,
  disabled,
  onClick,
}: {
  label: string;
  icon: ReactNode;
  disabled?: boolean;
  onClick: () => void;
}) {
  return (
    <button type="button" className="data-tile" disabled={disabled} onClick={onClick}>
      {icon}
      <span>{label}</span>
    </button>
  );
}

/**
 * The upload tile: the same shape as the others, over a hidden file input.
 *
 * A file cannot be chosen without one — the browser only opens the picker from
 * a real `<input type="file">` — so the input is present and invisible and the
 * tile is its label. The value is cleared after each pick so that choosing the
 * *same* file again still fires a change.
 */
function UploadTile({
  disabled,
  onFile,
}: {
  disabled?: boolean;
  onFile: (file: File) => void;
}) {
  return (
    <label className="data-tile mb-0">
      <IconUpload />
      <span>Upload CSV</span>
      <input
        type="file"
        accept=".csv,text/csv"
        className="d-none"
        disabled={disabled}
        onChange={(e) => {
          const file = e.target.files?.[0];
          e.target.value = "";
          if (file) onFile(file);
        }}
      />
    </label>
  );
}

/**
 * The triggers that fire on this table's rows (§10.2).
 *
 * Filtered from the whole trigger list rather than fetched by table, because a
 * trigger's **channel is its table** for the three row events — there is no
 * separate binding to query. Everything about a trigger is edited on the
 * trigger form; this card exists so that "what happens when a row of this table
 * changes?" is answerable from the table, which is where it is asked.
 */
function Triggers({
  table,
  triggers,
  onChange,
}: {
  table: string;
  triggers: TriggerItem[] | null;
  onChange: () => void;
}) {
  const [error, setError] = useState<string | null>(null);
  const mine = useMemo(() => triggersOnTable(triggers ?? [], table), [triggers, table]);

  const remove = async (trigger: TriggerItem) => {
    if (
      !window.confirm(
        `Delete the trigger "${trigger.name}"?\n\n` +
          "Its configuration is deleted with it. Switch it off instead if you " +
          "only want it to stop firing.",
      )
    ) {
      return;
    }
    setError(null);
    try {
      await api.deleteTrigger(trigger.id);
      onChange();
    } catch (err) {
      setError(errorMessage(err, "Could not delete the trigger."));
    }
  };

  return (
    <Card className="mb-4">
      <Card.Header>Triggers on this table</Card.Header>
      <Card.Body>
        {error && <Alert variant="danger">{error}</Alert>}
        <p className="text-muted">Triggers run actions in response to events on this table.</p>
        {mine.length > 0 && (
          <Table size="sm" hover responsive className="table-vcenter">
            <thead>
              <tr>
                <th>Name</th>
                <th>Event</th>
                <th>Runs</th>
                <th>Status</th>
                <th className="text-end">Actions</th>
              </tr>
            </thead>
            <tbody>
              {mine.map((trigger) => (
                <tr key={trigger.id}>
                  <td>
                    {trigger.name}
                    {trigger.description && (
                      <div className="text-muted small">{trigger.description}</div>
                    )}
                  </td>
                  <td>
                    {trigger.when}
                    {trigger.only_if && (
                      <div className="text-muted small font-monospace text-break">
                        if {trigger.only_if}
                      </div>
                    )}
                  </td>
                  <td className="text-break">{trigger.action}</td>
                  <td>
                    {trigger.error ? (
                      <StatusBadge tone="red" title={trigger.error}>
                        Not usable
                      </StatusBadge>
                    ) : trigger.enabled ? (
                      <StatusBadge tone="green">Enabled</StatusBadge>
                    ) : (
                      <StatusBadge tone="secondary">Off</StatusBadge>
                    )}
                  </td>
                  <td className="text-end">
                    <div className="btn-list justify-content-end flex-nowrap">
                      <Button
                        size="sm"
                        variant="outline-secondary"
                        href={`#/triggers/${encodeURIComponent(trigger.id)}/edit`}
                      >
                        Edit
                      </Button>
                      <Button
                        size="sm"
                        variant="outline-danger"
                        onClick={() => void remove(trigger)}
                      >
                        Delete
                      </Button>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </Table>
        )}
        <Button
          size="sm"
          onClick={() => navigate(`/triggers/new/${encodeURIComponent(table)}`)}
        >
          <IconPlus className="icon-2" />
          Create trigger
        </Button>
      </Card.Body>
    </Card>
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

  return (
    <Card className="mb-4">
      <Card.Header>Edit table properties</Card.Header>
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
              placeholder="Example: owner === user.id"
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
          <Button type="submit" size="sm" disabled={busy}>
            Save settings
          </Button>
          {settings?.configured && (
            <Form.Text muted className="d-block mt-2">
              &ldquo;Forget settings&rdquo;, on the table-data menu above, returns the table to
              admin-only. It never touches the table or its rows.
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

/** What the field modal is open on: a new field, or one that exists. */
type Editing = { mode: "add" } | { mode: "edit"; field: string };

/**
 * The fields (columns) panel: list, add and edit (design §3.4).
 *
 * The card is the **list**; adding and editing are the same modal behind the
 * "Add field" button and each row's Edit button. The form is the taller of the
 * two by some way — a Key's three dependent selects, a rich type's whole
 * attribute spec — and side by side it either squeezed the list into half a page
 * or left a column of white space under it, depending on which type was chosen.
 * In a modal it can be as tall as it needs to be, and the card goes back to
 * being the reference an admin reads.
 *
 * One modal for both, because a field is one thing to describe however it got
 * here, and the alternative is two forms that have to agree about where a File's
 * parameters live. What differs is only what may be changed: an edit writes the
 * `_sc_fields` overlay and nothing else (§3.3), so the name and the NOT NULL are
 * shown disabled rather than offered and quietly dropped.
 *
 * The type input is a pick-list assembled from `listFieldTypes` — basic types,
 * rich types and the Key/File kinds in one list — and choosing a type renders
 * **its** declared attribute form beneath, driven entirely by the type's
 * `config_spec` with no per-type code here. That is the same "settings as data"
 * move a file-store backend's form is built on: a rich type or a kind added to
 * the server's registry gets a working form with no change to this file.
 */
function Fields({
  table,
  fields,
  fieldTypes,
  tables,
  onChange,
}: {
  table: string;
  fields: ListFieldsResponse | null;
  fieldTypes: ListFieldTypesResponse | null;
  tables: ListTablesResponse | null;
  onChange: () => void;
}) {
  /** What the modal is open on, or `null` when it is closed. */
  const [editing, setEditing] = useState<Editing | null>(null);
  const [form, setForm] = useState<FieldForm>(newFieldForm(""));
  const [targetFields, setTargetFields] = useState<ListFieldsResponse | null>(null);
  const [busy, setBusy] = useState(false);
  // Two error slots, because the two things this card does now happen in two
  // places: a refused drop belongs on the card, beside the row it was about,
  // and everything the field form can be told belongs in the modal the admin is
  // looking at.
  const [error, setError] = useState<string | null>(null);
  const [formError, setFormError] = useState<string | null>(null);

  const update = (patch: Partial<FieldForm>) => setForm((f) => ({ ...f, ...patch }));

  /** Whether the modal is editing a field that exists — which is what decides
   * what it may change, not just what it says at the top. */
  const isEdit = editing?.mode === "edit";
  const selected = fieldTypes?.find((t) => t.name === form.typeName) ?? null;
  const basicTypes = useMemo(
    () => (fieldTypes ?? []).filter((t) => t.category === "basic"),
    [fieldTypes],
  );
  // A calc field has no column, so a kind (Key/File) makes no sense for it —
  // the picker offers only value types (basic + rich) while it is checked.
  const typeCategories = form.calculated ? ["basic", "rich"] : ["basic", "rich", "kind"];

  // Default the picker to the first type once the list loads.
  useEffect(() => {
    if (!form.typeName && fieldTypes && fieldTypes.length > 0) {
      update({ typeName: fieldTypes[0].name });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [fieldTypes, form.typeName]);

  // Turning on "calculated" while a kind is selected would leave an invalid
  // pairing; fall back to the first value type.
  useEffect(() => {
    if (form.calculated && selected?.category === "kind") {
      update({ typeName: basicTypes[0]?.name ?? "" });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [form.calculated, selected, basicTypes]);

  // The chosen target table's own fields, so the target and summary selects
  // offer what that table actually has. Loaded here rather than from the tables
  // listing because `listTables` reports settings, not columns — and a table's
  // fields change under this screen as often as they are edited on it.
  useEffect(() => {
    const target = form.key.target_table;
    if (!target) {
      setTargetFields(null);
      return;
    }
    let cancelled = false;
    api
      .listFields(target)
      .then((list) => {
        if (cancelled) return;
        setTargetFields(list);
        // Re-check the selects against what the table has: a target field left
        // over from the previously chosen table would name a column of the
        // wrong one.
        setForm((f) =>
          f.key.target_table === target ? { ...f, key: reconcileKey(f.key, list) } : f,
        );
      })
      .catch(() => {
        if (!cancelled) setFormError(`Could not read the fields of “${target}”.`);
      });
    return () => {
      cancelled = true;
    };
  }, [form.key.target_table]);

  /** Open the modal on an empty form — never on what the last one left behind. */
  const openAdd = () => {
    setForm(newFieldForm(fieldTypes?.[0]?.name ?? ""));
    setFormError(null);
    setEditing({ mode: "add" });
  };

  /** Open the modal on a field that exists, seeded from what the server says it
   * is — so saving without touching anything is a no-op rather than a reset. */
  const openEdit = (field: FieldItem) => {
    setForm(fieldForm(field));
    setFormError(null);
    setEditing({ mode: "edit", field: field.name });
  };

  const save = async (e: FormEvent) => {
    e.preventDefault();
    if (!editing || !form.name.trim() || !selected) return;
    const invalid = fieldFormError(form, selected);
    if (invalid) {
      setFormError(invalid);
      return;
    }
    setBusy(true);
    setFormError(null);
    try {
      if (editing.mode === "edit") {
        await api.updateField(table, editing.field, updateFieldBody(form, selected));
      } else {
        await api.createField(table, createFieldBody(form, selected));
      }
      // The field is in the list behind the modal now, so the modal's work is
      // done. A refusal leaves it open, on the values that were refused.
      setEditing(null);
      onChange();
    } catch (err) {
      setFormError(
        errorMessage(
          err,
          editing.mode === "edit" ? "Could not save the field." : "Could not add the field.",
        ),
      );
    } finally {
      setBusy(false);
    }
  };

  /**
   * Drop a field: the column, its data and its settings row.
   *
   * The server's refusal is shown as it came rather than replaced with a generic
   * message — "it is referenced by `matters.client`" is the only version of
   * "no" that says what to do next.
   */
  const drop = async (field: string) => {
    if (!window.confirm(`Drop the field "${field}" and the data in it? This cannot be undone.`)) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await api.deleteField(table, field);
      onChange();
    } catch (err) {
      setError(errorMessage(err, "Could not drop the field."));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card className="mb-4">
      <Card.Header>Fields</Card.Header>
      <Card.Body>
        {error && <Alert variant="danger">{error}</Alert>}
        <Table size="sm" hover responsive className="table-vcenter mb-3">
          <thead>
            <tr>
              <th>Name</th>
              <th>Type</th>
              <th>Kind</th>
              <th>Nullable</th>
              <th className="text-end">Actions</th>
            </tr>
          </thead>
          <tbody>
            {fields?.length === 0 && (
              <tr>
                <td colSpan={5} className="text-muted">
                  No fields yet.
                </td>
              </tr>
            )}
            {fields?.map((f) => (
              <tr key={f.name}>
                <td>{f.name}</td>
                <td>
                  <code>{f.type}</code>
                </td>
                <td>{kindLabel(f.kind)}</td>
                <td>{f.nullable ? "yes" : "no"}</td>
                <td className="text-end">
                  <div className="btn-list justify-content-end flex-nowrap">
                    <Button
                      size="sm"
                      variant="outline-secondary"
                      disabled={busy}
                      onClick={() => openEdit(f)}
                    >
                      Edit
                    </Button>
                    <Button
                      size="sm"
                      variant="outline-danger"
                      disabled={busy}
                      onClick={() => void drop(f.name)}
                    >
                      Delete
                    </Button>
                  </div>
                </td>
              </tr>
            ))}
          </tbody>
        </Table>
        <Button size="sm" onClick={openAdd}>
          <IconPlus className="icon-2" />
          Add field
        </Button>

        <Modal show={editing !== null} onHide={() => setEditing(null)} size="lg" scrollable>
          {/* The form wraps the whole modal so that the footer's button is the
              form's submit and Return in a text box does what the button does. */}
          <Form onSubmit={save}>
            <Modal.Header closeButton>
              <Modal.Title className="h4">
                {isEdit ? `Edit ${table}.${form.name}` : `Add field to ${table}`}
              </Modal.Title>
            </Modal.Header>
            <Modal.Body>
              {formError && <Alert variant="danger">{formError}</Alert>}
              <Row>
                <Col md={6}>
                  <Form.Group className="mb-2" controlId="fieldName">
                    <Form.Label>Name</Form.Label>
                    <Form.Control
                      value={form.name}
                      autoFocus={!isEdit}
                      // A rename is a migration, which `updateField` does not do
                      // (§3.3) — shown rather than hidden, because the name is
                      // the first thing that says which field this is.
                      disabled={isEdit}
                      onChange={(e) => update({ name: e.target.value })}
                    />
                    {isEdit && <Form.Text muted>A field cannot be renamed.</Form.Text>}
                  </Form.Group>
                </Col>
                <Col md={6}>
                  <Form.Group className="mb-2" controlId="fieldLabel">
                    <Form.Label>Label</Form.Label>
                    <Form.Control
                      value={form.label}
                      placeholder={form.name}
                      onChange={(e) => update({ label: e.target.value })}
                    />
                    <Form.Text muted>Shown instead of the name. Blank uses the name.</Form.Text>
                  </Form.Group>
                </Col>
              </Row>
              <Form.Group className="mb-2" controlId="fieldDescription">
                <Form.Label>Description</Form.Label>
                <Form.Control
                  value={form.description}
                  onChange={(e) => update({ description: e.target.value })}
                />
              </Form.Group>
              <Form.Group className="mb-2" controlId="fieldType">
                <Form.Label>{form.calculated ? "Value type" : "Type"}</Form.Label>
                <Form.Select
                  value={form.typeName}
                  // A different type has different attributes, so what was
                  // entered for the last one is cleared rather than sent under
                  // names the new type does not have.
                  onChange={(e) => update({ typeName: e.target.value, attrs: {} })}
                >
                  {typeCategories.map((category) => {
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
                {form.calculated ? (
                  <Form.Text muted>
                    How the computed value is shown. Its real type comes from the expression.
                  </Form.Text>
                ) : (
                  isEdit && (
                    <Form.Text muted>
                      The column&apos;s storage is unchanged — this is how its value is read
                      and shown.
                    </Form.Text>
                  )
                )}
              </Form.Group>

              <Form.Check
                className="mb-2"
                id="fieldCalculated"
                type="checkbox"
                label="Calculated (computed on read, no stored column)"
                checked={form.calculated}
                // Whether a field has a column is settled when it is made:
                // turning this on would leave a column nothing reads, and off
                // would leave a field with no column at all.
                disabled={isEdit}
                onChange={(e) => update({ calculated: e.target.checked })}
              />

              {form.calculated ? (
                <Form.Group className="mb-1" controlId="fieldExpression">
                  <Form.Label>Formula</Form.Label>
                  <Form.Control
                    as="textarea"
                    rows={2}
                    className="font-monospace"
                    value={form.expression}
                    placeholder="pages * 2"
                    onChange={(e) => update({ expression: e.target.value })}
                  />
                  <Form.Text muted>
                    A JavaScript expression over the row&apos;s fields, Ⱶ-joinfields,
                    Ↄ-aggregations and other calculated fields — never <code>user</code> or the
                    operation flags.
                  </Form.Text>
                </Form.Group>
              ) : (
                <>
                  {selected?.name === "key" ? (
                    // A Key's parameters are the one set the spec-driven form cannot
                    // render: each depends on the one above it.
                    <KeyFields
                      tables={tables}
                      targetFields={targetFields}
                      value={form.key}
                      onChange={(key) => update({ key })}
                    />
                  ) : (
                    /* The chosen type's own attributes / a kind's parameters, rendered
                       from its spec — no per-type code lives here. */
                    <SettingsFields
                      spec={selected?.config_spec ?? []}
                      values={form.attrs}
                      onChange={(key, v) => setForm((f) => ({ ...f, attrs: { ...f.attrs, [key]: v } }))}
                      idPrefix="field-attr"
                    />
                  )}

                  <Form.Check
                    className="mb-1"
                    id="fieldNullable"
                    type="checkbox"
                    label="Nullable"
                    checked={form.nullable}
                    // A NOT NULL is the column's, and changing one on a table
                    // with rows in it is a migration (§3.3).
                    disabled={isEdit}
                    onChange={(e) => update({ nullable: e.target.checked })}
                  />
                  {isEdit && (
                    <Form.Text muted className="d-block">
                      Whether the column accepts nulls cannot be changed here.
                    </Form.Text>
                  )}
                </>
              )}
            </Modal.Body>
            <Modal.Footer>
              <Button variant="secondary" type="button" onClick={() => setEditing(null)}>
                Cancel
              </Button>
              <Button type="submit" disabled={busy || !form.name.trim() || !selected}>
                {isEdit ? "Save field" : "Add field"}
              </Button>
            </Modal.Footer>
          </Form>
        </Modal>
      </Card.Body>
    </Card>
  );
}

/**
 * The parameter form for a `Key` field (design §3.4): what it points at, and
 * what a row of the other table is shown as.
 *
 * Three selects rather than the spec-driven form, because the three settings are
 * not independent — the target and summary fields are fields *of the chosen
 * table*, and the storage type is the target field's, so it is reported rather
 * than asked. Choosing from what exists is also what makes the reference valid
 * by construction: a typed table or column name is a reference the server has to
 * refuse after the fact.
 */
function KeyFields({
  tables,
  targetFields,
  value,
  onChange,
}: {
  tables: ListTablesResponse | null;
  targetFields: ListFieldsResponse | null;
  value: KeyKind;
  onChange: (value: KeyKind) => void;
}) {
  const storage = keyStorage(value, targetFields ?? []);
  return (
    <>
      <Form.Group className="mb-2" controlId="fieldKeyTable">
        <Form.Label>
          Target table<span className="text-danger"> *</span>
        </Form.Label>
        <Form.Select
          value={value.target_table}
          // The other two selects are about to describe a different table, so
          // they are cleared here and re-defaulted once its fields arrive.
          onChange={(e) =>
            onChange({ target_table: e.target.value, target_field: "", summary_field: "" })
          }
        >
          <option value="">Choose a table…</option>
          {(tables ?? []).map((t) => (
            <option key={t.name} value={t.name}>
              {t.label || t.name}
            </option>
          ))}
        </Form.Select>
      </Form.Group>

      <Form.Group className="mb-2" controlId="fieldKeyField">
        <Form.Label>
          Target field<span className="text-danger"> *</span>
        </Form.Label>
        <Form.Select
          value={value.target_field}
          disabled={!value.target_table || !targetFields}
          onChange={(e) => onChange({ ...value, target_field: e.target.value })}
        >
          {!value.target_table && <option value="">Choose a table first</option>}
          {(targetFields ?? []).map((f) => (
            <option key={f.name} value={f.name}>
              {f.name}
              {f.primary_key ? " (primary key)" : f.unique ? " (unique)" : ""}
            </option>
          ))}
        </Form.Select>
        <Form.Text muted>
          The column this key points at — it must be unique.
          {storage && (
            <>
              {" "}
              Stored as <code>{storage}</code>, to match it.
            </>
          )}
        </Form.Text>
      </Form.Group>

      <Form.Group className="mb-3" controlId="fieldKeySummary">
        <Form.Label>Summary field</Form.Label>
        <Form.Select
          value={value.summary_field}
          disabled={!value.target_table || !targetFields}
          onChange={(e) => onChange({ ...value, summary_field: e.target.value })}
        >
          <option value="">—</option>
          {(targetFields ?? []).map((f) => (
            <option key={f.name} value={f.name}>
              {f.name}
            </option>
          ))}
        </Form.Select>
        <Form.Text muted>How a referenced row is shown. Optional.</Form.Text>
      </Form.Group>
    </>
  );
}
