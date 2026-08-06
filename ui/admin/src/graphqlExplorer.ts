// The GraphQL explorer's model: a document and variables in, a request out; a
// response in, two panes out; and an introspection result in, a browsable schema
// out.
//
// Deliberately **not** a React component, for the reason `agentChat.ts` is not
// one: the parts that have to be right — malformed variables refused before a
// request is made, a GraphQL error rendered as an error rather than swallowed, a
// partial result shown as *both* panes, a type reference spelled the way the SDL
// spells it — are testable without a browser or a server, which is what
// `graphqlExplorer.test.ts` does.
//
// **Why there is no GraphiQL here.** The admin SPA is served under
// `default-src 'self'`, so the CDN bundle every GraphQL server ships cannot
// load; and vendoring GraphiQL would put a large dependency into a *server*
// screen to serve applications that never asked for one. What GraphiQL actually
// gives an admin is an editor, a variables pane, a response pane and a schema
// browsed from introspection — which is this file and the screen over it.

/** The GraphQL request body, exactly as the endpoint takes it. */
export type ExplorerRequest = {
  query: string;
  variables?: unknown;
  operationName?: string;
};

/** Either a request to send, or the reason there is none to send. */
export type BuildResult =
  | { ok: true; request: ExplorerRequest }
  | { ok: false; error: string };

/**
 * Turn what is in the two editors into a request body.
 *
 * The variables pane is JSON typed by a person, so it is parsed *here* and a
 * syntax error is reported against the pane it came from — a round trip that
 * comes back "invalid JSON body" tells the admin nothing about which of the two
 * editors is wrong. Empty (or whitespace) means no variables rather than an
 * error: it is the common case, and `{}` should not be something to remember.
 *
 * The document itself is **not** parsed here. It is the server's schema that
 * decides whether a document is valid, and a second, weaker parser in the
 * browser could only disagree with it.
 */
export function buildRequest(
  document: string,
  variablesText: string,
  operationName?: string,
): BuildResult {
  if (!document.trim()) {
    return { ok: false, error: "There is no query to run." };
  }
  const request: ExplorerRequest = { query: document };
  const raw = variablesText.trim();
  if (raw) {
    let parsed: unknown;
    try {
      parsed = JSON.parse(raw);
    } catch (err) {
      const detail = err instanceof Error ? err.message : String(err);
      return { ok: false, error: `The variables are not valid JSON: ${detail}` };
    }
    // GraphQL variables are a map of name → value; an array or a number would be
    // refused by the server, and saying so here names the pane.
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
      return {
        ok: false,
        error: "The variables must be a JSON object, e.g. { \"floor\": 40000 }.",
      };
    }
    request.variables = parsed;
  }
  if (operationName && operationName.trim()) {
    request.operationName = operationName.trim();
  }
  return { ok: true, request };
}

/**
 * The operations a document declares, for the picker.
 *
 * A **heuristic**, and only ever used to offer names: a document with more than
 * one operation needs `operationName`, and an admin should not have to type a
 * name that is already on screen. The server parses the document for real, so
 * the worst a miss here can do is leave the picker empty (a document with one
 * anonymous operation, which needs no name anyway).
 */
export function operationNames(document: string): string[] {
  const names: string[] = [];
  const pattern = /(?:^|\n)\s*(query|mutation|subscription)\s+([_A-Za-z][_0-9A-Za-z]*)/g;
  let match = pattern.exec(document);
  while (match) {
    if (!names.includes(match[2])) names.push(match[2]);
    match = pattern.exec(document);
  }
  return names;
}

/** One error out of a GraphQL response, flattened for display. */
export type ExplorerError = {
  message: string;
  /** The response path it was raised at, e.g. `departments.0.employees`. */
  path: string | null;
  /** A machine-readable code, when the server labelled it (`extensions.code`). */
  code: string | null;
};

/** The two panes a response becomes. */
export type ResponseView = {
  /** The `data` half, pretty-printed — `null` when the response carried none. */
  data: string | null;
  /** The `errors` half, flattened. */
  errors: ExplorerError[];
  /** Data *and* errors: what a nullable child list's refusal looks like. */
  partial: boolean;
};

/**
 * Split a GraphQL response into the panes the screen renders.
 *
 * A GraphQL endpoint answers `200` with its errors in the body, so a response
 * that "succeeded" may be entirely a refusal — which is exactly the case an
 * explorer exists to show. Both halves are rendered when both are there: Phase
 * 4 made child lists nullable so that a child table the caller may not read is
 * an error on *that field* with the parents still returned, and an explorer that
 * showed only one of the two would hide the design.
 */
