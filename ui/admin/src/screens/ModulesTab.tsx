// The Settings screen's Modules tab: what is installed, and how to install
// more.
//
// A module is a Saltcorn v1 plugin — an npm package — and this version loads the
// **actions** it supplies. So the list is written around the two questions an
// admin has about one: what did it give me (the actions, with their settings
// rendered from the module's own declaration), and what is wrong with it (it did
// not load, an action's name was taken, it also supplies four things this
// version ignores).
//
// The arithmetic — what counts as a filled-in specifier, how a module's state
// reads — is in `modules.ts`, tested without a browser. What is here is the
// flow: install, configure, reload, delete, and the busy states between.
//
// Installing a module runs `npm install` and then somebody else's JavaScript in
// a Node process with this server's privileges. That is said once, on the
// screen, rather than assumed: there is no sandbox in this version.

import { useCallback, useEffect, useState } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Form from "react-bootstrap/Form";

import { api, errorMessage } from "../api";
import { AlertBody, StatusBadge } from "../layout";
import {
  EMPTY_INSTALL,
  configValues,
  installBlocked,
  isConfigurable,
  locationLabel,
  locationPlaceholder,
  moduleStatus,
  moduleSubtitle,
  unsupportedSentence,
  type InstallForm,
  type Module,
  type ModuleSource,
} from "../modules";
import { SettingField, buildConfig, initialValues, type FieldSpec } from "../settings";

export function ModulesTab() {
  const [modules, setModules] = useState<Module[] | null>(null);
  const [root, setRoot] = useState("");
  const [npm, setNpm] = useState(true);
  const [node, setNode] = useState(true);
  const [form, setForm] = useState<InstallForm>(EMPTY_INSTALL);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const response = await api.listModules();
      setModules(response.modules);
      setRoot(response.root);
      setNpm(response.npm);
      setNode(response.node);
    } catch (e) {
      setError(errorMessage(e, "Could not read the installed modules."));
      setModules([]);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const install = async () => {
    setBusy("Installing…");
    setError(null);
    setNote(null);
    try {
      const installed = await api.installModule({
        source: form.source,
        location: form.location.trim(),
      });
      setForm(EMPTY_INSTALL);
      setNote(
        `${installed.name} ${installed.version ?? ""} installed, supplying ${
          installed.actions.length
        } action${installed.actions.length === 1 ? "" : "s"}.`,
      );
      await load();
    } catch (e) {
      setError(errorMessage(e, "The module could not be installed."));
    } finally {
      setBusy(null);
    }
  };

  const reload = async () => {
    setBusy("Reloading…");
    setError(null);
    setNote(null);
    try {
      const { modules: count } = await api.reloadModules();
      setNote(`${count} module${count === 1 ? "" : "s"} reloaded.`);
      await load();
    } catch (e) {
      setError(errorMessage(e, "The modules could not be reloaded."));
    } finally {
      setBusy(null);
    }
  };

  const remove = async (module: Module) => {
    if (!window.confirm(`Remove ${module.name} and the actions it supplies?`)) return;
    setBusy("Removing…");
    setError(null);
    setNote(null);
    try {
      await api.deleteModule(module.id);
      setNote(`${module.name} removed.`);
      await load();
    } catch (e) {
      setError(errorMessage(e, "The module could not be removed."));
    } finally {
      setBusy(null);
    }
  };

  const configure = async (module: Module, values: Record<string, string>) => {
    setBusy("Saving…");
    setError(null);
    setNote(null);
    try {
      await api.updateModule(module.id, {
        configuration: buildConfig(module.config_spec as FieldSpec[], values),
      });
      setNote(`${module.name} configured.`);
      await load();
    } catch (e) {
      setError(errorMessage(e, "The module's settings could not be saved."));
    } finally {
      setBusy(null);
    }
  };

  const blocked = installBlocked(form, npm);

  return (
    <>
      {error && (
        <Alert variant="danger" dismissible onClose={() => setError(null)}>
          <AlertBody>{error}</AlertBody>
        </Alert>
      )}
      {note && (
        <Alert variant="success" dismissible onClose={() => setNote(null)}>
          <AlertBody>{note}</AlertBody>
        </Alert>
      )}
      {!node && (
        <Alert variant="warning">
          <AlertBody>
            This server has no Node.js on its PATH. Modules run in a Node process, so an
            installed module cannot load until Node.js is installed and Saltcorn is restarted.
          </AlertBody>
        </Alert>
      )}

      <div className="card mb-3">
        <div className="card-header">
          <h3 className="card-title">Install a module</h3>
          <div className="card-actions">
            <Button variant="outline-secondary" size="sm" disabled={!!busy} onClick={() => void reload()}>
              Reload modules
            </Button>
          </div>
        </div>
        <div className="card-body">
          <p className="text-secondary">
            A module is a Saltcorn plugin: an npm package that supplies actions your triggers
            can run. Installing one runs <code>npm install</code> and then the module's own
            code, with this server's privileges and its network — so install modules you
            trust. Packages are installed under <code>{root}</code>.
          </p>
          <div className="row g-2 align-items-end">
            <div className="col-md-3">
              <Form.Group controlId="module-source">
                <Form.Label>Type</Form.Label>
                <Form.Select
                  value={form.source}
                  onChange={(e) =>
                    setForm({ source: e.target.value as ModuleSource, location: "" })
                  }
                >
                  <option value="npm">JavaScript — npm package</option>
                  <option value="local">JavaScript — local directory</option>
                </Form.Select>
              </Form.Group>
            </div>
            <div className="col-md-7">
              <Form.Group controlId="module-location">
                <Form.Label>{locationLabel(form.source)}</Form.Label>
                <Form.Control
                  type="text"
                  value={form.location}
                  placeholder={locationPlaceholder(form.source)}
                  onChange={(e) => setForm({ ...form, location: e.target.value })}
                />
              </Form.Group>
            </div>
            <div className="col-md-2">
              <Button
                className="w-100"
                disabled={!!busy || blocked !== null}
                title={blocked ?? undefined}
                onClick={() => void install()}
              >
                {busy === "Installing…" ? "Installing…" : "Install"}
              </Button>
            </div>
          </div>
          {blocked && <div className="text-secondary mt-2">{blocked}</div>}
        </div>
      </div>

      {modules === null ? (
        <div className="text-secondary">Loading…</div>
      ) : modules.length === 0 ? (
        <div className="card">
          <div className="card-body text-secondary">
            No modules are installed. Try <code>@saltcorn/mqtt</code>, which supplies an action
            that publishes a row to an MQTT broker.
          </div>
        </div>
      ) : (
        modules.map((module) => (
          <ModuleCard
            key={module.id}
            module={module}
            busy={!!busy}
            onRemove={() => void remove(module)}
            onConfigure={(values) => void configure(module, values)}
          />
        ))
      )}
    </>
  );
}

