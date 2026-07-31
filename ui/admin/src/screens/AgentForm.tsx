// Create / edit an agent: a provider, a prompt, and the traits it is given.
//
// The trait picker is the part worth reading. A trait declares its
// configuration as `FormField`s and `SettingsFields` renders it, so **no screen
// knows a specific trait** — `query_table`'s table box and `run_trigger`'s
// trigger box are the same code, and a trait added by a plugin gets a working
// form with no change here.
//
// The one thing that is not the file-store/trigger form's shape: a trait may be
// enabled **more than once** (§11.2). "Query books" and "query orders" are one
// trait with two configurations, so the enabled traits are a *list* the admin
// adds to, each entry carrying its own settings — and the order is the order the
// tools are offered to the model in.

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
  CreateAgentRequest,
  ListAgentTraitsResponse,
  ListAgentsResponse,
  ListLlmProvidersResponse,
} from "../client";
import { navigate } from "../App";
import { IconArrowLeft } from "../icons";
import { PageBody, PageHeader } from "../layout";
import { SettingsFields, buildConfig, readConfig } from "../settings";

type TraitInfo = ListAgentTraitsResponse[number];
type AgentItem = ListAgentsResponse[number];
type ProviderItem = ListLlmProvidersResponse[number];

/** One enabled trait as the form holds it: which trait, and its settings as the
 * strings the controls edit. */
type Enabled = { trait: string; config: Record<string, string> };

/** The sparse per-agent attributes (§9) this form offers. Absent means the
 * provider's own default, which is why each is a box that can be left empty
 * rather than a number with a default already in it. */
const ATTRIBUTES = [
  {
    key: "temperature",
    label: "Temperature",
    help: "Blank uses the provider's own default.",
  },
  {
    key: "max_tokens",
    label: "Max tokens per answer",
    help: "Blank uses the provider's own default.",
  },
  {
    key: "max_steps",
    label: "Max steps per run",
    help: "How many times one run may go round the loop. Blank means 20.",
  },
] as const;

