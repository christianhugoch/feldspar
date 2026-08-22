// The model behind the "Connect a database" dialog — the half of it that is not
// React.
//
// A database connection is a handful of boxes that are either all right or
// silently wrong: get the host and the port right and the password wrong, and
// what you get is not a form error but an empty table list. So the rules a test
// can pin without a browser live here — when Connect may be pressed, what an
// empty port box means, and how the connection reads back as one line in the
// list.
//
// There are two kinds of database behind one form. A **postgres** connection is
// a server: host, port, database, user, password, schema. A **sqlite**
// connection is a file, so it is named the way every other file is — a file
// store and a path inside it — and none of the server boxes apply to it. The
// rules below branch on the backend rather than validating boxes the chosen
// backend does not have.

/** Which kind of database a connection is. */
export type DbBackend = "postgres" | "sqlite";

/** What the dialog holds while it is open. */
export type DbConnectionForm = {
  /** The row being edited, or null when this is a new connection. */
  id: string | null;
  name: string;
  description: string;
  backend: DbBackend;
  host: string;
  /** Kept as text, because an empty box is a real state and `0` is not it. */
  port: string;
  database: string;
  username: string;
  password: string;
  schema: string;
  /** For a SQLite connection: the file store the database file is in. */
  fileStore: string;
  /** For a SQLite connection: the path of the file inside that store. */
  filePath: string;
};

/** The port a Postgres server listens on unless told otherwise. */
export const DEFAULT_PORT = 5432;
/** The schema a connection presents unless the admin names another. */
export const DEFAULT_SCHEMA = "public";
/** What the server sends instead of a stored password, and accepts back
 * unchanged to mean "I did not touch it" (design §11.1). */
export const SECRET_SENTINEL = "••••••••";

/** A dialog just opened on a new connection. */
export const EMPTY_DB_CONNECTION_FORM: DbConnectionForm = {
  id: null,
  name: "",
  description: "",
  backend: "postgres",
  host: "localhost",
  port: String(DEFAULT_PORT),
  database: "",
  username: "",
  password: "",
  schema: DEFAULT_SCHEMA,
  fileStore: "",
  filePath: "",
};

/** A connection as the server reports it, narrowed to what this module reads. */
export type DbConnectionRow = {
  id: string;
  name: string;
  description: string;
  backend?: string;
  host: string;
  port: number;
  database: string;
  username: string;
  password: string;
  schema: string;
  file_store?: string;
  file_path?: string;
  connected: boolean;
  error?: string | null;
  tables: number;
  shadowed: Array<string>;
};

/** The dialog opened on an existing connection.
 *
 * The password arrives as the sentinel and is put straight into the box, so
 * saving without touching it sends the sentinel back and the server restores
 * what is stored. An empty password stays empty: "no password" is a real state
 * and showing dots for it would claim one exists. */
export function formFromConnection(row: DbConnectionRow): DbConnectionForm {
  return {
    id: row.id,
    name: row.name,
    description: row.description,
    backend: backendOf(row),
    host: row.host,
    port: String(row.port),
    database: row.database,
    username: row.username,
    password: row.password,
    schema: row.schema,
    fileStore: row.file_store ?? "",
    filePath: row.file_path ?? "",
  };
}

/** Which kind of database a row is. A row that names none is Postgres, which is
 * what every connection was before there was a second kind. */
export function backendOf(row: DbConnectionRow): DbBackend {
  return row.backend === "sqlite" ? "sqlite" : "postgres";
}

/** The body to send for this form: the port as a number, blanks trimmed.
 *
 * The boxes the chosen backend does not use are sent **empty** rather than with
 * whatever the other backend's form last held — a SQLite connection carrying a
 * leftover `localhost` would read back, in the list and in the row, as a server
 * it has nothing to do with. */
export function connectionBody(form: DbConnectionForm) {
  const sqlite = form.backend === "sqlite";
  return {
    name: form.name.trim(),
    description: form.description.trim(),
    backend: form.backend,
    host: sqlite ? "" : form.host.trim(),
    port: sqlite ? 0 : portOf(form),
    database: sqlite ? "" : form.database.trim(),
    username: sqlite ? "" : form.username.trim(),
    password: sqlite ? "" : form.password,
    schema: sqlite ? "" : form.schema.trim() || DEFAULT_SCHEMA,
    file_store: sqlite ? form.fileStore.trim() : "",
    file_path: sqlite ? form.filePath.trim() : "",
  };
}

/** The port this form means: what was typed, or the Postgres default when the
 * box was left empty. An admin who cleared the box meant "the usual one", not
 * "port zero". */
export function portOf(form: DbConnectionForm): number {
  const typed = form.port.trim();
  if (!typed) return DEFAULT_PORT;
  const n = Number(typed);
  return Number.isFinite(n) ? n : DEFAULT_PORT;
}

/**
 * Why this form cannot be submitted yet, or `null` when it can.
 *
 * A message rather than a boolean, for the reason `newTableError` gives one:
 * the same answer disables the button and says what is missing.
 *
 * `primary` is refused here as well as on the server. It is the name every
 * table of Saltcorn's own database carries, so a connection holding it would
 * make a table's origin ambiguous — and being told that while the dialog is
 * still open beats being told it by a failed save.
 */
export function dbConnectionError(form: DbConnectionForm): string | null {
  const name = form.name.trim();
  if (!name) return "The connection needs a name.";
  if (name === "primary") {
    return "`primary` is the name of Saltcorn's own database. Choose another name.";
  }
  if (form.backend === "sqlite") {
    if (!form.fileStore.trim()) {
      return "Choose the file store the SQLite file is in.";
    }
    if (!form.filePath.trim()) return "The connection needs the path of its SQLite file.";
    return null;
  }
  if (!form.host.trim()) return "The connection needs a host.";
  if (!form.database.trim()) return "The connection needs a database name.";
  if (!form.username.trim()) return "The connection needs a user to connect as.";
  const typed = form.port.trim();
  if (typed && !/^\d+$/.test(typed)) return "The port must be a number.";
  if (typed && (Number(typed) < 1 || Number(typed) > 65535)) {
    return "The port must be between 1 and 65535.";
  }
  const schema = form.schema.trim();
  if (schema && !/^[A-Za-z0-9_$]+$/.test(schema)) {
    return "The schema must be a plain identifier: letters, digits, `_` and `$`.";
  }
  return null;
}

/** How a connection reads in the list's target column: `user@host:port/db`,
 * schema included, password never — or, for SQLite, the file and its store. */
export function connectionTarget(row: DbConnectionRow): string {
  if (backendOf(row) === "sqlite") {
    return `${row.file_path ?? ""} in file store ${row.file_store ?? ""}`;
  }
  return `${row.username}@${row.host}:${row.port}/${row.database} (schema ${row.schema})`;
}

/** What a connection contributed, in words: the sentence under its name.
 *
 * Three states, and the third is the one worth having: connected with tables,
 * connected with none (a schema that is empty, or the wrong schema — which
 * looks identical to a working connection unless something says so), and not
 * connected at all. */
export function connectionSummary(row: DbConnectionRow): string {
  if (!row.connected) return row.error ?? "Not connected.";
  if (row.tables === 0) {
    return backendOf(row) === "sqlite"
      ? "Connected, but the file has no tables Saltcorn can use."
      : `Connected, but schema ${row.schema} has no tables Saltcorn can use.`;
  }
  return `${row.tables} ${row.tables === 1 ? "table" : "tables"} in the tables list.`;
}
