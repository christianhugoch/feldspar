/**
 * The declarations the code editor loads are only worth having if they are
 * **true of the sandbox**, so this test does not check that the generator emits
 * particular strings — it hands what it emits to the TypeScript compiler,
 * together with a body written the way `run_js_code`'s own documentation writes
 * one, and asserts that the body type-checks against it.
 *
 * That is the property that matters. A declaration file with a typo in it, a
 * chain method that returns the wrong builder, a row interface a real body
 * cannot use: all of them are compile errors here, and all of them would
 * otherwise be a wrong completion in front of an admin.
 *
 * The counter-test matters as much: a body that misspells a method or reads a
 * column no table has must *fail*, or the declarations would be a rubber stamp.
 */

import ts from "typescript";
import { describe, expect, it } from "vitest";

import {
  codeLibrary,
  columnNames,
  columnType,
  typeName,
  type TableInfo,
} from "./codeTypes";

/** Two tables with a key between them: enough for a row type, a `Ⱶ` join
 * column, and an aggregation over the child. */
const TABLES: TableInfo[] = [
  {
    name: "invoices",
    columns: [
      { name: "id", type: "int", sqlType: "integer", required: true },
      { name: "amount", type: "float", sqlType: "double precision", required: true },
      { name: "paid", type: "bool", sqlType: "boolean", required: true },
      { name: "due", type: "date", sqlType: "date", required: false },
      {
        name: "customer",
        type: "int",
        sqlType: "integer",
        required: false,
        keyTo: "people",
      },
    ],
  },
  {
    name: "people",
    columns: [
      { name: "id", type: "int", sqlType: "integer", required: true },
      { name: "email", type: "text", sqlType: "text", required: true },
      { name: "signed_up", type: "timestamp", sqlType: "timestamptz", required: false },
    ],
  },
];

/** What one compilation is given: the ES library the sandbox really has (no DOM,
 * no Node) and nothing else — in particular none of this project's own `@types`,
 * which are not in the sandbox and would only be found because the test happens
 * to run inside `ui/admin`. */
const OPTIONS: ts.CompilerOptions = {
  allowJs: true,
  checkJs: true,
  noEmit: true,
  strict: true,
  target: ts.ScriptTarget.ES2022,
  lib: ["lib.es2022.d.ts"],
  types: [],
};

/** Compile one code body against the generated declarations, and return the
 * errors as their messages.
 *
 * `checkJs` is on here and **off in the editor** (see `CodeEditor.tsx`), and
 * deliberately so: this test wants every disagreement between the declarations
 * and a body reported, while an admin wants completions without a type-checker
 * arguing about a sandbox it cannot see. Same declarations, two readers.
 *
 * The files exist only in memory. Writing them to a temporary directory would
 * pull `node:fs` into a suite whose only other dependency is the code under
 * test, and the compiler takes a host, so there is no reason to.
 */
function check(body: string, tables: TableInfo[] = TABLES): string[] {
  const files: Record<string, string> = {
    "sandbox.d.ts": codeLibrary(tables, { table: "invoices", event: "insert" }),
    // A code body is the inside of a function: `return` at the top level is what
    // the action runs, so it is wrapped for the compiler exactly as the runtime
    // wraps it.
    "body.js": `function __body() {\n${body}\n}\n`,
  };
  // The installed TypeScript's own library files, by the absolute path it
  // reports for them.
  const defaultLib = ts.getDefaultLibFilePath(OPTIONS);
  const host: ts.CompilerHost = {
    fileExists: (name) => name in files || ts.sys.fileExists(name),
    readFile: (name) => files[name] ?? ts.sys.readFile(name),
    getSourceFile: (name, languageVersion) => {
      const text = files[name] ?? ts.sys.readFile(name);
      return text === undefined
        ? undefined
        : ts.createSourceFile(name, text, languageVersion, true);
    },
    getDefaultLibFileName: () => defaultLib,
    writeFile: () => undefined,
    getCurrentDirectory: () => "",
    getCanonicalFileName: (name) => name,
    useCaseSensitiveFileNames: () => true,
    getNewLine: () => "\n",
  };
  const program = ts.createProgram(Object.keys(files), OPTIONS, host);
  return ts
    .getPreEmitDiagnostics(program)
    .map((d) => ts.flattenDiagnosticMessageText(d.messageText, " "));
}

