// Rendering a `FormField[]` settings spec as a plain form.
//
// This is the shared half of the design's "settings as data" move (§6.2, §13.3).
// A framework declares its settings, a file-store backend declares its settings,
// and — post-MVP — actions, agents and model providers will too. All of them
// answer the same question in the same vocabulary, so the admin UI renders them
// with the same code and knows nothing about any particular one.
//
// It lives here rather than inside a screen because there are now two consumers
// (`ApplicationForm` and `FileStoreForm`), and a copy in each would be two places
// for the rendering of one vocabulary to drift. `ui/form-runtime` (§12) is the
// eventual home; until it exists this is the plain-form stand-in, and having a
// single stand-in is what makes replacing it a one-file change.

import Form from "react-bootstrap/Form";

/** One settings field, structurally matching the API's `form_field_schema`.
 *
 * Declared here rather than imported from a specific endpoint's response type,
 * so this module does not depend on whose settings it is rendering — which is
 * the whole point of the shared vocabulary. */
export type FieldSpec = {
  name: string;
  label: string;
  type: string;
  required: boolean;
  default?: unknown | null;
  options: unknown[];
};

/** Read a config value as a display string (config bags arrive as `unknown`). */
export function asString(value: unknown): string {
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  return "";
}

/** Read a stored config bag (an `unknown`) into the string map the form edits. */
export function readConfig(raw: unknown): Record<string, string> {
  const out: Record<string, string> = {};
  if (raw && typeof raw === "object") {
    for (const [key, value] of Object.entries(raw as Record<string, unknown>)) {
      out[key] = asString(value);
    }
  }
  return out;
}

/** Coerce the string form values back to the types the spec declares, dropping
 * empty optional settings.
 *
 * A *required* setting left empty is deliberately passed through rather than
 * defaulted or blocked here: the server validates against the same spec and its
 * message names the offending setting, which is a better error than anything the
 * form could invent, and it keeps one authority for what "valid" means. */
export function buildConfig(
  spec: FieldSpec[],
  values: Record<string, string>,
): Record<string, unknown> {
  const config: Record<string, unknown> = {};
  for (const field of spec) {
    const raw = values[field.name] ?? asString(field.default);
    if (field.type === "bool") {
      config[field.name] = raw === "true";
      continue;
    }
    if (raw.trim() === "") continue;
    config[field.name] = field.type === "int" ? Number(raw) : raw;
  }
  return config;
}

/** The initial form values for a spec: whatever is stored, else each field's
 * declared default. */
export function initialValues(
  spec: FieldSpec[],
  stored: Record<string, string>,
): Record<string, string> {
  const values: Record<string, string> = {};
  for (const field of spec) {
    values[field.name] = stored[field.name] ?? asString(field.default);
  }
  return values;
}

/** One setting rendered as a plain control: a select when it restricts options,
 * a checkbox/number/text otherwise. */
export function SettingField({
  field,
  value,
  onChange,
  idPrefix = "cfg",
}: {
  field: FieldSpec;
  value: string;
  onChange: (value: string) => void;
  idPrefix?: string;
}) {
  const controlId = `${idPrefix}-${field.name}`;
  if (field.options.length > 0) {
    return (
      <Form.Group className="mb-3" controlId={controlId}>
        <Form.Label>
          {field.label}
          {field.required && <span className="text-danger"> *</span>}
        </Form.Label>
        <Form.Select value={value} onChange={(e) => onChange(e.target.value)}>
          <option value="">—</option>
          {field.options.map((opt) => {
            const s = asString(opt);
            return (
              <option key={s} value={s}>
                {s}
              </option>
            );
          })}
        </Form.Select>
      </Form.Group>
    );
  }
  if (field.type === "bool") {
    return (
      <Form.Group className="mb-3" controlId={controlId}>
        <Form.Check
          type="checkbox"
          label={field.label}
          checked={value === "true"}
          onChange={(e) => onChange(e.target.checked ? "true" : "false")}
        />
      </Form.Group>
    );
  }
  return (
    <Form.Group className="mb-3" controlId={controlId}>
      <Form.Label>
        {field.label}
        {field.required && <span className="text-danger"> *</span>}
      </Form.Label>
      <Form.Control
        type={field.type === "int" ? "number" : "text"}
        value={value}
        required={field.required}
        onChange={(e) => onChange(e.target.value)}
      />
    </Form.Group>
  );
}

/** A whole settings spec rendered as a block of controls. */
export function SettingsFields({
  spec,
  values,
  onChange,
  idPrefix,
}: {
  spec: FieldSpec[];
  values: Record<string, string>;
  onChange: (name: string, value: string) => void;
  idPrefix?: string;
}) {
  return (
    <>
      {spec.map((field) => (
        <SettingField
          key={field.name}
          field={field}
          value={values[field.name] ?? asString(field.default)}
          onChange={(v) => onChange(field.name, v)}
          idPrefix={idPrefix}
        />
      ))}
    </>
  );
}
