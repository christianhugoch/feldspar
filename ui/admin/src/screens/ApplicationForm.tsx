// Create / edit an application. The point this screen proves: **no screen knows
// a specific framework's settings**. The admin picks a framework and the form
// renders whatever settings that framework's `config_spec` declares (design
// §13.2/§13.3) — there is no `code`-framework-specific code here. `ui/form-runtime`
// is out of MVP scope, so the spec is rendered with a plain form.
//
// The rest of the form covers the other pieces of an application record: its
// subdomain, its table and file-store subsets, its enabled APIs, its static
// directories, and its CSP.

import { useEffect, useState, type FormEvent } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Card from "react-bootstrap/Card";
import Col from "react-bootstrap/Col";
import Form from "react-bootstrap/Form";
import Row from "react-bootstrap/Row";
import Spinner from "react-bootstrap/Spinner";

import { api, errorMessage } from "../api";
import type {
  CreateApplicationRequest,
  ListApiProvidersResponse,
  ListApplicationsResponse,
  ListFrameworksResponse,
  ListTriggersResponse,
} from "../client";
import { navigate } from "../App";
import { IconArrowLeft } from "../icons";
import { PageBody, PageHeader } from "../layout";
import { setNotice } from "../notice";
import { SettingsFields, asString, buildConfig, readConfig } from "../settings";
import {
  apiRowsFromApp,
  apiRowsToRequest,
  blankApiRow,
  specFor,
  type ApiRow,
} from "../apiRows";

type FrameworkInfo = ListFrameworksResponse[number];
type AppItem = ListApplicationsResponse[number];
type TriggerItem = ListTriggersResponse[number];
type ApiProviderInfo = ListApiProvidersResponse[number];
/** A `{ mount, store, path }` static-directory row. */
type StaticRow = { mount: string; store: string; path: string };

