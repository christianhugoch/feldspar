// Create / edit an LLM provider. The same discipline `FileStoreForm` holds to:
// **no screen knows a specific backend**. A backend declares its settings as
// `FormField`s and `SettingsFields` renders them, so there is no mention of
// OpenAI or Anthropic below — including for the API key, which is a password
// input because the field says `secret`, not because this file knows what an
// `api_key` is.
//
// Two things here are not in the file-store form, and both follow from what a
// provider is:
//
//   - **The key round trip.** An existing provider's key arrives as the
//     sentinel, and sending it back unchanged tells the server to keep what it
//     has. So the form does nothing special on save: it submits what it holds,
//     and the sentinel *is* the "unchanged" signal. What it must not do is
//     rewrite the field, which is why the config is passed through
//     `buildConfig` exactly as any other settings bag.
//   - **Test connection.** A provider that saves fine can still be unusable —
//     a revoked key, a retired model, an endpoint that is down — and none of
//     that is knowable without a request. The button makes the request while
//     the admin is still looking at the form, which is the only place the
//     answer is actionable; without it the first sign of a wrong key is an
//     agent failing in a chat transcript.

import { useEffect, useState, type FormEvent } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Card from "react-bootstrap/Card";
import Form from "react-bootstrap/Form";
import Spinner from "react-bootstrap/Spinner";

import { api, errorMessage } from "../api";
import type {
  CreateLlmProviderRequest,
  ListLlmProviderBackendsResponse,
  ListLlmProvidersResponse,
} from "../client";
import { navigate } from "../App";
import { IconArrowLeft } from "../icons";
import { PageBody, PageHeader } from "../layout";
import { SettingsFields, buildConfig, readConfig } from "../settings";

type BackendInfo = ListLlmProviderBackendsResponse[number];
type ProviderItem = ListLlmProvidersResponse[number];

/** What a Test connection attempt produced. */
type TestResult = { ok: boolean; message: string; model: string };

export function LlmProviderForm({ providerId }: { providerId?: string }) {
  const [backends, setBackends] = useState<BackendInfo[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [testing, setTesting] = useState(false);
  const [test, setTest] = useState<TestResult | null>(null);

  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [backendName, setBackendName] = useState("");
  const [config, setConfig] = useState<Record<string, string>>({});

  useEffect(() => {
    let cancelled = false;
    const run = async () => {
      try {
        const list = await api.listLlmProviderBackends();
        let existing: ProviderItem | undefined;
        if (providerId) {
          existing = (await api.listLlmProviders()).find((p) => p.id === providerId);
          if (!existing) {
            if (!cancelled) setLoadError("That LLM provider no longer exists.");
            return;
          }
        }
        if (cancelled) return;
        setBackends(list);
        if (existing) {
          setName(existing.name);
          setDescription(existing.description);
          setBackendName(existing.backend);
          // Whatever the server sent, sentinel included: the form's job is to
          // hand it back unchanged unless the admin types over it.
          setConfig(readConfig(existing.config));
        } else {
          setBackendName(list[0]?.name ?? "");
        }
      } catch {
        if (!cancelled) setLoadError("Could not load the LLM provider backends.");
      }
    };
    void run();
    return () => {
      cancelled = true;
    };
  }, [providerId]);

  const selected = backends?.find((b) => b.name === backendName);
  const spec = selected?.config_spec ?? [];

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const body: CreateLlmProviderRequest = {
        name: name.trim(),
        description: description.trim(),
        backend: backendName,
        config: buildConfig(spec, config),
      };
      if (providerId) {
        await api.updateLlmProvider(providerId, body);
      } else {
        await api.createLlmProvider(body);
      }
      navigate("/llm-providers");
    } catch (err) {
      setError(errorMessage(err, "Could not save the LLM provider."));
      setBusy(false);
    }
  };

  const runTest = async () => {
    setTesting(true);
    setTest(null);
    setError(null);
    try {
      // The provider's id goes with it when there is one, so a stored key the
      // form only knows as the sentinel still resolves. Without it there is
      // nothing to resolve against, which is exactly the new-provider case —
      // where the admin has just typed the real key.
      const result = await api.testLlmProvider({
        id: providerId ?? null,
        backend: backendName,
        config: buildConfig(spec, config),
        model: null,
      });
      setTest(result);
    } catch (err) {
      // A structurally wrong config (no key, unknown backend) fails as an
      // ordinary request rather than as a provider answer, and reads as the
      // save error it resembles.
      setError(errorMessage(err, "Could not test the connection."));
    }
    setTesting(false);
  };

  if (loadError) {
    return (
      <PageBody>
        <Alert variant="danger">{loadError}</Alert>
      </PageBody>
    );
  }
  if (!backends) {
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
        pretitle="Agents"
        title={providerId ? "Edit LLM provider" : "New LLM provider"}
        actions={
          <Button variant="outline-secondary" onClick={() => navigate("/llm-providers")}>
            <IconArrowLeft className="icon-2" />
            Back
          </Button>
        }
      />
      <PageBody>
        {error && <Alert variant="danger">{error}</Alert>}
        {test && (
          <Alert
            variant={test.ok ? "success" : "danger"}
            onClose={() => setTest(null)}
            dismissible
          >
            <Alert.Heading className="h6">
              {test.ok ? `${test.model} answered` : `${test.model} did not answer`}
            </Alert.Heading>
            {/* The provider's own words either way — on success what the model
                actually replied, so an admin pointed at the wrong endpoint sees
                a wrong answer rather than a green tick. */}
            <div className="text-break small mb-0">{test.message}</div>
          </Alert>
        )}

        <Form onSubmit={submit}>
          <Form.Group className="mb-3" controlId="providerName">
            <Form.Label>Name</Form.Label>
            <Form.Control value={name} required onChange={(e) => setName(e.target.value)} />
            <Form.Text muted>How an agent refers to this provider.</Form.Text>
          </Form.Group>

          <Form.Group className="mb-3" controlId="providerDescription">
            <Form.Label>Description</Form.Label>
            <Form.Control
              value={description}
              onChange={(e) => setDescription(e.target.value)}
            />
          </Form.Group>

          <Card className="mb-3">
            <Card.Header>Backend</Card.Header>
            <Card.Body>
              <Form.Group className="mb-3" controlId="providerBackend">
                <Form.Label>Backend</Form.Label>
                <Form.Select
                  value={backendName}
                  onChange={(e) => setBackendName(e.target.value)}
                >
                  {backends.map((b) => (
                    <option key={b.name} value={b.name}>
                      {b.name}
                    </option>
                  ))}
                </Form.Select>
                <Form.Text muted>
                  Any endpoint speaking the same API is reached by changing the base URL —
                  a gateway, a local server, another vendor.
                </Form.Text>
              </Form.Group>

              {/* The backend's own settings, rendered from its config_spec. The
                  API key is a password field because the spec says `secret`. */}
              <SettingsFields
                spec={spec}
                values={config}
                onChange={(key, v) => setConfig((c) => ({ ...c, [key]: v }))}
                idPrefix="provider-cfg"
              />
            </Card.Body>
          </Card>

          <div className="btn-list">
            <Button type="submit" disabled={busy}>
              {busy ? "Saving…" : providerId ? "Save changes" : "Create provider"}
            </Button>
            <Button
              variant="outline-secondary"
              disabled={testing || busy}
              onClick={() => void runTest()}
            >
              {testing ? "Testing…" : "Test connection"}
            </Button>
          </div>
        </Form>

        <p className="text-muted small mt-3">
          Testing sends one short prompt to the provider, which counts against your
          account like any other request.
        </p>
      </PageBody>
    </>
  );
}
