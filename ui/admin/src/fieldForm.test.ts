/**
 * The field editor's round trip: a field as the server reports it, into the
 * form, back out as a request that says the same thing.
 *
 * This is the whole of what makes an Edit button safe. `updateField` states the
 * **whole** overlay — what is left out is cleared, not kept (§13.1) — so a form
 * that failed to read one part of a field back would silently drop it the moment
 * an admin opened the field to change something else. The claims worth pinning
 * down are therefore the ones where a field's identity is not where it looks:
 *
 *   - a File's parameters live on the `kind` object, not in `attributes`;
 *   - a Key names its target on the kind object, and reports "no summary" as
 *     `null` where the form's select holds "";
 *   - a calculated field's *type* is the value's display type, and its kind is
 *     the expression;
 *   - and a plain field with a rich type keeps the type, because dropping it
 *     would take the attributes with it.
 */

import { describe, expect, it } from "vitest";

import {
  createFieldBody,
  fieldForm,
  fieldFormError,
  newFieldForm,
  updateFieldBody,
  type FieldItem,
  type FieldTypeItem,
} from "./fieldForm";

/** A field as `listFields` reports it, with only what a test states set. */
function field(over: Partial<FieldItem>): FieldItem {
  return {
    name: "title",
    label: "",
    description: "",
    sql_type: "text",
    type: "text",
    nullable: true,
    required: false,
    unique: false,
    primary_key: false,
    kind: { type: "plain" },
    attributes: {},
    ...over,
  };
}

/** The field types this test picks from, as `listFieldTypes` reports them. */
const TEXT: FieldTypeItem = { name: "text", label: "Text", category: "basic", config_spec: [] };
const STRING: FieldTypeItem = {
  name: "string",
  label: "String",
  category: "rich",
  config_spec: [
    {
      name: "max_length",
      label: "Max length",
      type: "int",
      required: false,
      default: null,
      options: [],
      multiline: false,
      secret: false,
    },
  ],
};
const FILE: FieldTypeItem = {
  name: "file",
  label: "File",
  category: "kind",
  config_spec: [
    {
      name: "store",
      label: "Store",
      type: "string",
      required: true,
      default: null,
      options: [],
      multiline: false,
      secret: false,
    },
    {
      name: "folder",
      label: "Folder",
      type: "string",
      required: false,
      default: null,
      options: [],
      multiline: false,
      secret: false,
    },
    {
      name: "mime_allow",
      label: "Allowed types",
      type: "json",
      required: false,
      default: null,
      options: [],
      multiline: false,
      secret: false,
    },
  ],
};
const KEY: FieldTypeItem = { name: "key", label: "Key", category: "kind", config_spec: [] };

describe("reading a field into the form", () => {
  it("keeps a rich type and its attributes", () => {
    const form = fieldForm(
      field({ type: "string", label: "Book title", attributes: { max_length: 200 } }),
    );
    expect(form.typeName).toBe("string");
    expect(form.label).toBe("Book title");
    expect(form.attrs).toEqual({ max_length: "200" });
    expect(form.calculated).toBe(false);
  });

  it("names a File by its kind, and reads its parameters as the attribute form's", () => {
    const form = fieldForm(
      field({
        name: "cover",
        kind: { type: "file", store: "uploads", folder: "covers", mime_allow: ["image/png"] },
      }),
    );
    // The picker says "file" even though the column is text — that is what the
    // field *is*, and what the admin chose when it was made.
    expect(form.typeName).toBe("file");
    // The discriminator is not a parameter, so it is not offered as one.
    expect(form.attrs).toEqual({
      store: "uploads",
      folder: "covers",
      mime_allow: '[\n  "image/png"\n]',
    });
  });

  it("reads a Key's target, and its absent summary as the select's empty value", () => {
    const form = fieldForm(
      field({
        name: "author",
        type: "int8",
        kind: {
          type: "key",
          target_table: "people",
          target_field: "id",
          summary_field: null,
        },
      }),
    );
    expect(form.typeName).toBe("key");
    expect(form.key).toEqual({
      target_table: "people",
      target_field: "id",
      summary_field: "",
    });
  });

  it("shows a calculated field as its value type, with the formula", () => {
    const form = fieldForm(
      field({ name: "area", type: "float", kind: { type: "calc", expression: "w * h" } }),
    );
    expect(form.calculated).toBe(true);
    expect(form.typeName).toBe("float");
    expect(form.expression).toBe("w * h");
  });

  it("carries a NOT NULL column through as not nullable", () => {
    expect(fieldForm(field({ nullable: false, required: true })).nullable).toBe(false);
  });
});