/** Split a comma/whitespace-separated list into trimmed, non-empty names. */
function parseNames(raw: string): string[] {
  return raw
    .split(/[,\s]+/)
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

/** Render a CSP object as `directive: src1 src2` lines for the textarea. */
function cspToText(csp: unknown): string {
  if (!csp || typeof csp !== "object") return "";
  return Object.entries(csp as Record<string, unknown>)
    .map(([name, sources]) => {
      const list = Array.isArray(sources) ? sources.map(asString) : [];
      return `${name}: ${list.join(" ")}`.trimEnd();
    })
    .join("\n");
}

/** Parse `directive: src1 src2` lines back into a CSP object. */
function textToCsp(text: string): Record<string, string[]> {
  const csp: Record<string, string[]> = {};
  for (const line of text.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    const colon = trimmed.indexOf(":");
    const name = (colon === -1 ? trimmed : trimmed.slice(0, colon)).trim();
    const rest = colon === -1 ? "" : trimmed.slice(colon + 1);
    if (name) csp[name] = rest.split(/\s+/).filter((s) => s.length > 0);
  }
  return csp;
}

export function ApplicationForm({ appId }: { appId?: string }) {
  const [frameworks, setFrameworks] = useState<FrameworkInfo[] | null>(null);
  // The server's triggers, so the exposed subset is *picked* rather than typed:
  // a name that does not resolve is an application that will not mount, and the
  // list is right here to choose from (unlike tables, which are not).
  const [allTriggers, setAllTriggers] = useState<TriggerItem[]>([]);
  // The API providers this server registers, for the same reason: a provider name
  // is the one field of an application whose typo survives the save and turns up
  // later as "unknown API provider" from a mount that failed.
  const [allProviders, setAllProviders] = useState<ApiProviderInfo[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [subdomain, setSubdomain] = useState("");
  const [frameworkName, setFrameworkName] = useState("");
  const [config, setConfig] = useState<Record<string, string>>({});
  const [tables, setTables] = useState("");
  const [fileStores, setFileStores] = useState("");
  const [triggers, setTriggers] = useState<string[]>([]);
  // A new application starts with REST at `/api`. An app with no API has no
  // endpoints, which for a React app means a generated client with no methods and
  // a project that cannot compile — and for any app means a UI that cannot reach
  // its data. It is a row like any other, so removing it stays one click.
  const [apis, setApis] = useState<ApiRow[]>([
    { ...blankApiRow(), provider: "rest", mount: "/api" },
  ]);
  const [staticDirs, setStaticDirs] = useState<StaticRow[]>([]);
  // Empty by default: a new app takes its framework's policy (§2.2) unless the
  // admin states one. Editing an app fills this in from what was stored.
  const [csp, setCsp] = useState("");

  // Load the frameworks (always) and, when editing, the app to prefill.
  useEffect(() => {
    let cancelled = false;
    const run = async () => {
      try {
        const fws = await api.listFrameworks();
        // A server with no triggers, or one that cannot list them, still edits
        // applications: the picker is simply empty, and an app that already
        // names a trigger keeps naming it.
        const trigs = await api.listTriggers().catch(() => [] as TriggerItem[]);
        // Likewise: a server that cannot list its providers still edits
        // applications, with the provider box falling back to free text.
        const provs = await api.listApiProviders().catch(() => [] as ApiProviderInfo[]);
        let existing: AppItem | undefined;
        if (appId) {
          existing = (await api.listApplications()).find((a) => a.id === appId);
          if (!existing) {
            if (!cancelled) setLoadError("That application no longer exists.");
            return;
          }
        }
        if (cancelled) return;
        setFrameworks(fws);
        setAllTriggers(trigs);
        setAllProviders(provs);
        if (existing) {
          setName(existing.name);
          setDescription(existing.description);
          setSubdomain(existing.subdomain);
          setFrameworkName(existing.framework.name);
          setConfig(readConfig(existing.framework.config));
          setTables(existing.tables.join(", "));
          setFileStores(existing.file_stores.join(", "));
          setTriggers(existing.triggers);
          setApis(apiRowsFromApp(existing.apis));
          setStaticDirs(
            existing.static_dirs.map((d) => ({
              mount: d.mount,
              store: d.store,
              path: d.path,
            })),
          );
          setCsp(cspToText(existing.csp));
        } else {
          setFrameworkName(fws[0]?.name ?? "");
        }
      } catch {
        if (!cancelled) setLoadError("Could not load the framework list.");
      }
    };
    void run();
    return () => {
      cancelled = true;
    };
  }, [appId]);

  const selected = frameworks?.find((f) => f.name === frameworkName);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const body: CreateApplicationRequest = {
        name: name.trim(),
        description: description.trim(),
        subdomain: subdomain.trim(),
        framework: {
          name: frameworkName,
          config: buildConfig(selected?.config_spec ?? [], config),
        },
        extra_frameworks: [],
        tables: parseNames(tables),
        file_stores: parseNames(fileStores),
        triggers,
        apis: apiRowsToRequest(apis, allProviders),
        static_dirs: staticDirs.filter((d) => d.mount.trim() || d.path.trim()),
        // An empty box means "no opinion", and is sent as no field at all so the
        // *framework's* default policy applies (§2.2) — a React app gets the one
        // fitted to what Vite emits. Sending `default-src 'self'` because a
        // textarea was pre-filled would silently overrule that.
        csp: csp.trim() ? textToCsp(csp) : undefined,
        attributes: {},
      };
      if (appId) {
        await api.updateApplication(appId, body);
      } else {
        const created = await api.createApplication(body);
        // Creating a React application also creates its project on the server
        // (§2.3), and creating any application creates the agent that builds it
        // (§13.3). Both are news the admin should see, and so is either one being
        // refused — on an application that was still created, because the row is
        // valid either way. The list screen owns the banner, so the message is
        // handed to it rather than shown here on a screen about to unmount.
        const done: string[] = [];
        const refused: string[] = [];
        if (created.scaffolded) {
          done.push(
            `${created.scaffolded}. Build it to install its dependencies and serve it.`,
          );
        }
        if (created.scaffold_error) refused.push(created.scaffold_error);
        if (created.agent) {
          done.push(`Its builder agent, ${created.agent}, is ready to chat with.`);
        }
        if (created.agent_error) refused.push(created.agent_error);
        if (done.length || refused.length) {
          setNotice({
            ok: refused.length === 0,
            title: refused.length
              ? `Application created, but not everything with it — ${created.name}`
              : `Application created — ${created.name}`,
            text: [...refused, ...done].join(" "),
          });
        }
      }
      navigate("/applications");
    } catch (err) {
      setError(errorMessage(err, "Could not save the application."));
    } finally {
      setBusy(false);
    }
  };

  if (loadError) {
    return (
      <PageBody>
        <Alert variant="danger">{loadError}</Alert>
      </PageBody>
    );
  }
  if (!frameworks) {
    return (
      <PageBody>
        <div className="text-center py-5">
          <Spinner animation="border" role="status" />
        </div>
      </PageBody>
    );
  }

  return (
    <>
      <PageHeader
        pretitle="Deploy"
        title={appId ? "Edit application" : "New application"}
        actions={
          <Button variant="outline-secondary" onClick={() => navigate("/applications")}>
            <IconArrowLeft className="icon-2" />
            Back
          </Button>
        }
      />
      <PageBody>
        {error && <Alert variant="danger">{error}</Alert>}

        <Form onSubmit={submit}>
          <Row>
            <Col md={6}>
              <Form.Group className="mb-3" controlId="appName">
                <Form.Label>Name</Form.Label>
                <Form.Control
                  value={name}
                  required
                  onChange={(e) => setName(e.target.value)}
                />
              </Form.Group>
            </Col>
            <Col md={6}>
              <Form.Group className="mb-3" controlId="appSubdomain">
                <Form.Label>Subdomain</Form.Label>
                <Form.Control
                  value={subdomain}
                  required
                  onChange={(e) => setSubdomain(e.target.value)}
                />
                <Form.Text muted>Served at {subdomain || "<subdomain>"}.your-domain.</Form.Text>
              </Form.Group>
            </Col>
          </Row>

          <Form.Group className="mb-3" controlId="appDescription">
            <Form.Label>Description</Form.Label>
            <Form.Control
              value={description}
              onChange={(e) => setDescription(e.target.value)}
            />
          </Form.Group>

          <Card className="mb-3">
            <Card.Header>Framework</Card.Header>
            <Card.Body>
              {/* One choice per framework, each with the name and sentence the
                  *server* supplied. Two frameworks are not two equal names in a
                  dropdown — one creates the project for you and the other hands you
                  the paths — and that difference has to reach the admin. It does so
                  as data: the label, the description and the order all come from
                  the registry (§2.2/§2.4), so this screen presents the distinction
                  without knowing which framework is which. The first offered is the
                  one an admin should take, and is what a new application starts on. */}
              <fieldset className="mb-3">
                <legend className="form-label">Framework</legend>
                {frameworks.map((f) => (
                  <Form.Check
                    key={f.name}
                    type="radio"
                    name="framework"
                    id={`framework-${f.name}`}
                    className="mb-2"
                    checked={f.name === frameworkName}
                    onChange={() => setFrameworkName(f.name)}
                    label={
                      <>
                        <span className="fw-semibold">{f.label || f.name}</span>
                        {f.description && (
                          <div className="text-muted small">{f.description}</div>
                        )}
                      </>
                    }
                  />
                ))}
              </fieldset>

              {/* The framework's own settings, rendered from its config_spec — no
                  framework-specific code lives here. Picking the first framework
                  shows its two settings and picking the other shows five, with no
                  branch in this file: the spec is the branch. */}
              <SettingsFields
                spec={selected?.config_spec ?? []}
                values={config}
                onChange={(name, v) => setConfig((c) => ({ ...c, [name]: v }))}
              />
            </Card.Body>
          </Card>

          <Row>
            <Col md={6}>
              <Form.Group className="mb-3" controlId="appTables">
                <Form.Label>Tables</Form.Label>
                <Form.Control
                  value={tables}
                  placeholder="posts, comments"
                  onChange={(e) => setTables(e.target.value)}
                />
                <Form.Text muted>The tables this app may access.</Form.Text>
              </Form.Group>
            </Col>
            <Col md={6}>
              <Form.Group className="mb-3" controlId="appFileStores">
                <Form.Label>File stores</Form.Label>
                <Form.Control
                  value={fileStores}
                  placeholder="apps, uploads"
                  onChange={(e) => setFileStores(e.target.value)}
                />
                <Form.Text muted>The file stores this app may access.</Form.Text>
              </Form.Group>
            </Col>
          </Row>

          <Card className="mb-3">
            <Card.Header>Triggers</Card.Header>
            <Card.Body>
              {allTriggers.length === 0 && (
                <div className="text-muted">
                  No triggers are configured on this server.
                </div>
              )}
              {allTriggers.map((t) => (
                <Form.Check
                  key={t.id}
                  type="checkbox"
                  id={`trigger-${t.id}`}
                  className="mb-2"
                  checked={triggers.includes(t.name)}
                  onChange={(e) =>
                    setTriggers((current) =>
                      e.target.checked
                        ? [...current, t.name]
                        : current.filter((n) => n !== t.name),
                    )
                  }
                  label={
                    <>
                      <span className="fw-semibold">{t.name}</span>
                      <div className="text-muted small">
                        {t.action} · on {t.when} ·{" "}
                        {/* Same vocabulary the trigger form uses: 1 is admin,
                            100 is public, and no role set means admins only. */}
                        {t.min_role == null
                          ? "admins only (no minimum role set)"
                          : `minimum role ${t.min_role}`}
                      </div>
                    </>
                  }
                />
              ))}
              {/* An app that names a trigger the server no longer has will not
                  mount, so a stale selection is shown rather than dropped on the
                  floor by a picker that only knows about triggers that exist. */}
              {triggers
                .filter((name) => !allTriggers.some((t) => t.name === name))
                .map((name) => (
                  <div key={name} className="text-danger small mb-2">
                    <span className="fw-semibold">{name}</span> — no trigger of that
                    name exists here, so this application will not mount until it is
                    removed or the trigger is recreated.
                    <Button
                      size="sm"
                      variant="outline-danger"
                      className="ms-2"
                      onClick={() =>
                        setTriggers((current) => current.filter((n) => n !== name))
                      }
                    >
                      Remove
                    </Button>
                  </div>
                ))}
              <Form.Text muted>
                Each ticked trigger is exposed as <code>POST {"{api mount}"}/actions/
                {"{name}"}</code> on this app, guarded by the trigger's own minimum
                role.
              </Form.Text>
            </Card.Body>
          </Card>

          <ApiRows rows={apis} providers={allProviders} onChange={setApis} />

          <RepeatableRows
            title="Static directories"
            rows={staticDirs}
            columns={[
              { key: "mount", label: "Mount", placeholder: "/docs" },
              { key: "store", label: "Store", placeholder: "apps" },
              { key: "path", label: "Path", placeholder: "handbook" },
            ]}
            onChange={setStaticDirs}
            blank={{ mount: "", store: "", path: "" }}
          />

          <Form.Group className="mb-3" controlId="appCsp">
            <Form.Label>Content-Security-Policy</Form.Label>
            <Form.Control
              as="textarea"
              rows={3}
              value={csp}
              onChange={(e) => setCsp(e.target.value)}
            />
            <Form.Text muted>
              One directive per line, e.g. `default-src: 'self'`. Leave empty to use
              the framework's own default policy.
            </Form.Text>
          </Form.Group>

          <Button type="submit" disabled={busy}>
            {busy ? "Saving…" : appId ? "Save changes" : "Create application"}
          </Button>
        </Form>
      </PageBody>
    </>
  );
}

