// Create / edit a file store. The same point `ApplicationForm` proves, for
// backends: **no screen knows a specific backend's settings**. The admin picks a
// backend and the form renders whatever that backend's `config_spec` declares —
// there is no `local`-backend-specific code here, so an S3 or git-remote backend
// added to the registry gets a working form with no change to this file.
//
// Saving does not require the store to be reachable (§1.2): a well-formed
// definition whose directory is missing is saved, and the failure to connect is
// reported. That is deliberate — demanding a reachable directory would make a
// store whose disk was unmounted uneditable, and editing it is the repair.

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
  CreateFileStoreRequest,
  ListFileStoreBackendsResponse,
  ListFileStoresResponse,
} from "../client";
import { navigate } from "../App";
import { IconArrowLeft } from "../icons";
import { AlertBody, PageBody, PageHeader } from "../layout";
import { SettingsFields, buildConfig, readConfig } from "../settings";

type BackendInfo = ListFileStoreBackendsResponse[number];
type StoreItem = ListFileStoresResponse[number];

export function FileStoreForm({ storeId }: { storeId?: string }) {
  const [backends, setBackends] = useState<BackendInfo[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  // A save that succeeded but left the store unconnected: not an error, but the
  // admin needs to know the store is not usable yet and why.
  const [warning, setWarning] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [backendName, setBackendName] = useState("");
  const [config, setConfig] = useState<Record<string, string>>({});
  const [minRole, setMinRole] = useState("");

  useEffect(() => {
    let cancelled = false;
    const run = async () => {
      try {
        const list = await api.listFileStoreBackends();
        let existing: StoreItem | undefined;
        if (storeId) {
          existing = (await api.listFileStores()).find((s) => s.id === storeId);
          if (!existing) {
            if (!cancelled) setLoadError("That file store no longer exists.");
            return;
          }
        }
        if (cancelled) return;
        setBackends(list);
        if (existing) {
          setName(existing.name);
          setDescription(existing.description);
          setBackendName(existing.backend);
          setConfig(readConfig(existing.config));
          setMinRole(existing.min_role == null ? "" : String(existing.min_role));
        } else {
          setBackendName(list[0]?.name ?? "");
        }
      } catch {
        if (!cancelled) setLoadError("Could not load the file-store backends.");
      }
    };
    void run();
    return () => {
      cancelled = true;
    };
  }, [storeId]);

  const selected = backends?.find((b) => b.name === backendName);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    setWarning(null);
    try {
      const body: CreateFileStoreRequest = {
        name: name.trim(),
        description: description.trim(),
        backend: backendName,
        config: buildConfig(selected?.config_spec ?? [], config),
        min_role: minRole.trim() === "" ? null : Number(minRole),
      };
      const saved = storeId
        ? await api.updateFileStore(storeId, body)
        : await api.createFileStore(body);

      // Saved, but is it usable? The server connects on save and reports the
      // outcome, so the admin finds out now rather than at the next restart.
      if (!saved.connected) {
        setWarning(
          saved.error ??
            "The store was saved but could not be connected. Check its settings.",
        );
        setBusy(false);
        return;
      }
      navigate("/file-stores");
    } catch (err) {
      setError(errorMessage(err, "Could not save the file store."));
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
        pretitle="Storage"
        title={storeId ? "Edit file store" : "New file store"}
        actions={
          <Button variant="outline-secondary" onClick={() => navigate("/file-stores")}>
            <IconArrowLeft className="icon-2" />
            Back
          </Button>
        }
      />
      <PageBody>
        {error && <Alert variant="danger">{error}</Alert>}
        {warning && (
          <Alert variant="warning" onClose={() => setWarning(null)} dismissible>
            <AlertBody>
              <Alert.Heading className="h6">Saved, but not connected</Alert.Heading>
              <div className="text-break">{warning}</div>
              <hr />
              <div className="mb-0 small">
                The definition is stored and you can keep editing it. Fix the settings and save
                again, or go back to the list.
              </div>
            </AlertBody>
          </Alert>
        )}

        <Form onSubmit={submit}>
          <Row>
            <Col md={6}>
              <Form.Group className="mb-3" controlId="storeName">
                <Form.Label>Name</Form.Label>
                <Form.Control
                  value={name}
                  required
                  onChange={(e) => setName(e.target.value)}
                />
                <Form.Text muted>
                  How everything else refers to this store — a field, an application, the file
                  manager.
                </Form.Text>
              </Form.Group>
            </Col>
            <Col md={6}>
              <Form.Group className="mb-3" controlId="storeMinRole">
                <Form.Label>Minimum role</Form.Label>
                <Form.Control
                  type="number"
                  min={1}
                  max={100}
                  value={minRole}
                  placeholder="unrestricted"
                  onChange={(e) => setMinRole(e.target.value)}
                />
                <Form.Text muted>
                  1 is admin, 100 is public; lower is more restrictive. Leave blank for no
                  store-wide restriction. Applies before any per-file rule.
                </Form.Text>
              </Form.Group>
            </Col>
          </Row>

          <Form.Group className="mb-3" controlId="storeDescription">
            <Form.Label>Description</Form.Label>
            <Form.Control
              value={description}
              onChange={(e) => setDescription(e.target.value)}
            />
          </Form.Group>

          <Card className="mb-3">
            <Card.Header>Backend</Card.Header>
            <Card.Body>
              <Form.Group className="mb-3" controlId="storeBackend">
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
              </Form.Group>

              {/* The backend's own settings, rendered from its config_spec — no
                  backend-specific code lives here. */}
              <SettingsFields
                spec={selected?.config_spec ?? []}
                values={config}
                onChange={(key, v) => setConfig((c) => ({ ...c, [key]: v }))}
                idPrefix="store-cfg"
              />
            </Card.Body>
          </Card>

          <Button type="submit" disabled={busy}>
            {busy ? "Saving…" : storeId ? "Save changes" : "Create file store"}
          </Button>
        </Form>
      </PageBody>
    </>
  );
}