export function responseView(response: unknown): ResponseView {
  if (response === null || typeof response !== "object" || Array.isArray(response)) {
    // Not a GraphQL response shape at all. Show it rather than deciding it is
    // nothing: an admin looking at an unexpected body is debugging.
    return { data: pretty(response), errors: [], partial: false };
  }
  const body = response as Record<string, unknown>;
  const hasData = "data" in body && body.data !== null && body.data !== undefined;
  const errors = Array.isArray(body.errors) ? body.errors.map(explorerError) : [];
  return {
    data: hasData ? pretty(body.data) : null,
    errors,
    partial: hasData && errors.length > 0,
  };
}

/** One entry of a response's `errors` array, however sparse it is. */
function explorerError(raw: unknown): ExplorerError {
  if (raw === null || typeof raw !== "object") {
    return { message: String(raw), path: null, code: null };
  }
  const error = raw as Record<string, unknown>;
  const message =
    typeof error.message === "string" && error.message
      ? error.message
      : "The server reported an error with no message.";
  const path = Array.isArray(error.path) ? error.path.map(String).join(".") : null;
  const extensions =
    error.extensions && typeof error.extensions === "object"
      ? (error.extensions as Record<string, unknown>)
      : null;
  const code = extensions && typeof extensions.code === "string" ? extensions.code : null;
  return { message, path, code };
}

/** JSON as a person reads it. */
function pretty(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2) ?? String(value);
  } catch {
    return String(value);
  }
}

// --- the schema pane --------------------------------------------------------

/**
 * What the schema pane asks the application for.
 *
 * A cut-down `getIntrospectionQuery`: the roots, every type, its fields with
 * their argument names, and enough `ofType` unwrapping to spell `[Employees!]!`.
 * Deliberately not the full one — descriptions of deprecated fields, directives
 * and their locations are pages of response nobody browses — but it is ordinary
 * introspection, answered by the application's own schema, so the pane cannot
 * describe anything the API does not serve.
 */
export const INTROSPECTION_QUERY = `query IntrospectSchema {
  __schema {
    queryType { name }
    mutationType { name }
    types {
      kind
      name
      description
      fields {
        name
        description
        args { name type { ...Ref } }
        type { ...Ref }
      }
      inputFields { name type { ...Ref } }
      enumValues { name }
    }
  }
}

fragment Ref on __Type {
  kind
  name
  ofType {
    kind
    name
    ofType {
      kind
      name
      ofType { kind name }
    }
  }
}`;

/** A type reference as introspection returns it. */
export type TypeRef = {
  kind?: string | null;
  name?: string | null;
  ofType?: TypeRef | null;
};

/**
 * A type reference spelled the way the SDL spells it: `[Employees!]!`.
 *
 * Introspection returns wrappers outside in, so this unwraps them the same way
 * — and a reference deeper than the query unwrapped ends in `…`, which is
 * honest about being truncated rather than silently dropping a `!`.
 */
export function typeRefLabel(ref: TypeRef | null | undefined): string {
  if (!ref) return "…";
  if (ref.kind === "NON_NULL") return `${typeRefLabel(ref.ofType)}!`;
  if (ref.kind === "LIST") return `[${typeRefLabel(ref.ofType)}]`;
  return ref.name ?? "…";
}

/** The name at the bottom of a type reference: `[Employees!]!` → `Employees`. */
export function namedType(ref: TypeRef | null | undefined): string | null {
  if (!ref) return null;
  if (ref.name) return ref.name;
  return namedType(ref.ofType);
}

/** One field of a type, as the pane lists it. */
export type SchemaField = {
  name: string;
  /** The field's type, spelled as the SDL spells it. */
  type: string;
  /** The named type at the bottom of it — what a click navigates to. */
  target: string | null;
  args: { name: string; type: string }[];
  description: string;
};

/** One type of the schema, as the pane lists it. */
export type SchemaType = {
  name: string;
  kind: string;
  description: string;
  fields: SchemaField[];
};

/** The browsable schema. */
export type SchemaOverview = {
  queryType: string | null;
  mutationType: string | null;
  types: SchemaType[];
};

/**
 * Build the browsable schema from an introspection **response** (the whole
 * body, or just its `data` — the screen has the response and should not have to
 * unwrap it).
 *
 * Returns `null` when there is no schema in it, which is what a refusal or a
 * transport error leaves; the screen then shows the error it already has rather
 * than an empty pane pretending the application has no types.
 *
 * The `__`-prefixed introspection types are dropped: they describe the
 * introspection system, not this application, and an admin browsing `__Type`
 * has been sent the wrong way.
 */
