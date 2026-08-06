/**
 * The explorer's model (TODO "GraphQL API" Phase 9): document + variables → a
 * request, a response → the panes, and an introspection result → a browsable
 * schema.
 *
 * No browser and no server: everything below is a pure function over values the
 * endpoint really produces, so the cases that matter — malformed variables
 * refused before a round trip, a refusal rendered as a refusal, a *partial*
 * result rendered as both — are asserted rather than clicked through.
 */

import { describe, expect, it } from "vitest";

import {
  INTROSPECTION_QUERY,
  buildRequest,
  graphqlEndpointUrl,
  graphqlMount,
  namedType,
  operationNames,
  parseSchema,
  responseView,
  runsAsCaption,
  starterDocument,
  typeRefLabel,
} from "./graphqlExplorer";

describe("building the request", () => {
  it("sends the document alone when there are no variables", () => {
    const built = buildRequest("query { departments { name } }", "   ");
    expect(built.ok).toBe(true);
    if (!built.ok) return;
    expect(built.request).toEqual({ query: "query { departments { name } }" });
    // Not `variables: {}` — an absent key and an empty object are different
    // requests, and the empty one is what an admin has to remember to type.
    expect("variables" in built.request).toBe(false);
  });

  it("parses the variables pane and carries the operation name", () => {
    const built = buildRequest(
      "query staff($floor: BigInt) { employees(where: { salary: { gte: $floor } }) { name } }",
      '{ "floor": 40000 }',
      "staff",
    );
    expect(built.ok).toBe(true);
    if (!built.ok) return;
    expect(built.request.variables).toEqual({ floor: 40000 });
    expect(built.request.operationName).toBe("staff");
  });

  it("refuses malformed variables here, naming the pane, rather than round-tripping", () => {
    const built = buildRequest("query { departments { name } }", "{ floor: }");
    expect(built.ok).toBe(false);
    if (built.ok) return;
    expect(built.error).toMatch(/variables/i);
    expect(built.error).toMatch(/JSON/);
  });

  it("refuses variables that are not an object", () => {
    const built = buildRequest("query { departments { name } }", "[1, 2, 3]");
    expect(built.ok).toBe(false);
    if (built.ok) return;
    expect(built.error).toMatch(/object/i);
  });

  it("refuses an empty document", () => {
    expect(buildRequest("  \n ", "").ok).toBe(false);
  });

  it("offers the operations a document declares", () => {
    const document = `query staff { departments { name } }
      mutation addOne { insert_departments(object: { id: 4, name: "Legal" }) { id } }`;
    expect(operationNames(document)).toEqual(["staff", "addOne"]);
    // One anonymous operation needs no name, so there is nothing to offer.
    expect(operationNames("query { departments { name } }")).toEqual([]);
  });
});

describe("the response panes", () => {
  it("renders data as formatted JSON", () => {
    const view = responseView({ data: { departments: [{ name: "Engineering" }] } });
    expect(view.errors).toEqual([]);
    expect(view.partial).toBe(false);
    expect(view.data).toContain('"name": "Engineering"');
  });

  it("renders a refusal as an error, not as an empty result", () => {
    // What the app answers an unauthorized caller: 200, `data: null`, and the
    // refusal naming the table in `errors`.
    const view = responseView({
      data: null,
      errors: [
        {
          message: "not authorized to read table `departments`",
          path: ["departments"],
          extensions: { code: "forbidden" },
        },
      ],
    });
    expect(view.data).toBeNull();
    expect(view.errors).toHaveLength(1);
    expect(view.errors[0].message).toContain("departments");
    expect(view.errors[0].path).toBe("departments");
    expect(view.errors[0].code).toBe("forbidden");
  });

  it("renders a partial result as both panes", () => {
    // Phase 4's nullable child list: the parents came back, the child table's
    // refusal is an error on that field. Showing one half would hide the design.
    const view = responseView({
      data: { departments: [{ name: "Engineering", employees: null }] },
      errors: [{ message: "not authorized to read table `employees`", path: ["departments", 0, "employees"] }],
    });
    expect(view.partial).toBe(true);
    expect(view.data).toContain("Engineering");
    expect(view.errors[0].path).toBe("departments.0.employees");
    expect(view.errors[0].code).toBeNull();
  });

  it("shows an unexpected body rather than deciding it is nothing", () => {
    const view = responseView("gateway timeout");
    expect(view.data).toContain("gateway timeout");
    expect(view.errors).toEqual([]);
  });

  it("survives an error entry with no message", () => {
    const view = responseView({ errors: [{}] });
    expect(view.errors).toHaveLength(1);
    expect(view.errors[0].message).toMatch(/no message/);
  });
});

