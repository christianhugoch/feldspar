// The **type declarations** the code editor reads: `db`, its fluent chain, and
// the event's own bindings, as TypeScript.
//
// A `run_js_code` body is edited in Monaco (`CodeEditor.tsx`), and an editor
// with no types is a text area with brackets: completions are the point. So the
// editor is handed an ambient `.d.ts` describing exactly what a code body can
// reach — `db.invoices.where(…).rows()`, `row`, `old`, `user`, `payload` — with
// this server's own tables and columns in it.
//
// **The declarations are built in the browser, from the catalog the admin UI
// already reads.** The alternative was a server endpoint emitting the same text,
// which is the better home for it the moment a second consumer appears (a
// non-JavaScript adapter, §15). Today there is one consumer, and building it here
// costs no endpoint, no schema and no generated-client churn — while `listTables`
// and `listFields` are already the API this screen uses.
//
// The shape of the chain is the other half, and it is a **transcription** of
// `sc-expr`'s `DB_PRELUDE` (the JavaScript that implements `db`), the operators
// `sc-api`'s filter module accepts, and the results `sc-api::code_host` returns.
// Those live in Rust and this is a copy of what they do, so it can be *close* to
// correct and not provably so; the comments below name what each part is a copy
// of. Nothing type-checks against it — the editor reports no diagnostics on a
// body (see `CodeEditor.tsx`) — so a declaration that drifts costs a wrong
// completion, never a refused save.

import { api } from "./api";

/** One column of a table, as the declarations need it. */
export type ColumnInfo = {
  name: string;
  /** The catalog's type name (`text`, `int`, a rich type's name). */
  type: string;
  /** The SQL type, which a rich type falls back to. */
  sqlType: string;
  /** Whether a write must supply it — what makes it non-optional on an insert. */
  required: boolean;
  /** The table a key field points at, for the `Ⱶ` join columns. */
  keyTo?: string;
};

/** One table: its name and its columns. */
export type TableInfo = { name: string; columns: ColumnInfo[] };

/** What the screen knows about the event a body will run in.
 *
 * `table` is the trigger's table (a row event) and `undefined` for every kind
 * that has no row — which is exactly the difference between `row` being declared
 * and not being declared at all, because that is the difference in the sandbox
 * (`run_js_code`'s `bindings`: naming `row` in a `login` body is a
 * `ReferenceError`, not a null). */
export type CodeScope = { table?: string; event?: string };

/** The join separator between a key field and a column of the table it points
 * at (`customerⱵemail`) — `sc_expr::JOIN`. */
const JOIN = "Ⱶ";

/** The TypeScript type a column's values arrive as, over the JSON boundary.
 *
 * Rich types are not enumerated: they are stored as one of the basic types and
 * the SQL type is what says which, so an unknown type name falls through to the
 * column's `sql_type` and then to `unknown` — a wrong-but-quiet `unknown` being
 * better in an editor than a confidently wrong `string`. */
export function columnType(column: ColumnInfo): string {
  const scalar = (name: string): string | null => {
    switch (name) {
      case "bool":
      case "boolean":
        return "boolean";
      case "int":
      case "integer":
      case "bigint":
      case "smallint":
      case "float":
      case "double precision":
      case "real":
        return "number";
      // A decimal crosses as a string: it is exact, and a JavaScript number is
      // not. Same for the date/time family, which arrive as ISO strings.
      case "decimal":
      case "numeric":
      case "text":
      case "uuid":
      case "date":
      case "time":
      case "timestamp":
      case "timestamptz":
      case "character varying":
        return "string";
      case "json":
      case "jsonb":
        return "unknown";
      default:
        return null;
    }
  };
  const type = scalar(column.type) ?? scalar(column.sqlType) ?? "unknown";
  return column.required ? type : `${type} | null`;
}

/** A TypeScript identifier for a table's generated interfaces (`order_lines` →
 * `OrderLines`). Two tables cannot collide: a name that is not already unique
 * keeps enough of itself to stay so, because non-identifier characters become
 * `_` rather than being dropped. */