/** One installed module: what it supplies, what is wrong with it, and its own
 * settings. */
function ModuleCard({
  module,
  busy,
  onRemove,
  onConfigure,
}: {
  module: Module;
  busy: boolean;
  onRemove: () => void;
  onConfigure: (values: Record<string, string>) => void;
}) {
  const status = moduleStatus(module);
  const census = unsupportedSentence(module);
  const [values, setValues] = useState<Record<string, string>>(() =>
    initialValues(module.config_spec as FieldSpec[], configValues(module)),
  );
  const [open, setOpen] = useState(false);

  return (
    <div className="card mb-3">
      <div className="card-header">
        <div>
          <h3 className="card-title">
            {module.name} <StatusBadge tone={status.tone}>{status.label}</StatusBadge>
          </h3>
          <div className="text-secondary">{moduleSubtitle(module)}</div>
        </div>
        <div className="card-actions btn-list">
          {isConfigurable(module) && (
            <Button
              variant="outline-secondary"
              size="sm"
              onClick={() => setOpen((showing) => !showing)}
            >
              {open ? "Close settings" : "Settings"}
            </Button>
          )}
          <Button variant="outline-danger" size="sm" disabled={busy} onClick={onRemove}>
            Remove
          </Button>
        </div>
      </div>
      <div className="card-body">
        {module.issues.length > 0 && (
          <Alert variant="warning">
            <AlertBody>
              <ul className="mb-0">
                {module.issues.map((issue) => (
                  <li key={issue}>{issue}</li>
                ))}
              </ul>
            </AlertBody>
          </Alert>
        )}

        {module.actions.length > 0 && (
          <>
            <div className="text-secondary mb-1">Actions</div>
            <ul className="list-unstyled mb-2">
              {module.actions.map((action) => (
                <li key={action.name}>
                  <code>{action.name}</code>
                  {action.description && <span className="text-secondary"> — {action.description}</span>}
                </li>
              ))}
            </ul>
          </>
        )}

        {census && <div className="text-secondary">{census}</div>}

        {open && isConfigurable(module) && (
          <form
            className="mt-3"
            onSubmit={(e) => {
              e.preventDefault();
              onConfigure(values);
            }}
          >
            {(module.config_spec as FieldSpec[]).map((field) => (
              <SettingField
                key={field.name}
                field={field}
                value={values[field.name] ?? ""}
                idPrefix={`module-${module.id}`}
                onChange={(value) =>
                  setValues((current) => ({ ...current, [field.name]: value }))
                }
              />
            ))}
            <Button type="submit" size="sm" disabled={busy}>
              Save settings
            </Button>
          </form>
        )}
      </div>
    </div>
  );
}