describe("the schema pane", () => {
  /** An introspection answer shaped exactly as the endpoint returns one. */
  const introspection = {
    data: {
      __schema: {
        queryType: { name: "Query" },
        mutationType: { name: "Mutation" },
        types: [
          {
            kind: "OBJECT",
            name: "Query",
            description: "",
            fields: [
              {
                name: "departments",
                description: "Rows of departments",
                args: [
                  { name: "where", type: { kind: "INPUT_OBJECT", name: "DepartmentsBoolExp" } },
                  { name: "limit", type: { kind: "SCALAR", name: "Int" } },
                ],
                type: {
                  kind: "NON_NULL",
                  name: null,
                  ofType: {
                    kind: "LIST",
                    name: null,
                    ofType: {
                      kind: "NON_NULL",
                      name: null,
                      ofType: { kind: "OBJECT", name: "Departments" },
                    },
                  },
                },
              },
            ],
          },
          {
            kind: "OBJECT",
            name: "Departments",
            description: "The departments table",
            fields: [
              { name: "id", args: [], type: { kind: "NON_NULL", ofType: { kind: "SCALAR", name: "BigInt" } } },
              { name: "name", args: [], type: { kind: "SCALAR", name: "String" } },
              {
                name: "employees",
                args: [],
                type: { kind: "LIST", ofType: { kind: "OBJECT", name: "Employees" } },
              },
            ],
          },
          { kind: "OBJECT", name: "Employees", fields: [{ name: "name", args: [], type: { kind: "SCALAR", name: "String" } }] },
          {
            kind: "INPUT_OBJECT",
            name: "DepartmentsBoolExp",
            inputFields: [{ name: "name", type: { kind: "INPUT_OBJECT", name: "StringComparison" } }],
          },
          { kind: "SCALAR", name: "String" },
          { kind: "SCALAR", name: "BigInt" },
          { kind: "SCALAR", name: "Int" },
          { kind: "ENUM", name: "OrderDirection", enumValues: [{ name: "asc" }, { name: "desc" }] },
          // The introspection system's own types, which describe introspection
          // rather than this application.
          { kind: "OBJECT", name: "__Type", fields: [] },
        ],
      },
    },
  };

  it("asks for a schema, with the roots and enough unwrapping to spell a list", () => {
    expect(INTROSPECTION_QUERY).toContain("__schema");
    expect(INTROSPECTION_QUERY).toContain("queryType");
    expect(INTROSPECTION_QUERY).toContain("ofType");
  });

  it("spells a type reference the way the SDL does", () => {
    expect(
      typeRefLabel({
        kind: "NON_NULL",
        ofType: { kind: "LIST", ofType: { kind: "NON_NULL", ofType: { kind: "OBJECT", name: "Employees" } } },
      }),
    ).toBe("[Employees!]!");
    expect(typeRefLabel({ kind: "SCALAR", name: "String" })).toBe("String");
    expect(namedType({ kind: "LIST", ofType: { kind: "OBJECT", name: "Employees" } })).toBe("Employees");
    // Truncation is admitted rather than papered over.
    expect(typeRefLabel(null)).toBe("…");
  });

  it("builds the browsable schema, dropping the introspection types", () => {
    const overview = parseSchema(introspection);
    expect(overview).not.toBeNull();
    if (!overview) return;
    expect(overview.queryType).toBe("Query");
    expect(overview.mutationType).toBe("Mutation");
    expect(overview.types.map((t) => t.name)).not.toContain("__Type");
    // Sorted, so a schema of eighty tables is navigable.
    const names = overview.types.map((t) => t.name);
    expect([...names].sort((a, b) => a.localeCompare(b))).toEqual(names);

    const query = overview.types.find((t) => t.name === "Query");
    expect(query?.fields[0].type).toBe("[Departments!]!");
    expect(query?.fields[0].target).toBe("Departments");
    expect(query?.fields[0].args.map((a) => `${a.name}: ${a.type}`)).toEqual([
      "where: DepartmentsBoolExp",
      "limit: Int",
    ]);

    // An input object's members are its fields to whoever is browsing it.
    const boolExp = overview.types.find((t) => t.name === "DepartmentsBoolExp");
    expect(boolExp?.fields.map((f) => f.name)).toEqual(["name"]);
    // ...and an enum's values are what there is to know about it.
    const direction = overview.types.find((t) => t.name === "OrderDirection");
    expect(direction?.fields.map((f) => f.name)).toEqual(["asc", "desc"]);
  });

  it("has nothing to browse when the response carried no schema", () => {
    expect(parseSchema({ errors: [{ message: "refused" }] })).toBeNull();
    expect(parseSchema(null)).toBeNull();
  });

  it("writes a starter query naming only the leaves", () => {
    const overview = parseSchema(introspection);
    const document = starterDocument(overview, "departments");
    expect(document).toContain("departments {");
    expect(document).toContain("id");
    expect(document).toContain("name");
    // `employees` is a list of an object type: naming it with no selection set
    // would be a document that does not parse.
    expect(document).not.toContain("employees");
    // Without a schema there is still something to start from.
    expect(starterDocument(null, "departments")).toContain("departments");
  });
});

describe("what the screen says about itself", () => {
  it("finds the application's GraphQL mount, and says when there is none", () => {
    expect(graphqlMount([{ provider: "rest", mount: "/api" }, { provider: "graphql", mount: "/gql" }])).toBe("/gql");
    expect(graphqlMount([{ provider: "rest", mount: "/api" }])).toBeNull();
  });

  it("names the endpoint being explored, on the app's own subdomain", () => {
    expect(
      graphqlEndpointUrl("staff", "/graphql", { protocol: "https:", host: "example.com" }),
    ).toBe("https://staff.example.com/graphql");
  });

  it("names the person the queries run as, and that it is the most anyone can see", () => {
    // The trap this screen must not be: an explorer quietly holding more
    // authority than the caller being debugged.
    const caption = runsAsCaption("admin@example.com");
    expect(caption).toContain("admin@example.com");
    expect(caption.toLowerCase()).toContain("admin");
    expect(caption).toMatch(/most any caller can see/);
  });
});
