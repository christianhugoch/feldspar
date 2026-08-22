/**
 * The "Connect a database" dialog's rules.
 *
 * Worth pinning for the reason the New table dialog's are: every one of these
 * mistakes is silent. A connection named `primary` collides with the id every
 * table of Saltcorn's own database carries; a schema that is not a plain
 * identifier is refused by the driver rather than by the form; an empty port box
 * would be sent as `0` and dial nothing. And a connection that dialled fine but
 * points at an empty schema looks exactly like one that works, unless the list
 * says so.
 */

import { describe, expect, it } from "vitest";

import {
  DEFAULT_PORT,
  DEFAULT_SCHEMA,
  EMPTY_DB_CONNECTION_FORM,
  SECRET_SENTINEL,
  connectionBody,
  connectionSummary,
  connectionTarget,
  dbConnectionError,
  formFromConnection,
  portOf,
  type DbConnectionForm,
  type DbConnectionRow,
} from "./dbConnection";

/** A form with only what a test states set. */
function form(over: Partial<DbConnectionForm>): DbConnectionForm {
  return { ...EMPTY_DB_CONNECTION_FORM, ...over };
}

/** A filled-in form, so a test can change one thing at a time. */
const complete = form({
  name: "reporting",
  host: "db.example.com",
  database: "analytics",
  username: "reader",
});

/** A connection as the server reports it. */
function row(over: Partial<DbConnectionRow> = {}): DbConnectionRow {
  return {
    id: "0195b1a0-0000-7000-8000-000000000000",
    name: "reporting",
    description: "",
    host: "db.example.com",
    port: 5432,
    database: "analytics",
    username: "reader",
    password: SECRET_SENTINEL,
    schema: "public",
    connected: true,
    error: null,
    tables: 3,
    shadowed: [],
    ...over,
  };
}

describe("dbConnectionError", () => {
  it("wants a name, a host, a database and a user", () => {
    expect(dbConnectionError(form({}))).toMatch(/name/);
    expect(dbConnectionError({ ...complete, host: " " })).toMatch(/host/);
    expect(dbConnectionError({ ...complete, database: "" })).toMatch(/database/);
    expect(dbConnectionError({ ...complete, username: "" })).toMatch(/user/);
    expect(dbConnectionError(complete)).toBe(null);
  });

  it("refuses `primary`, which names Saltcorn's own database", () => {
    expect(dbConnectionError({ ...complete, name: "primary" })).toMatch(/primary/);
  });

  it("does not want a password: a socket connection may need none", () => {
    expect(dbConnectionError({ ...complete, password: "" })).toBe(null);
  });

  it("checks the port is a number in range, but lets the box be empty", () => {
    expect(dbConnectionError({ ...complete, port: "" })).toBe(null);
    expect(dbConnectionError({ ...complete, port: "54ab" })).toMatch(/number/);
    expect(dbConnectionError({ ...complete, port: "0" })).toMatch(/between/);
    expect(dbConnectionError({ ...complete, port: "99999" })).toMatch(/between/);
    expect(dbConnectionError({ ...complete, port: "5433" })).toBe(null);
  });

  it("checks the schema is a plain identifier, because the driver will", () => {
    // It reaches the server as `-c search_path=<schema>`, which is interpolated
    // rather than bound — so the driver refuses anything it would have to quote,
    // and the form says so first.
    expect(dbConnectionError({ ...complete, schema: "my schema" })).toMatch(/identifier/);
    expect(dbConnectionError({ ...complete, schema: "reporting_2" })).toBe(null);
  });
});

describe("the body a form sends", () => {
  it("reads an empty port box as the Postgres default, not as zero", () => {
    expect(portOf({ ...complete, port: "" })).toBe(DEFAULT_PORT);
    expect(portOf({ ...complete, port: "  " })).toBe(DEFAULT_PORT);
    expect(portOf({ ...complete, port: "5433" })).toBe(5433);
    expect(connectionBody({ ...complete, port: "" }).port).toBe(DEFAULT_PORT);
  });

  it("reads an empty schema box as `public`", () => {
    expect(connectionBody({ ...complete, schema: "" }).schema).toBe(DEFAULT_SCHEMA);
  });

  it("trims what was typed, but never the password", () => {
    const body = connectionBody({
      ...complete,
      name: "  reporting ",
      host: " db.example.com ",
      password: " s3cret ",
    });
    expect(body.name).toBe("reporting");
    expect(body.host).toBe("db.example.com");
    // A password with a space at either end is a password with a space at
    // either end; trimming it would break a login for a "tidy-up" nobody asked
    // for.
    expect(body.password).toBe(" s3cret ");
  });

  it("sends the sentinel back unchanged when the admin did not retype it", () => {
    // This is the whole contract: the server sees its own sentinel and restores
    // the stored password, so editing the host does not require knowing the
    // password.
    const editing = formFromConnection(row());
    expect(editing.password).toBe(SECRET_SENTINEL);
    expect(connectionBody(editing).password).toBe(SECRET_SENTINEL);
  });

  it("carries an empty password through as empty", () => {
    // "No password" is a real connection — a Unix socket with peer auth — and
    // must not be turned into dots on the way to the form and back.
    const editing = formFromConnection(row({ password: "" }));
    expect(editing.password).toBe("");
    expect(connectionBody(editing).password).toBe("");
  });
});

describe("how a connection reads in the list", () => {
  it("shows where it points, and never the password", () => {
    expect(connectionTarget(row())).toBe(
      "reader@db.example.com:5432/analytics (schema public)",
    );
    expect(connectionTarget(row({ password: "hunter2" }))).not.toContain("hunter2");
  });

  it("says what it contributed", () => {
    expect(connectionSummary(row({ tables: 3 }))).toMatch(/3 tables/);
    expect(connectionSummary(row({ tables: 1 }))).toMatch(/1 table\b/);
  });

  it("distinguishes an empty schema from a working connection", () => {
    // Both dialled. Only one of them put anything in the tables list, and
    // without this the admin sees "Connected" and an unchanged tables list.
    expect(connectionSummary(row({ tables: 0 }))).toMatch(/no tables/);
  });

  it("says why, when it did not connect", () => {
    expect(
      connectionSummary(row({ connected: false, error: "could not translate host name" })),
    ).toMatch(/host name/);
    // A connection that failed with nothing recorded still says something.
    expect(connectionSummary(row({ connected: false, error: null }))).toMatch(/Not connected/);
  });
});
