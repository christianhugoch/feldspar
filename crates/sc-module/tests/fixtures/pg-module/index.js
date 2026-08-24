// A **writable table provider over a remote PostgreSQL table**, modelled on
// `@saltcorn/postgres-tables` (design §8.3).
//
// Why a fixture rather than that plugin itself: `@saltcorn/postgres-tables`
// begins `require("@saltcorn/data/db")`, so it is a client of v1's own
// internals rather than a thin wrapper over an npm library, and v1's package
// does not load on a worker that has no v1 around it (it fails inside its own
// module graph with `isNode is not a function`). `pg` — the part that actually
// talks to Postgres — runs here perfectly well, which is what this fixture
// demonstrates.
//
// Everything that matters about that plugin is kept:
//
//   * a `configuration_workflow` asking for the connection and the table;
//   * `fields(cfg)` reporting the remote table's columns, so the Saltcorn table
//     has the columns the remote one has;
//   * `get_table(cfg)` returning `getRows` plus `insertRow`/`updateRow`/
//     `deleteRows` — and **omitting the writing three** when `read_only` is set,
//     which is v1's whole model of writability;
//   * the `where`/`options` pair honoured in SQL rather than ignored, so the
//     remote database does the filtering.

const { Pool } = require("pg");
const Workflow = require("@saltcorn/data/models/workflow");
const Form = require("@saltcorn/data/models/form");

/** One pool per connection string, in this module's scope.
 *
 * The same arrangement `@saltcorn/postgres-tables` has, and the reason a call
 * must reach the worker the module was loaded on: a second copy of this map on
 * another isolate would be a second set of connections. */
const pools = {};

function getConnection(cfg) {
  const key = JSON.stringify([cfg.host, cfg.port, cfg.user, cfg.database]);
  if (!pools[key])
    pools[key] = new Pool({
      host: cfg.host,
      port: cfg.port,
      user: cfg.user,
      password: cfg.password || undefined,
      database: cfg.database,
    });
  return pools[key];
}

/** An identifier, sanitised the way v1's `sqlsanitize` does: anything that is
 * not a word character is not part of a name. Values never go through here —
 * they are bind parameters. */
const ident = (s) => String(s || "").replace(/[^A-Za-z_0-9]/g, "");

const qualified = (cfg) => `"${ident(cfg.schema || "public")}"."${ident(cfg.table_name)}"`;

/** Postgres's column types, as Saltcorn's. Everything unrecognised is a String,
 * which is what v1's discovery does too. */
function scType(dataType) {
  switch (dataType) {
    case "integer":
    case "bigint":
    case "smallint":
      return "Integer";
    case "double precision":
    case "real":
    case "numeric":
      return "Float";
    case "boolean":
      return "Bool";
    case "date":
      return "Date";
    case "timestamp with time zone":
    case "timestamp without time zone":
      return "Date";
    case "json":
    case "jsonb":
      return "JSON";
    default:
      return "String";
  }
}

/** v1's `where` object as a SQL fragment and its bind values.
 *
 * The subset v1's own matcher speaks, which is the subset `sc_catalog`'s
 * pushdown produces: `{ col: value }`, `{ col: { in: [...] } }`,
 * `{ col: { gt|lt, equal } }`, `{ col: { ilike } }` and `{ or: [...] }`. An
 * empty object is no condition at all — which for a *read* means the whole
 * table, and is why a delete is never sent one. */
function mkWhere(where, values = []) {
  const parts = [];
  for (const [key, condition] of Object.entries(where || {})) {
    if (key === "or" && Array.isArray(condition)) {
      const branches = condition.map((c) => mkWhere(c, values).sql).filter(Boolean);
      if (branches.length) parts.push(`(${branches.join(" or ")})`);
      continue;
    }
    const col = `"${ident(key)}"`;
    if (condition === null) {
      parts.push(`${col} is null`);
    } else if (typeof condition === "object" && !Array.isArray(condition)) {
      if (Array.isArray(condition.in)) {
        if (!condition.in.length) {
          parts.push("false");
          continue;
        }
        const holes = condition.in.map((v) => `$${values.push(v)}`);
        parts.push(`${col} in (${holes.join(", ")})`);
      } else if (condition.gt !== undefined) {
        parts.push(`${col} >${condition.equal ? "=" : ""} $${values.push(condition.gt)}`);
      } else if (condition.lt !== undefined) {
        parts.push(`${col} <${condition.equal ? "=" : ""} $${values.push(condition.lt)}`);
      } else if (condition.ilike !== undefined) {
        parts.push(`${col} ilike $${values.push(`%${condition.ilike}%`)}`);
      }
    } else {
      parts.push(`${col} = $${values.push(condition)}`);
    }
  }
  return { sql: parts.join(" and "), values };
}