/** The APIs list: one card per enabled provider — its name, its mount, and the
 * settings *that provider declares*, rendered by the same `SettingsFields` a
 * framework's are (§13.3).
 *
 * Its own control rather than a `RepeatableRows` row, because an API row is no
 * longer uniform strings: two providers on one application show different
 * settings, and the difference comes from the server. There is still no
 * provider-specific code here — enabling GraphQL shows its aggregation switch
 * and its four bounds because that is what its `config_spec` says. */
function ApiRows({
  rows,
  providers,
  onChange,
}: {
  rows: ApiRow[];
  providers: ApiProviderInfo[];
  onChange: (rows: ApiRow[]) => void;
}) {
  const setRow = (index: number, next: ApiRow) =>
    onChange(rows.map((r, i) => (i === index ? next : r)));
  return (
    <Card className="mb-3">
      <Card.Header className="d-flex justify-content-between align-items-center">
        <span>APIs</span>
        <Button
          size="sm"
          variant="outline-primary"
          onClick={() => onChange([...rows, blankApiRow()])}
        >
          Add
        </Button>
      </Card.Header>
      <Card.Body>
        {rows.length === 0 && <div className="text-muted">None.</div>}
        {rows.map((row, index) => {
          const spec = specFor(providers, row.provider);
          return (
            <div key={index} className={index > 0 ? "border-top pt-3 mt-3" : undefined}>
              <Row className="mb-2 align-items-end">
                <Col>
                  <Form.Label className="small mb-1">Provider</Form.Label>
                  {/* The registered names, from the server. An empty list — a
                      server that could not list them — leaves this the text box
                      it was: a picker that cannot be populated should not become
                      a field that cannot be filled in. */}
                  {providers.length > 0 ? (
                    <Form.Select
                      value={row.provider}
                      onChange={(e) => {
                        const provider = e.target.value;
                        setRow(index, {
                          ...row,
                          provider,
                          // Picking a provider fills an *empty* mount with that
                          // provider's usual sub-path, so the common case is one
                          // click. An admin who has typed a mount keeps it.
                          mount: row.mount.trim()
                            ? row.mount
                            : (providers.find((p) => p.name === provider)?.default_mount ??
                              ""),
                        });
                      }}
                    >
                      <option value="">Choose…</option>
                      {providers.map((p) => (
                        <option key={p.name} value={p.name}>
                          {p.label}
                        </option>
                      ))}
                      {/* A stored value this server does not register — a
                          provider from a plugin that is gone, or an older name.
                          Kept as an option so opening the form does not silently
                          change it. */}
                      {row.provider && !providers.some((p) => p.name === row.provider) && (
                        <option value={row.provider}>{row.provider} (not registered)</option>
                      )}
                    </Form.Select>
                  ) : (
                    <Form.Control
                      value={row.provider}
                      placeholder="rest"
                      onChange={(e) => setRow(index, { ...row, provider: e.target.value })}
                    />
                  )}
                </Col>
                <Col>
                  <Form.Label className="small mb-1">Mount</Form.Label>
                  <Form.Control
                    value={row.mount}
                    placeholder="/api"
                    onChange={(e) => setRow(index, { ...row, mount: e.target.value })}
                  />
                </Col>
                <Col xs="auto">
                  <Button
                    variant="outline-danger"
                    onClick={() => onChange(rows.filter((_, i) => i !== index))}
                  >
                    Remove
                  </Button>
                </Col>
              </Row>
              {spec.length > 0 && (
                <div className="ps-1">
                  <SettingsFields
                    spec={spec}
                    values={row.config}
                    idPrefix={`api-${index}`}
                    onChange={(name, v) =>
                      setRow(index, { ...row, config: { ...row.config, [name]: v } })
                    }
                  />
                </div>
              )}
            </div>
          );
        })}
        {providers.length > 0 && (
          <Form.Text muted>
            {providers.map((p) => (
              <div key={p.name}>
                <code>{p.name}</code> — {p.description}
              </div>
            ))}
            <div className="mt-1">
              Each provider is mounted on its own sub-path; two on the same one is
              refused, because a request resolves to only one of them.
            </div>
          </Form.Text>
        )}
      </Card.Body>
    </Card>
  );
}

