// The **Saltcorn 1 `Table` and `Field`**, in JavaScript, over the plan seam.
//
// One source, two hosts: this file is compiled into the code isolates beside
// the `db` prelude and concatenated into `sc-module`'s host script, so the
// `Table` a `run_js_code` body gets and the `Table` an installed v1 plugin gets
// are the same text. Two implementations that agreed today would be two
// implementations of v1's `Where` translation, and they would disagree by the
// third bug fixed in one of them.
//
// It is JavaScript for the reason the `db` prelude is: what crosses into Rust
// is a **plan**, so v1's whole `Where` vocabulary, its `selopts` and (later) its
// `joinFields` are lowered *here*, against a seam that resolves every name it is
// handed and trusts none of them. No SQL is assembled anywhere in this file.
//
// # The division
//
// v1's `Table.findOne` is synchronous and eight years of plugins are written
// that way, so **metadata is local and synchronous, data is a host call and
// asynchronous** — which is v1's own division, and is why the port is possible
// at all. The local half is the *schema snapshot*: every table and field this
// catalog has, built by `sc_api::code_host::schema` and handed to the isolate
// before the run starts. Nothing in this file makes a host call to answer a
// property.
//
// # What is here and what throws
//
// Everything this version does not implement is reachable as a property and
// **fatal on call**, naming the path — `NOT_IMPLEMENTED` below is the one place
// that list lives, so a method implemented later is deleted from it in the same
// edit that implements it, and the two can never disagree. A method that
// answered `undefined` would not fail; it would compute the wrong answer inside
// somebody's trigger.
(() => {
  "use strict";

  const fixed = (name, value) =>
    Object.defineProperty(globalThis, name, {
      value: value, writable: false, configurable: false, enumerable: false,
    });

  // -------------------------------------------------------------------------
  // The refusal tier — one list, for the whole of both classes
  // -------------------------------------------------------------------------

  // The reasons, written once each so that the sixteen schema-editing methods
  // say the same sentence rather than sixteen paraphrases of it.
  const SCHEMA =
    "this server introspects the schema it is given, so a table is created and " +
    "altered in the database itself rather than through the API";
  const HISTORY = "this server keeps no row history to read, restore or compress";
  const SYNC =
    "the mobile offline sync of Saltcorn 1 has no counterpart on this server";
  const STORED =
    "this server recomputes calculated fields itself, on read; there is nothing " +
    "to drive by hand";
  const PORTS =
    "import and export are this server's own, through the API and the admin UI";
  const BUILDER =
    "these are Saltcorn 1's view builder talking to itself, and this server " +
    "builds its views another way";
  const LATER = null;

  // Every v1 `Table` and `Field` member this version does not implement, and
  // why. `Table.`/`Field.` is a static, `table.`/`field.` an instance method —
  // the two spellings v1 itself uses.
  //
  // A `null` reason is a method that is simply not built yet: it says it is not
  // available and does not pretend to have a principle behind it.
  const NOT_IMPLEMENTED = {
    // Reads and writes. Not built yet; the phases after this one are them, and
    // each is deleted from this list by the edit that implements it.
    "table.getRows": LATER,
    "table.getRow": LATER,
    "table.countRows": LATER,
    "table.distinctValues": LATER,
    "table.aggregationQuery": LATER,
    "table.getJoinedRows": LATER,
    "table.getJoinedRow": LATER,
    "table.getJoinedQuery": LATER,
    "table.insertRow": LATER,
    "table.tryInsertRow": LATER,
    "table.updateRow": LATER,
    "table.tryUpdateRow": LATER,
    "table.deleteRows": LATER,
    "table.toggleBool": LATER,
    "table.run_trigger": LATER,
    "field.distinct_values": LATER,

    // Schema editing (§9): a v1 plugin that edits the schema is a plugin
    // editing a schema this server introspects, which is a different argument.
    "Table.create": SCHEMA,
    "Table.update": SCHEMA,
    "Table.rename": SCHEMA,
    "Table.delete": SCHEMA,
    "table.update": SCHEMA,
    "table.rename": SCHEMA,
    "table.delete": SCHEMA,
    "table.add_unique_constraint": SCHEMA,
    "table.remove_unique_constraint": SCHEMA,
    "table.enable_fkey_constraint": SCHEMA,
    "table.resetSequence": SCHEMA,
    "table.repairCompositePrimary": SCHEMA,
    "Field.create": SCHEMA,
    "field.update": SCHEMA,
    "field.delete": SCHEMA,
    "field.alter_sql_type": SCHEMA,
    "field.toggle_not_null": SCHEMA,
    "field.enable_fkey_constraint": SCHEMA,
    "field.add_unique_constraint": SCHEMA,
    "field.remove_unique_constraint": SCHEMA,

    // Row history.
    "table.get_history": HISTORY,
    "table.insert_history_row": HISTORY,
    "table.restore_row_version": HISTORY,
    "table.undo_row_changes": HISTORY,
    "table.redo_row_changes": HISTORY,
    "table.compress_history": HISTORY,

    // Offline sync.
    "table.latestSyncInfo": SYNC,
    "table.latestSyncInfos": SYNC,

    // Stored calculated fields: v1's recalculation entry points.
    "table.update_stored_calculateds": STORED,
    "table.recalculate_for_stored": STORED,

    // Import and export.
    "Table.create_from_csv": PORTS,
    "table.import_csv_file": PORTS,
    "table.import_json_file": PORTS,
    "table.dump_to_json": PORTS,

    // The view builder's own helpers.
    "table.get_join_field_options": BUILDER,
    "table.get_relation_options": BUILDER,
    "table.get_relation_data": BUILDER,
    "table.get_parent_relations": BUILDER,
    "table.get_child_relations": BUILDER,
    "table.field_options": BUILDER,
    "table.slug_options": BUILDER,
    "table.delete_url": BUILDER,
    "table.getTags": BUILDER,
    "table.getFormulaExamples": BUILDER,
    "field.fill_fkey_options": BUILDER,
    "field.generate": BUILDER,
    "field.validate": BUILDER,
  };

  // What a refused member says. The same sentence in a code body and in a
  // module, because it is the same method that is missing.
  const notAvailable = (path) =>
    "the Saltcorn v1 API " + path + " is not available in this version of Saltcorn" +
    (NOT_IMPLEMENTED[path] ? ": " + NOT_IMPLEMENTED[path] : "");

  // A member that is reachable and fatal on call, naming the path used. Its
  // `name` is the path so that a stack, a log line and a `String(fn)` all say
  // which method the plugin reached for.
  const refusal = (path) => {
    const f = function () {
      throw new Error(notAvailable(path));
    };
    Object.defineProperty(f, "name", { value: path });
    return f;
  };

  // Install every refusal whose path starts with `prefix` onto `target`.
  //
  // A name that is **already there** is a hard error at build time rather than
  // a silent overwrite: that is what stops the list and the implementation from
  // disagreeing, and it fires the moment anybody implements a method without
  // deleting its line above.
  const installRefusals = (target, prefix) => {
    for (const path of Object.keys(NOT_IMPLEMENTED)) {
      if (path.indexOf(prefix) !== 0) continue;
      const name = path.slice(prefix.length);
      if (name.indexOf(".") >= 0) continue;
      if (name in target) {
        throw new Error(
          path + " is both implemented and on the v1 refusal list; delete its line"
        );
      }
      Object.defineProperty(target, name, {
        value: refusal(path), writable: false, configurable: false, enumerable: false,
      });
    }
    return target;
  };

  // Reachable so a test can walk every refusal and assert each names itself,
  // and so an admin screen can list what a v1 plugin will not find here. Inert:
  // it is a list of strings.
  fixed("__scV1Refused", () => Object.keys(NOT_IMPLEMENTED).slice());

  // -------------------------------------------------------------------------
  // v1's `Where`, translated (§5)
  // -------------------------------------------------------------------------

  const isObject = (v) =>
    v !== null && typeof v === "object" && !Array.isArray(v);

  const kindOf = (v) => Object.prototype.toString.call(v);

  // The v1 keys and operators this server deliberately does not carry, and what
  // to write instead. Each is a SQL construct the plan seam does not express or
  // a feature this server does not have — and a translator that dropped one on
  // the floor would compute the wrong answer inside somebody's trigger.
  const REFUSED_WHERE = {
    inSelect:
      "a subquery in a where is not part of this server's plan seam; read the " +
      "inner query first and pass its values with { in: [...] }",
    inSelectWithLevels:
      "recursive subqueries are not part of this server's plan seam",
    json:
      "a JSON path condition is not part of this server's plan seam; select the " +
      "value with a formula and compare that",
    slugify: "slug matching is not part of this server's where vocabulary",
    _fts:
      "full-text search is not part of this server's where vocabulary; " +
      "{ ilike: \"...\" } is the nearest thing",
    day_only:
      "date truncation is not part of this server's where vocabulary; compare " +
      "against the two ends of the day instead",
    eq:
      "the two-expression form of eq compares two SQL expressions, which this " +
      "server's plan seam does not carry; write the comparison as a formula string",
    sql: "raw SQL in a where is not part of this server's plan seam",
  };

  // The value side of a comparison. Two v1 spellings are refused here rather
  // than passed on, because both reach SQL as text in v1 and as nothing at all
  // here.
  const literal = (where, value) => {
    if (typeof value === "symbol") {
      throw new Error(
        "`" + where + "`: a Symbol is Saltcorn 1's raw-SQL escape, which this " +
        "server does not run; write the condition as a formula string instead"
      );
    }
    if (kindOf(value) === "[object RegExp]") {
      throw new Error(
        "`" + where + "`: a regular expression is Saltcorn 1's ~ operator, which " +
        "this server does not carry; { ilike: \"...\" } is the nearest thing"
      );
    }
    if (typeof value === "function") {
      throw new Error("`" + where + "`: a function is not a value to compare against");
    }
    return value;
  };

  const refuseKey = (key) => {
    if (Object.prototype.hasOwnProperty.call(REFUSED_WHERE, key)) {
      throw new Error(
        "`" + key + "` in a where is not supported: " + REFUSED_WHERE[key]
      );
    }
  };

  // v1's implicit `%…%`: `{ ilike: "tol" }` matches anywhere in the column,
  // which is what every v1 search box means by it. `fullMatch: true` is the
  // caller saying the pattern is already whole.
  const likePattern = (field, value, fullMatch) => {
    if (typeof value !== "string") {
      throw new Error(
        "`" + field + "`: ilike takes the text to look for, e.g. { ilike: \"tol\" }"
      );
    }
    return fullMatch ? value : "%" + value + "%";
  };

  // The conditions one field's value asks for, as a list — a list because v1
  // says two things about one column in one object (`{ gt: 100, lt: 500 }`) and
  // this server's filter object takes one comparison per key.
  const fieldConditions = (field, value) => {
    if (value === null) return [{ is_null: true }];
    if (Array.isArray(value)) {
      // v1: an array of conditions on one field is their conjunction.
      return value.reduce((acc, part) => acc.concat(fieldConditions(field, part)), []);
    }
    // The three v1 values that are not values here reach `literal` **before**
    // the operator branch: a RegExp has no own keys, so an object walk would
    // read v1's `~` as an empty condition and quietly match every row.
    if (typeof value === "symbol" || typeof value === "function" ||
        kindOf(value) === "[object RegExp]") {
      literal(field, value);
    }
    if (!isObject(value) || kindOf(value) === "[object Date]") {
      return [{ eq: literal(field, value) }];
    }
    const keys = Object.keys(value);
    // v1's two modifiers are read off the object rather than translated: they
    // change what a sibling key means and are not conditions of their own.
    const equal = value.equal === true;
    const fullMatch = value.fullMatch === true;
    const out = [];
    for (const key of keys) {
      if (key === "equal" || key === "fullMatch") continue;
      refuseKey(key);
      const operand = value[key];
      switch (key) {
        case "gt":
          out.push(equal ? { gte: literal(field, operand) } : { gt: literal(field, operand) });
          break;
        case "lt":
          out.push(equal ? { lte: literal(field, operand) } : { lt: literal(field, operand) });
          break;
        case "gte":
        case "lte":
        case "ne": {
          const one = {};
          one[key] = literal(field, operand);
          out.push(one);
          break;
        }
        case "in":
          if (!Array.isArray(operand)) {
            throw new Error("`" + field + "`: in takes an array of values");
          }
          out.push({ in: operand.map((v) => literal(field, v)) });
          break;
        case "ilike":
          out.push({ ilike: likePattern(field, operand, fullMatch) });
          break;
        case "like":
          out.push({ like: likePattern(field, operand, fullMatch) });
          break;
        case "not":
          out.push({ not: operand });
          break;
        case "or":
          if (!Array.isArray(operand)) {
            throw new Error("`" + field + "`: or takes an array of conditions");
          }
          out.push({ or: operand });
          break;
        case "and":
          if (!Array.isArray(operand)) {
            throw new Error("`" + field + "`: and takes an array of conditions");
          }
          out.push({ and: operand });
          break;
        default:
          throw new Error(
            "`" + key + "` is not a Saltcorn 1 where operator on `" + field +
            "`; the operators are: gt, lt, gte, lte, ne, in, not, ilike, like, " +
            "or, and, with equal and fullMatch as modifiers"
          );
      }
    }
    // `{}` says nothing about the column in v1 and says nothing here: an empty
    // object that matched every row and an empty object that matched none are
    // both worse than no condition at all.
    return out;
  };

  // One field's conditions, as clauses of this server's filter object. The
  // three that are not comparisons — `not`, `or`, `and` — are combinators
  // *about that field*, so they nest the field name back inside themselves.
  const fieldClauses = (field, value) =>
    fieldConditions(field, value).map((cond) => {
      if (Object.prototype.hasOwnProperty.call(cond, "not")) {
        const inner = cond.not;
        // `{ id: { not: { in: [...] } } }` is v1's spelling of NOT IN, and
        // `nin` is what this server's vocabulary calls it — one operator rather
        // than a negated one, which is the statement anybody reading the SQL
        // expects.
        if (isObject(inner) && Object.keys(inner).length === 1 && Array.isArray(inner.in)) {
          return keyed(field, { nin: inner.in.map((v) => literal(field, v)) });
        }
        return { not: onField(field, inner) };
      }
      if (Object.prototype.hasOwnProperty.call(cond, "or")) {
        return { or: cond.or.map((part) => onField(field, part)) };
      }
      if (Object.prototype.hasOwnProperty.call(cond, "and")) {
        return { and: cond.and.map((part) => onField(field, part)) };
      }
      return keyed(field, cond);
    });

  const keyed = (field, cond) => {
    const out = {};
    out[field] = cond;
    return out;
  };

  // Everything one field's value says, as a single clause.
  const onField = (field, value) => conjoin(fieldClauses(field, value));

  // A list of clauses as one: nothing constrains nothing, one is itself — so
  // the common where is flat — and more than one is their conjunction.
  const conjoin = (clauses) => {
    if (clauses.length === 0) return {};
    if (clauses.length === 1) return clauses[0];
    return { and: clauses };
  };

  // v1's where-expression, as this server's filter object.
  //
  // Written as a list of clauses rather than one object with the fields' names
  // as keys because v1 says several things about one column in one place and
  // this server's filter takes one comparison per key: `{ pages: { gt: 100,
  // lt: 500 } }` is two clauses on `pages`, and an object cannot hold both.
  const translateWhere = (where) => {
    if (where === undefined || where === null) return null;
    if (!isObject(where)) {
      throw new Error(
        "a where is an object of conditions, e.g. { author: \"Tolstoy\" }"
      );
    }
    const clauses = [];
    for (const key of Object.keys(where)) {
      const value = where[key];
      // v1 drops an undefined value rather than filtering on it: an option that
      // was not passed is not a condition that nothing satisfies.
      if (value === undefined) continue;
      refuseKey(key);
      if (key === "or" || key === "and") {
        if (!Array.isArray(value)) {
          throw new Error("`" + key + "` takes an array of where-expressions");
        }
        const parts = value.map(translateWhere).filter((p) => p !== null);
        const combined = {};
        combined[key] = parts;
        clauses.push(combined);
        continue;
      }
      if (key === "not") {
        const inner = translateWhere(value);
        if (inner !== null) clauses.push({ not: inner });
        continue;
      }
      // v1's "match nothing", which a view writes when its state rules a query
      // out. There is no `false` in the filter vocabulary, so it is said in the
      // other spelling this seam already carries.
      if (key === "_false") {
        if (value) clauses.push({ formula: "false" });
        continue;
      }
      clauses.push.apply(clauses, fieldClauses(key, value));
    }
    if (clauses.length === 0) return null;
    return conjoin(clauses);
  };

  // -------------------------------------------------------------------------
  // v1's `selopts`, lowered (§5)
  // -------------------------------------------------------------------------

  const SELOPTS = [
    "fields", "orderBy", "orderDesc", "limit", "offset", "forUser", "forPublic",
  ];

  const bound = (name, value) => {
    if (typeof value !== "number" || !isFinite(value) || value < 0 || value % 1 !== 0) {
      throw new Error("`" + name + "` is a whole number of rows, e.g. { " + name + ": 10 }");
    }
    return value;
  };

  // One `orderBy` entry: v1's field name, or the `{ field, desc }` object.
  const orderKey = (entry, orderDesc) => {
    if (typeof entry === "string") {
      return { field: entry, dir: orderDesc ? "desc" : "asc" };
    }
    if (isObject(entry) && typeof entry.field === "string") {
      const desc = entry.desc === undefined ? orderDesc : !!entry.desc;
      return { field: entry.field, dir: desc ? "desc" : "asc" };
    }
    if (isObject(entry) && typeof entry.operator === "string") {
      throw new Error(
        "`orderBy: { operator: \"" + entry.operator + "\" }` is not supported: " +
        "this server orders by a field, a join path or a formula"
      );
    }
    throw new Error(
      "`orderBy` is a field name or { field, desc }, e.g. { orderBy: \"title\" }"
    );
  };

  // Whose view of the data this is. v1 says it with a *user*, and this server's
  // plan says it with an authority — `{ user: id }`, the named user through the
  // very same ownership functions `authority: "user"` goes through. It can only
  // narrow: the body already runs as admin and could read everything by leaving
  // the option out.
  const whoFor = (opts) => {
    if (opts.forUser !== undefined && opts.forUser !== null &&
        opts.forPublic !== undefined && opts.forPublic !== false) {
      throw new Error(
        "`forUser` and `forPublic` both say whose view of the data this is; pass one"
      );
    }
    if (opts.forPublic) return "public";
    const user = opts.forUser;
    if (user === undefined || user === null) return null;
    if (typeof user === "number" || typeof user === "string") return { user: user };
    if (isObject(user)) {
      if (user.id === undefined || user.id === null) {
        throw new Error(
          "`forUser` is the user to read as, and this one has no id; " +
          "pass the user row or its id"
        );
      }
      return { user: user.id };
    }
    throw new Error("`forUser` is a user row or a user id");
  };

  // v1's `selopts`, as the fields of a plan. An **unknown key is refused** for
  // the reason a misspelled fetch option is: an option that silently does
  // nothing is the worst way to learn it was spelled wrong.
  const translateSelopts = (opts) => {
    const out = {};
    if (opts === undefined || opts === null) return out;
    if (!isObject(opts)) {
      throw new Error(
        "the second argument is an options object, e.g. { orderBy: \"title\", limit: 10 }"
      );
    }
    for (const key of Object.keys(opts)) {
      if (SELOPTS.indexOf(key) < 0) {
        throw new Error(
          "`" + key + "` is not an option this server implements; the options " +
          "are: " + SELOPTS.join(", ")
        );
      }
    }
    if (opts.fields !== undefined && opts.fields !== null) {
      const fields = Array.isArray(opts.fields) ? opts.fields : [opts.fields];
      for (const f of fields) {
        if (typeof f !== "string") {
          throw new Error("`fields` is a list of field names");
        }
      }
      out.select = fields.slice();
    }
    if (opts.orderBy !== undefined && opts.orderBy !== null) {
      const desc = !!opts.orderDesc;
      const entries = Array.isArray(opts.orderBy) ? opts.orderBy : [opts.orderBy];
      out.order = entries.map((e) => orderKey(e, desc));
    } else if (opts.orderDesc) {
      throw new Error("`orderDesc` says which way to sort, and there is no `orderBy` to sort by");
    }
    if (opts.limit !== undefined && opts.limit !== null) out.limit = bound("limit", opts.limit);
    if (opts.offset !== undefined && opts.offset !== null) out.offset = bound("offset", opts.offset);
    const authority = whoFor(opts);
    if (authority !== null) out.authority = authority;
    return out;
  };

  // Both are pure — they hold no authority and reach nothing — so they are
  // reachable for the same reason `__scSchema` is: it is hygiene, not a
  // boundary, and a test that asserts what v1's vocabulary becomes is worth
  // more than the privacy of a function that only rearranges an object.
  fixed("__scV1Where", translateWhere);
  fixed("__scV1Selopts", translateSelopts);

  // -------------------------------------------------------------------------
  // `Field`: a view of the snapshot, not a record (§7)
  // -------------------------------------------------------------------------

  // v1 code assigns to a field and expects the assignment to matter — that is
  // what `Field.update` is for. Here it would change a copy of a snapshot and
  // nothing else, so it is **refused at the property**: a Proxy rather than
  // `Object.freeze`, because a frozen object swallows the assignment silently
  // in a non-strict body, which is the failure this is preventing.
  const readOnly = (obj, what) => {
    const refuse = (prop) => {
      throw new TypeError(
        what + " is this server's schema as it is, not a record to edit: " +
        "assigning to `" + String(prop) + "` would change a copy and nothing else"
      );
    };
    return new Proxy(obj, {
      set: (_t, prop) => refuse(prop),
      defineProperty: (_t, prop) => refuse(prop),
      deleteProperty: (_t, prop) => refuse(prop),
    });
  };

  const capitalise = (s) => (s.length === 0 ? s : s[0].toUpperCase() + s.slice(1));

  // v1's `Field.labelToName` and `Field.nameToLabel`, which plugins use to
  // build a column from a form label and a label from a column.
  const labelToName = (label) =>
    String(label).toLowerCase().replace(/ /g, "_").replace(/[^a-z0-9_]/g, "");
  const nameToLabel = (name) => capitalise(String(name).replace(/_/g, " "));

  // One field of one table, with v1's property names on it.
  const makeField = (api, table, spec) => {
    const field = {
      // Its id **is** its name: this server identifies a field by name (§9),
      // and a plugin keying a map by `f.id` gets a stable key either way.
      id: spec.name,
      name: spec.name,
      label: spec.label,
      // v1's two type properties, both carried because v1 code reads both: an
      // object for a plain field, the string `"Key to authors"` for a key.
      type: spec.type,
      typename: spec.typename,
      required: spec.required,
      is_unique: spec.is_unique,
      primary_key: spec.primary_key,
      calculated: spec.calculated,
      stored: spec.stored,
      expression: spec.expression,
      is_fkey: spec.is_fkey,
      reftable_name: spec.reftable_name,
      reftype: spec.reftype,
      refname: spec.refname,
      attributes: spec.attributes || {},
      fieldview: spec.fieldview,
      sublabel: spec.sublabel,
      // v1's `table_id` is a number and this server's tables are named (§9).
      table_id: spec.table_id,
      sql_name: spec.sql_name,
      sql_type: spec.sql_type,
      type_name: typeof spec.type === "string" ? spec.type : spec.type && spec.type.name,
      // v1's `pretty_type`: what a key is called, or the type's own name.
      pretty_type: spec.reftable_name ? "Key to " + spec.reftable_name : spec.typename,
      // v1's `form_name` differs from `name` only inside a subform, which this
      // server has no counterpart for; the name is the answer either way.
      form_name: spec.name,
    };
    // The field's own table, resolved rather than nested: a copy of every table
    // inside every field of it is a snapshot several times its own size.
    Object.defineProperty(field, "table", {
      get: () => api.Table.findOne(spec.table_id), enumerable: false,
    });
    installRefusals(field, "field.");
    return readOnly(field, "a field of `" + table + "`");
  };

  // -------------------------------------------------------------------------
  // `Table`: metadata, synchronously (§2)
  // -------------------------------------------------------------------------

  const makeTable = (api, spec) => {
    const fields = spec.fields.map((f) => makeField(api, spec.name, f));
    const byName = new Map(fields.map((f) => [f.name, f]));
    const pk = spec.primary_key || [];
    const table = {
      // A table's id is its name too, and for the same reason a field's is.
      id: spec.name,
      name: spec.name,
      label: spec.label,
      description: spec.description,
      min_role_read: spec.min_role_read,
      min_role_write: spec.min_role_write,
      // The ownership *formula's source*, because what a v1 plugin does with it
      // is show it or log it, and v1's field where — and only where — the
      // formula says exactly what such a field says.
      ownership_formula: spec.ownership_formula,
      ownership_field_id: spec.ownership_field_id,
      fields: Object.freeze(fields),
      // v1's `pk_name` is the first key; the whole of a composite one is beside
      // it, because this server allows a composite primary key and v1 did not.
      pk_name: pk.length ? pk[0] : undefined,
      composite_pk_names: Object.freeze(pk.slice()),
      // What the table is called in SQL. No tenant schema qualifies it here,
      // and nothing on this server will run SQL a plugin builds from it.
      sql_name: '"' + spec.name + '"',
      // Awaited in v1 (`await table.getFields()`), and awaiting an array is an
      // array — so the v1 spelling and this one are the same line.
      getFields: () => Object.freeze(fields),
      getForeignKeys: () => fields.filter((f) => f.is_fkey),
      owner_fieldname: () => spec.ownership_field_id || undefined,
      to_json: () => ({
        id: spec.name,
        name: spec.name,
        label: spec.label,
        description: spec.description,
        min_role_read: spec.min_role_read,
        min_role_write: spec.min_role_write,
        ownership_formula: spec.ownership_formula,
        ownership_field_id: spec.ownership_field_id,
        primary_key: pk.slice(),
        fields: spec.fields.map((f) => Object.assign({}, f)),
      }),
    };
    // v1's `pk_type` is what a caller branches on, so it is the type's *name*
    // — `"Integer"`, `"String"` — and not the type object `type` already
    // carries.
    Object.defineProperty(table, "pk_type", {
      enumerable: true,
      get: () => {
        const field = byName.get(table.pk_name);
        return field ? field.type_name : undefined;
      },
    });
    // v1's `getField` walks a dotted path — `"author.name"` is the *author's*
    // name field, on the table the key points at — which is the one place v1's
    // metadata is a graph rather than a list.
    Object.defineProperty(table, "getField", {
      enumerable: false,
      value: (path) => {
        if (typeof path !== "string") {
          throw new Error("getField takes a field name, e.g. getField(\"author\")");
        }
        const parts = path.split(".");
        let field = byName.get(parts[0]);
        for (let i = 1; i < parts.length; i++) {
          if (!field || !field.reftable_name) return undefined;
          const target = api.Table.findOne(field.reftable_name);
          if (!target) return undefined;
          field = target.getField(parts[i]);
        }
        return field;
      },
    });
    installRefusals(table, "table.");
    return readOnly(table, "table `" + spec.name + "`");
  };

  // -------------------------------------------------------------------------
  // The factory
  // -------------------------------------------------------------------------

  // One run's `Table` and `Field`, over one run's token and the schema snapshot
  // that run resolved at invoke.
  //
  // The token is what a read will carry when there are reads (the phase after
  // this one): with many runs resident on one isolate it is what tells the host
  // whose call this is, and a body is handed the classes rather than the token.
  //
  // A run with **no snapshot** gets classes that say so by name rather than
  // classes that know no tables: a `Table.findOne` answering undefined for
  // everything would have a plugin compute the wrong answer instead of failing.
  fixed("__scMakeV1Api", (__scTok, snapshot) => {
    const api = {};
    const absent = (what) => {
      throw new Error(
        what + " is not available here: this run was given no schema snapshot, " +
        "so there is nothing for it to answer from"
      );
    };
    if (!snapshot || !Array.isArray(snapshot.tables)) {
      api.Table = installRefusals({
        findOne: () => absent("Table.findOne"),
        find: () => absent("Table.find"),
      }, "Table.");
      api.Field = installRefusals({
        find: () => absent("Field.find"),
        findOne: () => absent("Field.findOne"),
        findCached: () => absent("Field.findCached"),
        labelToName: labelToName,
        nameToLabel: nameToLabel,
      }, "Field.");
      return api;
    }

    // `_sc_*` is where this server keeps its own rows, and a `Table.find()`
    // that listed them would put them in front of a plugin that only ever
    // wanted the application's tables.
    const specs = snapshot.tables.filter((t) => !t.is_system);
    // Built on demand and **kept**, so that `Table.findOne("books")` twice is
    // the same object twice: v1's is a state cache and plugins compare what
    // comes out of it.
    const built = new Map();
    const tableOf = (spec) => {
      if (!built.has(spec.name)) built.set(spec.name, makeTable(api, spec));
      return built.get(spec.name);
    };

    // A v1 `where` over the tables' own properties: `Table.findOne("books")`,
    // `Table.findOne({ name: "books" })` and `Table.find({ min_role_read: 1 })`
    // are all this, and none of them is a query.
    const matches = (spec, where) => {
      if (typeof where === "string" || typeof where === "number") {
        return spec.name === String(where);
      }
      if (!isObject(where)) return true;
      for (const key of Object.keys(where)) {
        const wanted = where[key];
        const held = key === "id" ? spec.name : spec[key];
        if (held !== wanted) return false;
      }
      return true;
    };

    api.Table = installRefusals({
      // Synchronous, which is the whole reason the snapshot exists. A table
      // this catalog has not got is `undefined`, as it is in v1 — an absence
      // this server really knows about, and not a stub declining to answer.
      findOne: (where) => {
        if (where === undefined || where === null) return undefined;
        const spec = specs.find((s) => matches(s, where));
        return spec ? tableOf(spec) : undefined;
      },
      find: (where, selopts) => {
        let out = specs.filter((s) => matches(s, where));
        if (selopts !== undefined && selopts !== null) {
          if (!isObject(selopts)) {
            throw new Error("Table.find's second argument is an options object");
          }
          for (const key of Object.keys(selopts)) {
            if (key !== "orderBy" && key !== "limit") {
              throw new Error(
                "`" + key + "` is not an option of Table.find; the options are: " +
                "orderBy, limit"
              );
            }
          }
          if (selopts.orderBy) {
            const by = selopts.orderBy;
            out = out.slice().sort((a, b) =>
              String(a[by] === undefined ? "" : a[by]) <
              String(b[by] === undefined ? "" : b[by]) ? -1 : 1
            );
          }
          if (selopts.limit !== undefined) out = out.slice(0, bound("limit", selopts.limit));
        }
        return out.map(tableOf);
      },
    }, "Table.");

    // Every field of every table, which is what v1's `Field.find` reads out of
    // `_sc_fields` — a table this server does not have, and a snapshot it does.
    const allFields = () => {
      const out = [];
      for (const spec of specs) {
        const table = tableOf(spec);
        for (const field of table.fields) out.push(field);
      }
      return out;
    };
    const fieldMatches = (field, where) => {
      if (typeof where === "string") return field.name === where;
      if (!isObject(where)) return true;
      for (const key of Object.keys(where)) {
        if (field[key] !== where[key]) return false;
      }
      return true;
    };
    const findFields = (where) => allFields().filter((f) => fieldMatches(f, where));

    api.Field = installRefusals({
      find: findFields,
      findOne: (where) => findFields(where)[0],
      // v1's cached spelling of the same question. Everything here is cached —
      // the snapshot is the cache — so the two answer alike rather than one of
      // them going to a database this server would not let it reach anyway.
      findCached: findFields,
      labelToName: labelToName,
      nameToLabel: nameToLabel,
    }, "Field.");

    return api;
  });
})();