/** v1's `options`: `orderBy`, `orderDesc`, `limit`, `offset`. */
function mkOptions(opts) {
  let sql = "";
  if (opts && opts.orderBy)
    sql += ` order by "${ident(opts.orderBy)}"${opts.orderDesc ? " desc" : ""}`;
  if (opts && Number.isInteger(opts.limit)) sql += ` limit ${opts.limit}`;
  if (opts && Number.isInteger(opts.offset)) sql += ` offset ${opts.offset}`;
  return sql;
}

const configuration_workflow = () =>
  new Workflow({
    steps: [
      {
        name: "Connection",
        form: () =>
          new Form({
            fields: [
              { name: "host", label: "Database host", type: "String", required: true },
              { name: "port", label: "Port", type: "Integer", default: 5432 },
              { name: "user", label: "User", type: "String", required: true },
              { name: "password", label: "Password", type: "String", fieldview: "password" },
              { name: "database", label: "Database", type: "String", required: true },
              { name: "schema", label: "Schema", type: "String" },
              { name: "table_name", label: "Table name", type: "String", required: true },
              {
                name: "primary_key",
                label: "Primary key column",
                sublabel: "The column updateRow addresses a row by",
                type: "String",
                default: "id",
              },
              { name: "read_only", label: "Read-only", type: "Bool" },
            ],
          }),
      },
    ],
  });

module.exports = {
  sc_plugin_api_version: 1,
  table_providers: {
    "PostgreSQL table": {
      configuration_workflow,

      // The remote table's own columns, read out of `information_schema` —
      // which is the reason `fields` is a *function* of the configuration and
      // is asked again on every catalog reload.
      fields: async (cfg) => {
        const pool = getConnection(cfg);
        const res = await pool.query(
          `select c.column_name, c.data_type,
                  coalesce(k.is_key, false) as is_key
             from information_schema.columns c
             left join (
               select kcu.column_name, true as is_key
                 from information_schema.table_constraints tc
                 join information_schema.key_column_usage kcu
                   on kcu.constraint_name = tc.constraint_name
                  and kcu.table_schema = tc.table_schema
                where tc.constraint_type = 'PRIMARY KEY'
                  and tc.table_schema = $1 and tc.table_name = $2
             ) k on k.column_name = c.column_name
            where c.table_schema = $1 and c.table_name = $2
            order by c.ordinal_position`,
          [cfg.schema || "public", cfg.table_name],
        );
        return res.rows.map((r) => ({
          name: r.column_name,
          label: r.column_name,
          type: scType(r.data_type),
          primary_key: r.is_key,
        }));
      },

      get_table: (cfg) => ({
        getRows: async (where, opts) => {
          const pool = getConnection(cfg);
          const { sql, values } = mkWhere(where);
          const res = await pool.query(
            `select * from ${qualified(cfg)}${sql ? ` where ${sql}` : ""}${mkOptions(opts)}`,
            values,
          );
          return res.rows;
        },

        // The writing three, withheld entirely when the admin ticked
        // "Read-only" — v1 has no other way to say it, and the host reads
        // writability off exactly this.
        ...(cfg.read_only
          ? {}
          : {
              insertRow: async (rec) => {
                const pool = getConnection(cfg);
                const columns = Object.keys(rec);
                if (!columns.length) {
                  const res = await pool.query(
                    `insert into ${qualified(cfg)} default values returning *`,
                  );
                  return keyOf(cfg, res.rows[0]);
                }
                const values = columns.map((c) => rec[c]);
                const holes = columns.map((_, i) => `$${i + 1}`);
                const res = await pool.query(
                  `insert into ${qualified(cfg)} (${columns
                    .map((c) => `"${ident(c)}"`)
                    .join(", ")}) values (${holes.join(", ")}) returning *`,
                  values,
                );
                // v1's contract: the new row's key, or nothing.
                return keyOf(cfg, res.rows[0]);
              },

              updateRow: async (rec, id) => {
                const pool = getConnection(cfg);
                const columns = Object.keys(rec);
                if (!columns.length) return;
                const values = columns.map((c) => rec[c]);
                const sets = columns.map((c, i) => `"${ident(c)}" = $${i + 1}`);
                values.push(id);
                await pool.query(
                  `update ${qualified(cfg)} set ${sets.join(", ")} where "${ident(
                    cfg.primary_key || "id",
                  )}" = $${values.length}`,
                  values,
                );
              },

              deleteRows: async (where) => {
                const pool = getConnection(cfg);
                const { sql, values } = mkWhere(where);
                // A `where` that says nothing would delete the table. v1 has the
                // same hole and the same answer: do not.
                if (!sql) throw new Error("a delete with no condition was refused");
                await pool.query(`delete from ${qualified(cfg)} where ${sql}`, values);
              },
            }),
      }),
    },
  },
};

/** The key column's value in a returned row: whatever the configuration named,
 * or `id`. `undefined` when the row does not carry one, which v1 allows — the
 * caller then keeps the record it wrote. */
function keyOf(cfg, row) {
  if (!row) return undefined;
  return row[cfg.primary_key || "id"];
}