export function AgentForm({ agentId }: { agentId?: string }) {
  const [traits, setTraits] = useState<TraitInfo[] | null>(null);
  const [providers, setProviders] = useState<ProviderItem[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [provider, setProvider] = useState("");
  const [model, setModel] = useState("");
  const [systemPrompt, setSystemPrompt] = useState("");
  const [minRole, setMinRole] = useState("");
  const [enabled, setEnabled] = useState<Enabled[]>([]);
  const [attributes, setAttributes] = useState<Record<string, string>>({});
  const [adding, setAdding] = useState("");

  useEffect(() => {
    let cancelled = false;
    const run = async () => {
      try {
        const [traitList, providerList] = await Promise.all([
          api.listAgentTraits(),
          api.listLlmProviders(),
        ]);
        let existing: AgentItem | undefined;
        if (agentId) {
          existing = (await api.listAgents()).find((a) => a.id === agentId);
          if (!existing) {
            if (!cancelled) setLoadError("That agent no longer exists.");
            return;
          }
        }
        if (cancelled) return;
        setTraits(traitList);
        setProviders(providerList);
        setAdding(traitList[0]?.name ?? "");
        if (existing) {
          setName(existing.name);
          setDescription(existing.description);
          setProvider(existing.provider);
          setModel(existing.model ?? "");
          setSystemPrompt(existing.system_prompt);
          setMinRole(existing.min_role == null ? "" : String(existing.min_role));
          setEnabled(
            existing.traits.map((t) => ({
              trait: t.trait,
              config: readConfig(t.config),
            })),
          );
          setAttributes(readConfig(existing.attributes));
        } else {
          setProvider(providerList[0]?.name ?? "");
        }
      } catch {
        if (!cancelled) setLoadError("Could not load the traits and providers.");
      }
    };
    void run();
    return () => {
      cancelled = true;
    };
  }, [agentId]);

  const specOf = (trait: string) => traits?.find((t) => t.name === trait)?.config_spec ?? [];

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const body: CreateAgentRequest = {
        name: name.trim(),
        description: description.trim(),
        provider,
        model: model.trim() === "" ? null : model.trim(),
        system_prompt: systemPrompt,
        traits: enabled.map((entry) => ({
          trait: entry.trait,
          config: buildConfig(specOf(entry.trait), entry.config),
        })),
        min_role: minRole.trim() === "" ? null : Number(minRole),
        // The sparse attributes: what was typed, nothing that was not. An empty
        // box is not a zero.
        attributes: Object.fromEntries(
          ATTRIBUTES.filter(({ key }) => (attributes[key] ?? "").trim() !== "").map(({ key }) => [
            key,
            Number(attributes[key]),
          ]),
        ),
      };
      if (agentId) {
        await api.updateAgent(agentId, body);
      } else {
        await api.createAgent(body);
      }
      navigate("/agents");
    } catch (err) {
      // The server's own refusal — "no LLM provider named `house`", "its tool
      // `query_books` has the same name as one from trait 1" — is the message
      // that says what to fix.
      setError(errorMessage(err, "Could not save the agent."));
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
  if (!traits) {
    return (
      <PageBody>
        <div className="py-5 text-center">
          <Spinner animation="border" role="status" />
        </div>
      </PageBody>
    );
  }

  return (
    <>
      <PageHeader
        pretitle="Agents"
        title={agentId ? "Edit agent" : "New agent"}
        actions={
          <Button variant="outline-secondary" onClick={() => navigate("/agents")}>
            <IconArrowLeft className="icon-2" />
            Back
          </Button>
        }
      />
      <PageBody>
        {error && <Alert variant="danger">{error}</Alert>}
        {providers.length === 0 && (
          <Alert variant="warning">
            No LLM providers are configured, so this agent will have nothing to talk to.{" "}
            <Alert.Link href="#/llm-providers/new">Connect one first.</Alert.Link>
          </Alert>
        )}

        <Form onSubmit={(e) => void submit(e)}>
          <Row>
            <Col md={6}>
              <Form.Group className="mb-3" controlId="agentName">
                <Form.Label>
                  Name<span className="text-danger"> *</span>
                </Form.Label>
                <Form.Control value={name} required onChange={(e) => setName(e.target.value)} />
                <Form.Text muted>
                  How a trigger and the chat panel refer to this agent. Renaming it breaks
                  those references deliberately.
                </Form.Text>
              </Form.Group>
            </Col>
            <Col md={6}>
              <Form.Group className="mb-3" controlId="agentMinRole">
                <Form.Label>Minimum role</Form.Label>
                <Form.Control
                  type="number"
                  min={1}
                  max={100}
                  value={minRole}
                  placeholder="admin only"
                  onChange={(e) => setMinRole(e.target.value)}
                />
                <Form.Text muted>
                  Who may chat with this agent. 1 is admin, 100 is public. Leave blank for
                  admin only.
                </Form.Text>
              </Form.Group>
            </Col>
          </Row>

          <Form.Group className="mb-3" controlId="agentDescription">
            <Form.Label>Description</Form.Label>
            <Form.Control
              value={description}
              onChange={(e) => setDescription(e.target.value)}
            />
          </Form.Group>

          <Card className="mb-3">
            <Card.Header>Model</Card.Header>
            <Card.Body>
              <Row>
                <Col md={6}>
                  <Form.Group className="mb-3" controlId="agentProvider">
                    <Form.Label>
                      LLM provider<span className="text-danger"> *</span>
                    </Form.Label>
                    <Form.Select
                      value={provider}
                      onChange={(e) => setProvider(e.target.value)}
                    >
                      {/* A provider that was deleted out from under a saved
                          agent still has to be shown, or saving this form would
                          silently repoint the agent at another one. */}
                      {providers.every((p) => p.name !== provider) && provider !== "" && (
                        <option value={provider}>{provider} (missing)</option>
                      )}
                      {providers.map((p) => (
                        <option key={p.id} value={p.name}>
                          {p.name}
                        </option>
                      ))}
                    </Form.Select>
                  </Form.Group>
                </Col>
                <Col md={6}>
                  <Form.Group className="mb-3" controlId="agentModel">
                    <Form.Label>Model</Form.Label>
                    <Form.Control
                      value={model}
                      placeholder="the provider's default"
                      onChange={(e) => setModel(e.target.value)}
                    />
                    <Form.Text muted>
                      Overrides the model the provider was configured with.
                    </Form.Text>
                  </Form.Group>
                </Col>
              </Row>

              <Form.Group className="mb-3" controlId="agentPrompt">
                <Form.Label>System prompt</Form.Label>
                <Form.Control
                  as="textarea"
                  rows={5}
                  value={systemPrompt}
                  onChange={(e) => setSystemPrompt(e.target.value)}
                />
                <Form.Text muted>
                  What the agent is told it is, before anything the conversation adds.
                </Form.Text>
              </Form.Group>

              <Row>
                {ATTRIBUTES.map((attr) => (
                  <Col md={4} key={attr.key}>
                    <Form.Group className="mb-3" controlId={`agent-${attr.key}`}>
                      <Form.Label>{attr.label}</Form.Label>
                      <Form.Control
                        type="number"
                        step="any"
                        value={attributes[attr.key] ?? ""}
                        onChange={(e) =>
                          setAttributes((a) => ({ ...a, [attr.key]: e.target.value }))
                        }
                      />
                      <Form.Text muted>{attr.help}</Form.Text>
                    </Form.Group>
                  </Col>
                ))}
              </Row>
            </Card.Body>
          </Card>

          <Card className="mb-3">
            <Card.Header>Traits</Card.Header>
            <Card.Body>
              {enabled.length === 0 && (
                <p className="text-muted">
                  No traits: this agent can talk, and can do nothing else. Each trait you add
                  is one deliberate grant.
                </p>
              )}

              {enabled.map((entry, index) => (
                <Card className="mb-3" key={`${entry.trait}-${index}`}>
                  <Card.Header className="d-flex align-items-center justify-content-between">
                    <div>
                      <strong>{entry.trait}</strong>
                      <div className="text-muted small">
                        {traits.find((t) => t.name === entry.trait)?.description ??
                          "This trait is not registered on this server."}
                      </div>
                    </div>
                    <Button
                      size="sm"
                      variant="outline-danger"
                      onClick={() => setEnabled((list) => list.filter((_, i) => i !== index))}
                    >
                      Remove
                    </Button>
                  </Card.Header>
                  <Card.Body>
                    <SettingsFields
                      spec={specOf(entry.trait)}
                      values={entry.config}
                      onChange={(key, value) =>
                        setEnabled((list) =>
                          list.map((item, i) =>
                            i === index
                              ? { ...item, config: { ...item.config, [key]: value } }
                              : item,
                          ),
                        )
                      }
                      idPrefix={`trait-${index}`}
                    />
                  </Card.Body>
                </Card>
              ))}

              <div className="d-flex gap-2 align-items-start">
                <Form.Select
                  value={adding}
                  onChange={(e) => setAdding(e.target.value)}
                  aria-label="Trait to add"
                  className="w-auto"
                >
                  {traits.map((t) => (
                    <option key={t.name} value={t.name}>
                      {t.name}
                    </option>
                  ))}
                </Form.Select>
                <Button
                  variant="outline-secondary"
                  onClick={() =>
                    adding !== "" &&
                    setEnabled((list) => [...list, { trait: adding, config: {} }])
                  }
                >
                  Add trait
                </Button>
              </div>
              <p className="text-muted small mt-3 mb-0">
                A trait can be added more than once — one <code>query_table</code> for each
                table it may read. Two that would produce the same tool name are refused when
                you save.
              </p>
            </Card.Body>
          </Card>

          <div className="btn-list">
            <Button type="submit" disabled={busy}>
              {busy ? "Saving…" : agentId ? "Save changes" : "Create agent"}
            </Button>
          </div>
        </Form>
      </PageBody>
    </>
  );
}