describe("saving an edited field", () => {
  it("re-states a rich type, so editing an attribute does not drop the type", () => {
    const form = fieldForm(field({ type: "string", attributes: { max_length: 200 } }));
    expect(updateFieldBody({ ...form, attrs: { max_length: "100" } }, STRING)).toEqual({
      label: "",
      description: "",
      type: "string",
      attributes: { max_length: 100 },
    });
  });

  it("round-trips a File unchanged", () => {
    const stored = field({
      name: "cover",
      label: "Cover",
      kind: { type: "file", store: "uploads", folder: "covers", mime_allow: ["image/png"] },
    });
    expect(updateFieldBody(fieldForm(stored), FILE)).toEqual({
      label: "Cover",
      description: "",
      type: "text",
      kind: {
        type: "file",
        store: "uploads",
        folder: "covers",
        mime_allow: ["image/png"],
      },
    });
  });

  it("round-trips a Key unchanged, and still omits an empty summary", () => {
    const stored = field({
      name: "author",
      type: "int8",
      kind: { type: "key", target_table: "people", target_field: "id", summary_field: null },
    });
    // No `type`: a key is stored as whatever its target is, which is the
    // server's to derive rather than this form's to re-assert.
    expect(updateFieldBody(fieldForm(stored), KEY)).toEqual({
      label: "",
      description: "",
      kind: { type: "key", target_table: "people", target_field: "id" },
    });
  });

  it("names a basic type rather than clearing it, and sends no attributes", () => {
    const form = fieldForm(field({ label: "Title", description: "The book's title" }));
    expect(updateFieldBody(form, TEXT)).toEqual({
      label: "Title",
      description: "The book's title",
      type: "text",
    });
  });

  it("never sends the name or the nullability, which an edit cannot change", () => {
    const body = updateFieldBody(fieldForm(field({ nullable: false })), TEXT);
    expect(body).not.toHaveProperty("name");
    expect(body).not.toHaveProperty("required");
  });
});

describe("adding a field", () => {
  it("sends the name, and required as the opposite of nullable", () => {
    const form = { ...newFieldForm("text"), name: "  title  ", nullable: false };
    expect(createFieldBody(form, TEXT)).toEqual({
      name: "title",
      label: "",
      description: "",
      required: true,
      type: "text",
    });
  });

  it("never makes a calculated field required — there is no column to constrain", () => {
    const form = {
      ...newFieldForm("float"),
      name: "area",
      nullable: false,
      calculated: true,
      expression: " w * h ",
    };
    expect(createFieldBody(form, { ...TEXT, name: "float" })).toEqual({
      name: "area",
      label: "",
      description: "",
      required: false,
      type: "float",
      kind: { type: "calc", expression: "w * h" },
    });
  });
});

describe("what the form itself refuses", () => {
  it("wants a formula for a calculated field and a target for a key", () => {
    const calc = { ...newFieldForm("text"), name: "area", calculated: true };
    expect(fieldFormError(calc, TEXT)).toMatch(/formula/);
    expect(fieldFormError({ ...calc, expression: "w * h" }, TEXT)).toBeNull();

    const key = { ...newFieldForm("key"), name: "author" };
    expect(fieldFormError(key, KEY)).toMatch(/table and a field/);
    expect(
      fieldFormError(
        { ...key, key: { target_table: "people", target_field: "id", summary_field: "" } },
        KEY,
      ),
    ).toBeNull();
  });

  it("passes everything else to the server, which is the authority on it", () => {
    // A max_length of "nonsense" is a mistake, and the server's message names
    // the attribute — this form's job is only to send what was typed.
    const form = { ...newFieldForm("string"), name: "title", attrs: { max_length: "nonsense" } };
    expect(fieldFormError(form, STRING)).toBeNull();
  });
});
