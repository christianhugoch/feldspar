// `db` as `vendor/saltcorn-data/models/library.ts` sees it (TODO "The builder"
// §8, 2.2) — and as no other file sees it: `build.mjs` resolves that file's
// `../db/index.js` here, keyed on the importer, and every other `db` import is
// still the host's.
//
// v1's `Library` reads and writes `_sc_library` through `db`. On this server
// the library is framework storage the admin server holds (`_fd_library`), and
// the worker renders from the application's snapshot of it, which reaches the
// call as `getState().library`. So the two reads `Library.find` and
// `Library.findOne` make are answered from that list, filtered by v1's own
// `satisfies`, and everything else on `db` refuses by name: `create`, `update`,
// `delete` and `saveLibraryUpdates` belong to the admin API, never the worker.
import { getState } from "@saltcorn/data/db/state";
import { satisfies } from "../../vendor/saltcorn-data/utils.js";

type Obj = Record<string, any>;

const LIBRARY_TABLE = "_sc_library"; // v1's table, the name models/library.ts selects from

/** The application's library, for a read of `table`, which must be v1's. */
function libraryRows(table: string, call: string): Obj[] {
  if (table !== LIBRARY_TABLE) {
    throw new Error(
      `\`db.${call}("${table}")\` is not available to models/library here: it reads only ${LIBRARY_TABLE}`,
    );
  }
  return (getState() as Obj).library ?? [];
}

/** v1's `db.select(table, where, selectopts)`, over the snapshot. */
async function select(table: string, where?: Obj, selectopts: Obj = {}): Promise<Obj[]> {
  let rows = libraryRows(table, "select").filter(satisfies(where ?? {}));
  const { orderBy, orderDesc, limit, offset } = selectopts;
  if (orderBy !== undefined) {
    if (typeof orderBy !== "string") {
      throw new Error("`db.select` in models/library orders only by one column name here");
    }
    const dir = orderDesc ? -1 : 1;
    rows = [...rows].sort((a, b) => (a[orderBy] < b[orderBy] ? -dir : a[orderBy] > b[orderBy] ? dir : 0));
  }
  const from = offset ?? 0;
  return rows.slice(from, limit ? from + limit : undefined);
}

/** v1's `db.selectMaybeOne`: the first match, or `null`. */
async function selectMaybeOne(table: string, where?: Obj): Promise<Obj | null> {
  return (await select(table, where))[0] ?? null;
}

const refused = (member: string) => () => {
  throw new Error(
    `\`db.${member}\` is refused in models/library here: the Saltcorn UI worker renders the ` +
      `library from the application's snapshot and never writes it; library items are saved ` +
      `through the admin API`,
  );
};

const answered: Obj = { select, selectMaybeOne };

export default new Proxy(answered, {
  get(target, prop) {
    if (typeof prop === "symbol" || prop === "then") return undefined;
    return prop in target ? target[prop] : refused(prop);
  },
});