export function typeName(table: string): string {
  const cleaned = table.replace(/[^A-Za-z0-9_]/g, "_");
  const camel = cleaned
    .split("_")
    .map((part) => (part === "" ? "_" : part[0].toUpperCase() + part.slice(1)))
    .join("");
  return /^[A-Za-z]/.test(camel) ? camel : `T${camel}`;
}

/** How a property name is written in an interface: bare when it is an
 * identifier, quoted otherwise — which every `Ⱶ` join column is. */
function propertyKey(name: string): string {
  return /^[A-Za-z_$][A-Za-z0-9_$]*$/.test(name) ? name : JSON.stringify(name);
}

/** A string literal, for the column unions. */
function literal(value: string): string {
  return JSON.stringify(value);
}

/** Every column name a query on `table` may name: its own columns, plus one
 * `keyⱵcolumn` per column of each table its key fields point at.
 *
 * One level deep on purpose. The server resolves a path of any length
 * (`join_path_expr`), but the completions that matter are the first hop, and
 * every further hop multiplies the union by another table's width. */
export function columnNames(table: TableInfo, tables: TableInfo[]): string[] {
  const names = table.columns.map((c) => c.name);
  for (const column of table.columns) {
    const target = tables.find((t) => t.name === column.keyTo);
    if (!target) continue;
    for (const far of target.columns) names.push(`${column.name}${JOIN}${far.name}`);
  }
  return names;
}

/** The part of the declarations that is the same on every server: the value
 * types, the filter DSL, and the chain itself.
 *
 * Transcribed from `sc-expr`'s `DB_PRELUDE` (which methods exist and what they
 * return), `sc_api::filter::OPERATORS` (the comparisons), and
 * `sc_api::code_host` (the shape a write answers with). */
