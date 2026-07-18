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
  ListApplicationsResponse,
  ListFrameworksResponse,
} from "../client";
import { navigate } from "../App";
import { SettingsFields, asString, buildConfig, readConfig } from "../settings";

type FrameworkInfo = ListFrameworksResponse[number];
type AppItem = ListApplicationsResponse[number];

/** A `{ provider, mount }` API row, edited as a repeatable list. */
type ApiRow = { provider: string; mount: string };
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
  const [apis, setApis] = useState<ApiRow[]>([]);
  const [staticDirs, setStaticDirs] = useState<StaticRow[]>([]);
  const [csp, setCsp] = useState("default-src: 'self'");

  // Load the frameworks (always) and, when editing, the app to prefill.
  useEffect(() => {
    let cancelled = false;
    const run = async () => {
      try {
        const fws = await api.listFrameworks();
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
        if (existing) {
          setName(existing.name);
          setDescription(existing.description);
          setSubdomain(existing.subdomain);
          setFrameworkName(existing.framework.name);
          setConfig(readConfig(existing.framework.config));
          setTables(existing.tables.join(", "));
          setFileStores(existing.file_stores.join(", "));
          setApis(existing.apis.map((a) => ({ provider: a.provider, mount: a.mount })));
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
        apis: apis.filter((a) => a.provider.trim() || a.mount.trim()),
        static_dirs: staticDirs.filter((d) => d.mount.trim() || d.path.trim()),
        csp: textToCsp(csp),
        attributes: {},
      };
      if (appId) {
        await api.updateApplication(appId, body);
      } else {
        await api.createApplication(body);
      }
      navigate("/applications");
    } catch (err) {
      setError(errorMessage(err, "Could not save the application."));
    } finally {
      setBusy(false);
    }
  };

  if (loadError) {
    return <Alert variant="danger">{loadError}</Alert>;
  }
  if (!frameworks) {
    return (
      <div className="text-center py-5">
        <Spinner animation="border" role="status" />
      </div>
    );
  }

  return (
    <>
      <div className="d-flex justify-content-between align-items-center mb-4">
        <h1 className="h3 mb-0">{appId ? "Edit application" : "New application"}</h1>
        <Button variant="outline-secondary" onClick={() => navigate("/applications")}>
          Back
        </Button>
      </div>

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
            <Form.Group className="mb-3" controlId="appFramework">
              <Form.Label>Framework</Form.Label>
              <Form.Select
                value={frameworkName}
                onChange={(e) => setFrameworkName(e.target.value)}
              >
                {frameworks.map((f) => (
                  <option key={f.name} value={f.name}>
                    {f.name}
                  </option>
                ))}
              </Form.Select>
            </Form.Group>

            {/* The framework's own settings, rendered from its config_spec — no
                framework-specific code lives here. */}
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

        <RepeatableRows
          title="APIs"
          rows={apis}
          columns={[
            { key: "provider", label: "Provider", placeholder: "rest" },
            { key: "mount", label: "Mount", placeholder: "/api" },
          ]}
          onChange={setApis}
          blank={{ provider: "", mount: "" }}
        />

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
          <Form.Text muted>One directive per line, e.g. `default-src: 'self'`.</Form.Text>
        </Form.Group>

        <Button type="submit" disabled={busy}>
          {busy ? "Saving…" : appId ? "Save changes" : "Create application"}
        </Button>
      </Form>
    </>
  );
}

/** A repeatable list of uniform string-field rows (APIs, static dirs), with
 * add/remove. Generic over the row shape so both lists share one control. */
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
    onChange(rows.map((r, i) => (i === index ? { ...r, [key]: value } : r)));
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
