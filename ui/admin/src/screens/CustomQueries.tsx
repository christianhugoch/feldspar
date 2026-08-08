// The **custom SQL query** editor: an application's own endpoints, written as
// SQL (§13.4).
//
// One list per API row that serves them, each query carrying a name, a method,
// a sub-path, a role floor, the SQL and its declared parameters. The button that
// matters is **Check**: it prepares the statement on the server and shows the
// columns Postgres says it returns — which is both the validation ("column
// `titel` does not exist", in Postgres's own words, while its author is still
// looking at the SQL) and the *documentation*, because those columns are what
// the generated client method will hand back.
//
// The model is `../customQueries`; this file is controls. In particular nothing
// here decides whether a query is valid — the server does, with the same call a
// save makes.
//
// **The authority note is on the screen, not only in the docs** (decision 6).
// Raw SQL does not go through the row layer, so ownership formulae, rich-type
// coercion and File-field rules do not reach it. An admin opening this hole
// should be told what it is a hole in, where they are opening it.

import { useState } from "react";
import Button from "react-bootstrap/Button";
import Card from "react-bootstrap/Card";
import Col from "react-bootstrap/Col";
import Form from "react-bootstrap/Form";
import Row from "react-bootstrap/Row";

import { api, errorMessage } from "../api";
import {
  PARAM_TYPES,
  QUERY_METHODS,
  blankParamRow,
  blankQueryRow,
  describeBody,
  statusSummary,
  type CheckStatus,
  type QueryRow,
} from "../customQueries";
import { roleOptions, useRoles } from "../roles";

/** The queries of one API row, edited in place.
 *
 * `tables` is the application's declared table subset, sent with a check so the
 * server can also refuse a name or a path that this app's own table routes
 * already answer — the whole refusal a save would give, from the check button. */