describe("the types the code editor loads", () => {
  it("type-check the milestone's own example body", () => {
    // Verbatim from `run_js_code`'s documentation and the TODO's definition of
    // done, which is the body these declarations exist to support.
    const errors = check(`
      const overdue = db.invoices
        .where({ paid: false, due: { lt: payload.today } })
        .select("id", "amount", "customerⱵemail", { chased: "remindersↃinvoice.length" })
        .orderBy("due")
        .limit(50)
        .rows();

      for (const inv of overdue) {
        db.people.insert({ email: String(inv["customerⱵemail"]) });
      }
      return { chased: overdue.length, owed: db.invoices.where({ paid: false }).sum("amount") };
    `);
    expect(errors).toEqual([]);
  });

  it("cover the rest of the chain, both authorities and every terminal", () => {
    expect(
      check(`
        const one = db.table("invoices").where("amount > 100").first();
        const mine = db.asUser().invoices.where({ paid: true }).exists();
        const n = db.invoices.where({ due: { is_null: true } }).count();
        const top = db.invoices.orderBy("amount", "desc").limit(1).offset(0).rows();
        const person = db.people.get(1);
        const written = db.people.insert([{ email: "a@b.c" }, { email: "d@e.f" }]);
        const updated = db.invoices.where({ paid: false }).asUser().update({ paid: true });
        const gone = db.people.where({ email: { ilike: "%@example.com" } }).delete();
        return [one, mine, n, top, person, written.length, updated.ids, gone.deleted];
      `),
    ).toEqual([]);
  });

  it("declare the event's own bindings, with the row typed by the trigger's table", () => {
    expect(
      check(`
        const amount = row.amount + (old === null ? 0 : old.amount);
        const who = user === null ? "nobody" : user.email;
        return { amount, who, sent: payload.sent };
      `),
    ).toEqual([]);
  });

  it("declare no row where the event has none", () => {
    // A `login` trigger's body naming `row` is a ReferenceError in the sandbox,
    // so the editor must not complete it either.
    const library = codeLibrary(TABLES, { event: "login" });
    expect(library).not.toContain("declare const row");
    expect(library).toContain("declare const user");
    expect(library).toContain("declare const payload");
    expect(library).toContain("declare const db");
  });

  it("refuse what the sandbox would refuse", () => {
    // Each of these is a mistake an admin can make, and each is a case where a
    // completion list that offered it would be lying.
    expect(check(`return db.invoices.rowz();`).join(" ")).toMatch(/rowz/);
    expect(check(`return db.invoicez.rows();`).join(" ")).toMatch(/invoicez/);
    expect(check(`return db.invoices.orderBy("nope").rows();`).join(" ")).toMatch(/nope/);
    expect(check(`return db.invoices.where({ paid: { gtt: 1 } }).rows();`).join(" ")).toMatch(
      /gtt/,
    );
    // `.update()` and `.delete()` answer a count and ids, not rows.
    expect(
      check(`return db.invoices.where({ paid: false }).update({ paid: true }).length;`).join(" "),
    ).toMatch(/length/);
  });

  it("survive a table with no readable fields", () => {
    // `listFields` can fail for one table while the rest load. The table is kept
    // — `db.<name>` exists in the sandbox whatever this UI knows — so what must
    // hold is that the declarations still compile.
    expect(check(`return db.mystery;`, [...TABLES, { name: "mystery", columns: [] }])).toEqual(
      [],
    );
  });
});

describe("the pieces the declarations are built from", () => {
  it("map a column to the type its values cross as", () => {
    const column = (over: Partial<TableInfo["columns"][number]>) =>
      columnType({ name: "c", type: "text", sqlType: "text", required: true, ...over });
    expect(column({})).toBe("string");
    expect(column({ type: "int", sqlType: "integer" })).toBe("number");
    expect(column({ type: "bool", sqlType: "boolean" })).toBe("boolean");
    // Exact by nature and a JavaScript number is not, so it crosses as a string.
    expect(column({ type: "decimal", sqlType: "numeric" })).toBe("string");
    // A rich type is stored as one of the basic types: the SQL type is what says
    // which, and an unknown one stays honestly unknown.
    expect(column({ type: "Email", sqlType: "text" })).toBe("string");
    expect(column({ type: "Weather", sqlType: "geography" })).toBe("unknown");
    // Nullability is the column's, not the type's.
    expect(column({ required: false })).toBe("string | null");
  });

  it("name the join columns a key field reaches", () => {
    const names = columnNames(TABLES[0], TABLES);
    expect(names).toContain("amount");
    expect(names).toContain("customerⱵemail");
    expect(names).toContain("customerⱵsigned_up");
    // One hop only: the union is a completion list, and every further hop
    // multiplies it by another table's width.
    expect(names.some((n) => n.split("Ⱶ").length > 2)).toBe(false);
  });

  it("turn a table name into an identifier that stays unique", () => {
    expect(typeName("invoices")).toBe("Invoices");
    expect(typeName("order_lines")).toBe("OrderLines");
    // Non-identifier characters become `_` rather than disappearing, so two
    // tables cannot collapse onto one interface name.
    expect(typeName("order lines")).not.toBe(typeName("orderlines"));
    expect(typeName("2024_totals")).toMatch(/^[A-Za-z]/);
  });
});