export function chainDeclarations(): string {
  return `/** A value a column can hold, as it crosses into the sandbox. */
type ScValue = string | number | boolean | null;

/** One row, when its columns are not known ahead of time.
 *
 * \`any\` rather than \`unknown\` wherever a value is genuinely undeclared —
 * here, in the open end of a row interface, in \`user\`'s other columns and in
 * \`payload\`. Nothing declares what a sender put in a payload or what an alias
 * in a \`.select()\` produced, and \`unknown\` would state that by making every
 * ordinary use of it (\`payload.today\` inside a comparison) an error. The
 * editor reports no errors, so what this really decides is whether hovering one
 * says something useful; "not declared" is the true answer either way. */
type ScRow = Record<string, any>;

/** A comparison on one column. The operators are the ones every Saltcorn
 * surface speaks — a REST query string, an agent's tool, this chain. */
interface ScCompare {
  eq?: ScValue;
  ne?: ScValue;
  gt?: ScValue;
  gte?: ScValue;
  lt?: ScValue;
  lte?: ScValue;
  in?: ScValue[];
  nin?: ScValue[];
  /** SQL \`LIKE\` pattern: \`%\` is any run of characters. */
  like?: string;
  /** \`LIKE\`, ignoring case. */
  ilike?: string;
  is_null?: boolean;
}

/** What \`.where()\` takes: the object DSL, or a formula written as a string
 * (\`"pages > 200 && !paid"\`). Both mean the same predicate. */
type ScWhere<Col extends string> =
  | string
  | ({ [K in Col]?: ScValue | ScCompare } & {
      and?: ScWhere<Col>[];
      or?: ScWhere<Col>[];
      not?: ScWhere<Col>;
      /** The formula spelling, inside an object. */
      formula?: string;
    });

/** What \`.select()\` takes: a column name (or \`keyⱵcolumn\` join path), or an
 * object of alias → formula, which is how a computed or aggregated column
 * (\`{ chased: "remindersↃinvoice.length" }\`) is asked for. */
type ScProjection<Col extends string> = Col | Record<string, string>;

/** What a bulk \`.update()\` or \`.delete()\` answers: how many rows, and which. */
interface ScWriteResult {
  updated?: number;
  deleted?: number;
  ids: ScValue[];
}

/** A query over one table. Every chain method is pure and returns a new query;
 * the **terminals** below execute, each one sending a single statement.
 *
 * Bounded, and the bounds are named errors rather than truncations: 1000 rows
 * per read, 200 database calls per run, and the trigger's \`timeout_ms\` wall
 * clock. */
interface ScQuery<Row, Col extends string> {
  /** Narrow the rows. Repeated calls are ANDed. */
  where(condition: ScWhere<Col>): ScQuery<Row, Col>;
  /** Choose the columns, instead of the whole row. */
  select(...columns: ScProjection<Col>[]): ScQuery<Row, Col>;
  /** Order by a column, ascending unless told otherwise. */
  orderBy(field: Col, direction?: "asc" | "desc"): ScQuery<Row, Col>;
  groupBy(...fields: Col[]): ScQuery<Row, Col>;
  limit(n: number): ScQuery<Row, Col>;
  offset(n: number): ScQuery<Row, Col>;
  /** Run what follows as the person who caused the event: their ownership rule
   * decides every row, and a write they may not make is a catchable error. */
  asUser(): ScQuery<Row, Col>;
  /** Run what follows as the server (the default). */
  asAdmin(): ScQuery<Row, Col>;

  /** The matching rows. Synchronous — there are no promises in the sandbox. */
  rows(): Row[];
  /** The first matching row, or null. */
  first(): Row | null;
  /** The row with this primary key, or null. */
  get(pk: ScValue): Row | null;
  exists(): boolean;
  count(): number;
  sum(field: Col): number | null;
  avg(field: Col): number | null;
  min(field: Col): ScValue;
  max(field: Col): ScValue;

  /** Insert a row and return it as stored — coerced, calculated columns filled
   * in, and the table's own triggers fired. */
  insert(values: Partial<Row>): Row;
  insert(values: Partial<Row>[]): Row[];
  /** Update every row the \`.where()\` matched. A \`.where()\` is required: an
   * omitted one would rewrite the table. */
  update(values: Partial<Row>): ScWriteResult;
  /** Delete every row the \`.where()\` matched. A \`.where()\` is required. */
  delete(): ScWriteResult;
}

/** A query over a table named at runtime, whose columns are therefore not
 * known here. */
type ScAnyQuery = ScQuery<ScRow, string>;

/** The signed-in person who caused the event, or null.
 *
 * \`id\` and \`role\` are always there; every other column of the users table
 * comes with them. */
interface ScUser {
  id: string;
  role: number;
  email?: string;
  [column: string]: any;
}
`;
}

/** The declarations for this server's tables: a row interface and a column union
 * per table, and the `db` handle carrying one property per table. */
export function tableDeclarations(tables: TableInfo[]): string {
  const parts: string[] = [];
  for (const table of tables) {
    const name = typeName(table.name);
    const fields = table.columns
      .map((c) => `  ${propertyKey(c.name)}: ${columnType(c)};`)
      .join("\n");
    parts.push(
      `/** A row of \`${table.name}\`. */\ninterface ${name}Row {\n${fields}\n` +
        // A `.select()` with an alias, or a Ⱶ join column, puts keys here that
        // the table itself does not have. Left open rather than enumerated, so
        // reading one is quiet instead of wrong.
        `  [column: string]: any;\n}\n`,
    );
    const columns = columnNames(table, tables).map(literal).join(" | ");
    parts.push(
      `/** A column of \`${table.name}\`, or a \`keyⱵcolumn\` path from it. */\n` +
        `type ${name}Column = ${columns || "never"};\n`,
    );
  }

  const properties = tables
    .map(
      (t) =>
        `  /** The \`${t.name}\` table. */\n` +
        `  ${propertyKey(t.name)}: ScQuery<${typeName(t.name)}Row, ${typeName(t.name)}Column>;`,
    )
    .join("\n");
  // `(string & {})` keeps the literal completions while still accepting a name
  // computed at runtime — `db.table(payload.which)` is legitimate.
  const names = tables.map((t) => literal(t.name)).join(" | ");
  const tableName = names === "" ? "string" : `${names} | (string & {})`;

  parts.push(
    `/** The tables, read and written from a code body. */\ninterface ScDb {\n` +
      `  /** Any table, by name — the general form of \`db.<table>\`. */\n` +
      `  table(name: ${tableName}): ScAnyQuery;\n` +
      `  /** Delegate everything that follows to the person who caused the event. */\n` +
      `  asUser(): ScDb;\n` +
      `  /** Act as the server (the default). */\n` +
      `  asAdmin(): ScDb;\n${properties}\n}\n`,
  );
  return parts.join("\n");
}