export function CustomQueries({
  queries,
  tables,
  idPrefix,
  onChange,
}: {
  queries: QueryRow[];
  tables: string[];
  idPrefix: string;
  onChange: (queries: QueryRow[]) => void;
}) {
  const roles = useRoles();
  // Keyed by index rather than held on the row: a check result is about this
  // editing session, not part of what gets saved.
  const [status, setStatus] = useState<Record<number, CheckStatus>>({});

  const setQuery = (index: number, next: QueryRow) =>
    onChange(queries.map((q, i) => (i === index ? next : q)));

  const check = async (index: number) => {
    setStatus((s) => ({ ...s, [index]: { kind: "checking" } }));
    try {
      const { columns } = await api.describeCustomQuery(describeBody(queries[index], tables));
      // The described columns land on the row as well as in the status: they
      // are what the stored query will carry, and showing them after a reopen
      // is the same fact as showing them now.
      setQuery(index, { ...queries[index], columns });
      setStatus((s) => ({ ...s, [index]: { kind: "ok", columns } }));
    } catch (err) {
      setStatus((s) => ({
        ...s,
        [index]: {
          kind: "error",
          message: errorMessage(err, "The server could not prepare this query."),
        },
      }));
    }
  };

  return (
    <Card className="mb-3">
      <Card.Header className="d-flex justify-content-between align-items-center">
        <span>Custom SQL queries</span>
        <Button
          size="sm"
          variant="outline-primary"
          onClick={() => onChange([...queries, blankQueryRow()])}
        >
          Add query
        </Button>
      </Card.Header>
      <Card.Body>
        <p className="text-muted small">
          Each query becomes one endpoint on this API and one typed method on the
          app's generated client, with the return type taken from the columns
          Postgres reports. <strong>A custom query's authority is its own:</strong>{" "}
          raw SQL does not go through the row layer, so ownership formulae,
          rich-type coercion and File-field rules do not apply to it, and a write
          inside one raises no table event. What still applies is the role floor
          below — <em>admin unless you say otherwise</em> — and the caller's
          database context, so a row-level-security policy still decides what the
          query can see.
        </p>
        {queries.length === 0 && <div className="text-muted">None.</div>}
        {queries.map((query, index) => (
          <div key={index} className={index > 0 ? "border-top pt-3 mt-3" : undefined}>
            <Row className="mb-2 align-items-end">
              <Col md={4}>
                <Form.Label className="small mb-1" htmlFor={`${idPrefix}-q${index}-name`}>
                  Name
                </Form.Label>
                <Form.Control
                  id={`${idPrefix}-q${index}-name`}
                  value={query.name}
                  placeholder="topAuthors"
                  onChange={(e) => setQuery(index, { ...query, name: e.target.value })}
                />
              </Col>
              <Col md={2}>
                <Form.Label className="small mb-1" htmlFor={`${idPrefix}-q${index}-method`}>
                  Method
                </Form.Label>
                <Form.Select
                  id={`${idPrefix}-q${index}-method`}
                  value={query.method}
                  onChange={(e) => setQuery(index, { ...query, method: e.target.value })}
                >
                  {QUERY_METHODS.map((m) => (
                    <option key={m} value={m}>
                      {m}
                    </option>
                  ))}
                </Form.Select>
              </Col>
              <Col md={3}>
                <Form.Label className="small mb-1" htmlFor={`${idPrefix}-q${index}-path`}>
                  Sub-path
                </Form.Label>
                <Form.Control
                  id={`${idPrefix}-q${index}-path`}
                  value={query.path}
                  placeholder="/reports/top-authors"
                  onChange={(e) => setQuery(index, { ...query, path: e.target.value })}
                />
              </Col>
              <Col md={3}>
                <Form.Label className="small mb-1" htmlFor={`${idPrefix}-q${index}-role`}>
                  Minimum role
                </Form.Label>
                <Form.Select
                  id={`${idPrefix}-q${index}-role`}
                  value={String(query.minRole)}
                  onChange={(e) =>
                    setQuery(index, { ...query, minRole: Number(e.target.value) })
                  }
                >
                  {roleOptions(query.minRole, roles).map((r) => (
                    <option key={r.role} value={r.role}>
                      {r.name} ({r.role})
                    </option>
                  ))}
                </Form.Select>
              </Col>
            </Row>

            <Form.Group className="mb-2">
              <Form.Label className="small mb-1" htmlFor={`${idPrefix}-q${index}-desc`}>
                Description
              </Form.Label>
              <Form.Control
                id={`${idPrefix}-q${index}-desc`}
                value={query.description}
                placeholder="What this query is for — it becomes the client method's doc comment."
                onChange={(e) => setQuery(index, { ...query, description: e.target.value })}
              />
            </Form.Group>

            <Form.Group className="mb-2">
              <Form.Label className="small mb-1" htmlFor={`${idPrefix}-q${index}-sql`}>
                SQL
              </Form.Label>
              <Form.Control
                as="textarea"
                rows={5}
                className="font-monospace"
                id={`${idPrefix}-q${index}-sql`}
                value={query.sql}
                placeholder="select author, count(*) as n from books where year > :since group by author"
                onChange={(e) => setQuery(index, { ...query, sql: e.target.value })}
              />
              <Form.Text muted>
                One statement. Write a parameter as <code>:name</code> and declare it
                below; arguments are always bound, never pasted into the text.
              </Form.Text>
            </Form.Group>

            <div className="mb-2">
              <div className="d-flex justify-content-between align-items-center mb-1">
                <span className="small fw-semibold">Parameters</span>
                <Button
                  size="sm"
                  variant="outline-secondary"
                  onClick={() =>
                    setQuery(index, { ...query, params: [...query.params, blankParamRow()] })
                  }
                >
                  Add parameter
                </Button>
              </div>
              {query.params.length === 0 && (
                <div className="text-muted small">None.</div>
              )}
              {query.params.map((param, pi) => (
                <Row key={pi} className="mb-2 align-items-end">
                  <Col md={5}>
                    <Form.Control
                      aria-label="Parameter name"
                      value={param.name}
                      placeholder="since"
                      onChange={(e) =>
                        setQuery(index, {
                          ...query,
                          params: query.params.map((p, i) =>
                            i === pi ? { ...p, name: e.target.value } : p,
                          ),
                        })
                      }
                    />
                  </Col>
                  <Col md={3}>
                    <Form.Select
                      aria-label="Parameter type"
                      value={param.type}
                      onChange={(e) =>
                        setQuery(index, {
                          ...query,
                          params: query.params.map((p, i) =>
                            i === pi ? { ...p, type: e.target.value } : p,
                          ),
                        })
                      }
                    >
                      {PARAM_TYPES.map((t) => (
                        <option key={t} value={t}>
                          {t}
                        </option>
                      ))}
                    </Form.Select>
                  </Col>
                  <Col md={2}>
                    <Form.Check
                      type="checkbox"
                      id={`${idPrefix}-q${index}-p${pi}-req`}
                      label="Required"
                      checked={param.required}
                      onChange={(e) =>
                        setQuery(index, {
                          ...query,
                          params: query.params.map((p, i) =>
                            i === pi ? { ...p, required: e.target.checked } : p,
                          ),
                        })
                      }
                    />
                  </Col>
                  <Col xs="auto">
                    <Button
                      size="sm"
                      variant="outline-danger"
                      onClick={() =>
                        setQuery(index, {
                          ...query,
                          params: query.params.filter((_, i) => i !== pi),
                        })
                      }
                    >
                      Remove
                    </Button>
                  </Col>
                </Row>
              ))}
              <Form.Text muted>
                An optional parameter binds SQL <code>NULL</code> when it is left out,
                which is what makes <code>(:q is null or name = :q)</code> an optional
                filter.
              </Form.Text>
            </div>

            <div className="d-flex gap-2 align-items-center">
              <Button
                size="sm"
                variant="outline-primary"
                disabled={status[index]?.kind === "checking"}
                onClick={() => void check(index)}
              >
                Check
              </Button>
              <Button
                size="sm"
                variant="outline-danger"
                onClick={() => {
                  onChange(queries.filter((_, i) => i !== index));
                  setStatus({});
                }}
              >
                Remove query
              </Button>
              <CheckResult status={status[index] ?? { kind: "idle" }} query={query} />
            </div>
          </div>
        ))}
      </Card.Body>
    </Card>
  );
}

/** What the check said — the refusal in Postgres's words, or the shape the
 * client method will return.
 *
 * With no check yet, the columns *stored* with the query are shown instead:
 * reopening an application should not make an admin re-derive what its query
 * returns, and those columns are the database's own answer from the last save. */
function CheckResult({ status, query }: { status: CheckStatus; query: QueryRow }) {
  if (status.kind === "error") {
    return <span className="text-danger small">{statusSummary(status)}</span>;
  }
  if (status.kind !== "idle") {
    return (
      <span className={status.kind === "ok" ? "text-success small" : "text-muted small"}>
        {statusSummary(status)}
      </span>
    );
  }
  if (query.columns.length === 0) {
    return (
      <span className="text-muted small">
        Not checked yet — Check prepares it and shows what it returns.
      </span>
    );
  }
  return (
    <span className="text-muted small">
      {statusSummary({ kind: "ok", columns: query.columns })}
    </span>
  );
}