export function parseSchema(response: unknown): SchemaOverview | null {
  const body = asObject(response);
  if (!body) return null;
  const data = asObject(body.data) ?? body;
  const schema = asObject(data.__schema);
  if (!schema) return null;
  const rawTypes = Array.isArray(schema.types) ? schema.types : [];
  const types: SchemaType[] = [];
  for (const raw of rawTypes) {
    const type = asObject(raw);
    const name = type && typeof type.name === "string" ? type.name : null;
    if (!type || !name || name.startsWith("__")) continue;
    types.push({
      name,
      kind: typeof type.kind === "string" ? type.kind : "OBJECT",
      description: typeof type.description === "string" ? type.description : "",
      // An input object's members arrive as `inputFields` rather than `fields`,
      // and to someone browsing `DepartmentsBoolExp` they are its fields.
      fields: [
        ...schemaFields(type.fields),
        ...schemaFields(type.inputFields),
        // An enum's values are what there is to know about it.
        ...enumValues(type.enumValues),
      ],
    });
  }
  types.sort((a, b) => a.name.localeCompare(b.name));
  return {
    queryType: rootName(schema.queryType),
    mutationType: rootName(schema.mutationType),
    types,
  };
}

/** The name of a schema root (`{ name: "Query" }`), or `null` for a schema with
 * no mutations — which is what a read-only application has. */
function rootName(raw: unknown): string | null {
  const root = asObject(raw);
  return root && typeof root.name === "string" ? root.name : null;
}

function schemaFields(raw: unknown): SchemaField[] {
  if (!Array.isArray(raw)) return [];
  const fields: SchemaField[] = [];
  for (const entry of raw) {
    const field = asObject(entry);
    if (!field || typeof field.name !== "string") continue;
    const type = (field.type ?? null) as TypeRef | null;
    fields.push({
      name: field.name,
      type: typeRefLabel(type),
      target: namedType(type),
      args: Array.isArray(field.args)
        ? field.args.flatMap((entry) => {
            const arg = asObject(entry);
            if (!arg || typeof arg.name !== "string") return [];
            return [{ name: arg.name, type: typeRefLabel((arg.type ?? null) as TypeRef | null) }];
          })
        : [],
      description: typeof field.description === "string" ? field.description : "",
    });
  }
  return fields;
}

function enumValues(raw: unknown): SchemaField[] {
  if (!Array.isArray(raw)) return [];
  return raw.flatMap((entry) => {
    const value = asObject(entry);
    if (!value || typeof value.name !== "string") return [];
    return [{ name: value.name, type: "", target: null, args: [], description: "" }];
  });
}

function asObject(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

/**
 * A query to start from, for a root field the admin picked out of the schema
 * pane: the field, with the leaf columns of the type it returns.
 *
 * Leaves only — a key field projects another table's object type and a child
 * list is a list of one, and neither can be selected without a nested selection
 * set, so a starter document that named them would not run. This is the
 * "clicking a table writes the query" affordance, and it is the one place the
 * pane produces text rather than describing it.
 */
export function starterDocument(overview: SchemaOverview | null, rootField: string): string {
  const empty = `query {\n  ${rootField}\n}`;
  if (!overview) return empty;
  const query = overview.types.find((t) => t.name === overview.queryType);
  const field = query?.fields.find((f) => f.name === rootField);
  const target = field?.target;
  const type = overview.types.find((t) => t.name === target);
  if (!type) return empty;
  const scalars = new Set(
    overview.types.filter((t) => t.kind === "SCALAR" || t.kind === "ENUM").map((t) => t.name),
  );
  const leaves = type.fields.filter((f) => f.target !== null && scalars.has(f.target));
  if (leaves.length === 0) return empty;
  const selection = leaves.map((f) => `    ${f.name}`).join("\n");
  return `query {\n  ${rootField} {\n${selection}\n  }\n}`;
}

// --- what the screen says about itself --------------------------------------

/** The GraphQL mount an application enables, or `null` if it enables none. */
export function graphqlMount(apis: { provider: string; mount: string }[]): string | null {
  return apis.find((api) => api.provider === "graphql")?.mount ?? null;
}

/**
 * Where the explored API actually is, for the caption: the app's own subdomain
 * under the admin's host, plus the provider's mount.
 */
export function graphqlEndpointUrl(
  subdomain: string,
  mount: string,
  location: { protocol: string; host: string },
): string {
  return `${location.protocol}//${subdomain}.${location.host}${mount}`;
}

/**
 * What the screen says about whose authority it is running under.
 *
 * Required, not decorative. The explorer executes as the signed-in admin, and an
 * admin clears every table's role floor — so a query that works here is not
 * evidence that it works for the application's users, and an explorer that let
 * someone believe otherwise would be a trap. The sentence is in the model so
 * that a test can hold it to naming the person.
 */
export function runsAsCaption(email: string): string {
  return (
    `Queries run as you — ${email}, an admin — against this application's own ` +
    `GraphQL mount. An admin clears every table's role floor and every ownership ` +
    `rule, so this shows the most any caller can see, not what a particular user ` +
    `of the application sees.`
  );
}