/** The declarations for the event's own bindings, which depend on the trigger.
 *
 * Presence is the whole point: a binding this event does not have is left out,
 * because naming it in the sandbox is a `ReferenceError` and an editor that
 * completed it would be promising something the run refuses. */
export function scopeDeclarations(scope: CodeScope, tables: TableInfo[]): string {
  const parts: string[] = [];
  const table = tables.find((t) => t.name === scope.table);
  if (table) {
    const row = `${typeName(table.name)}Row`;
    parts.push(`/** The \`${table.name}\` row the event is about. */\ndeclare const row: ${row};`);
    parts.push(
      `/** The row as it was before this event — null on an insert. */\n` +
        `declare const old: ${row} | null;`,
    );
  } else if (scope.table !== undefined) {
    // A table event whose table has no declarations (it was dropped, or the
    // fetch for it failed): the bindings still exist, so declare them loosely
    // rather than leaving them undeclared and completing nothing.
    parts.push(`/** The row the event is about. */\ndeclare const row: ScRow;`);
    parts.push(`/** The row as it was before this event — null on an insert. */\ndeclare const old: ScRow | null;`);
  }
  parts.push(
    `/** Whoever caused the event, or null for the server's own events. */\n` +
      `declare const user: ScUser | null;`,
  );
  parts.push(
    `/** What the trigger was called with: the body posted to a directly-run\n` +
      ` * trigger, or what the event carried. */\n` +
      `declare const payload: Record<string, any>;`,
  );
  parts.push(
    `/** The tables. Only a code body has this — a formula (an \`only if\`, an\n` +
      ` * ownership rule) evaluates without it. */\ndeclare const db: ScDb;`,
  );
  return `${parts.join("\n\n")}\n`;
}

/** The whole ambient library handed to the editor. */
export function codeLibrary(tables: TableInfo[], scope: CodeScope): string {
  return [
    "// The Saltcorn code sandbox, as types. Generated by the admin UI from this",
    "// server's tables; not a file in any project.",
    "",
    chainDeclarations(),
    tableDeclarations(tables),
    scopeDeclarations(scope, tables),
  ].join("\n");
}

/** The catalog the declarations are built from, read once per page.
 *
 * Cached as the *promise*, so two editors opening at once make one round of
 * requests. Not invalidated: a table added in another tab changes what a body
 * can reach, and the admin reloads to see it — the cost of being wrong is a
 * missing completion, and the cost of re-reading the whole catalog on every
 * keystroke-adjacent event is worse. */
let catalogCache: Promise<TableInfo[]> | null = null;

/** Read every table and its fields.
 *
 * One request per table, in parallel, because that is the API the admin UI has
 * (`listFields` is per-table). A table whose fields cannot be read is kept with
 * no columns rather than dropped: `db.<name>` still exists in the sandbox, so it
 * should still exist in the completions. */
export async function loadCatalog(): Promise<TableInfo[]> {
  const tables = await api.listTables();
  return await Promise.all(
    tables.map(async (table): Promise<TableInfo> => {
      try {
        const fields = await api.listFields(table.name);
        return {
          name: table.name,
          columns: fields.map((field) => {
            const kind = field.kind as { type?: string; target_table?: string } | null;
            return {
              name: field.name,
              type: field.type,
              sqlType: field.sql_type,
              required: field.required,
              keyTo: kind?.type === "key" ? kind.target_table : undefined,
            };
          }),
        };
      } catch {
        return { name: table.name, columns: [] };
      }
    }),
  );
}

/** [`loadCatalog`] once per page. */
export function catalog(): Promise<TableInfo[]> {
  catalogCache ??= loadCatalog();
  return catalogCache;
}