/** A repeatable list of uniform string-field rows (static dirs), with
 * add/remove. Generic over the row shape. */
function RepeatableRows<T extends Record<string, string>>({
  title,
  rows,
  columns,
  blank,
  onChange,
}: {
  title: string;
  rows: T[];
  columns: { key: keyof T & string; label: string; placeholder?: string }[];
  blank: T;
  onChange: (rows: T[]) => void;
}) {
  const setCell = (index: number, key: keyof T & string, value: string) => {
    onChange(rows.map((r, i) => (i === index ? ({ ...r, [key]: value } as T) : r)));
  };
  return (
    <Card className="mb-3">
      <Card.Header className="d-flex justify-content-between align-items-center">
        <span>{title}</span>
        <Button
          size="sm"
          variant="outline-primary"
          onClick={() => onChange([...rows, { ...blank }])}
        >
          Add
        </Button>
      </Card.Header>
      <Card.Body>
        {rows.length === 0 && <div className="text-muted">None.</div>}
        {rows.map((row, index) => (
          <Row key={index} className="mb-2 align-items-end">
            {columns.map((col) => (
              <Col key={col.key}>
                <Form.Label className="small mb-1">{col.label}</Form.Label>
                <Form.Control
                  value={row[col.key]}
                  placeholder={col.placeholder}
                  onChange={(e) => setCell(index, col.key, e.target.value)}
                />
              </Col>
            ))}
            <Col xs="auto">
              <Button
                variant="outline-danger"
                onClick={() => onChange(rows.filter((_, i) => i !== index))}
              >
                Remove
              </Button>
            </Col>
          </Row>
        ))}
      </Card.Body>
    </Card>
  );
}
