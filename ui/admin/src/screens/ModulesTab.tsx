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
// **Two different privileges, and the screen has to keep them apart.** Installing
// a module runs `npm install` — somebody else's install scripts, as the server,
// before anything is sandboxed. *Running* one does not: a module's worker gets
// the permission set on this screen, closed unless an admin granted something.
// Both sentences are on the screen, because the presence of a permissions form
// would otherwise imply the first one had been solved too.

import { useCallback, useEffect, useState } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Form from "react-bootstrap/Form";

import { api, errorMessage } from "../api";
import { AlertBody, StatusBadge } from "../layout";
import {
  EMPTY_INSTALL,
  PERMISSION_KINDS,
  configValues,
  installBlocked,
  isClosed,
  isConfigurable,
  locationLabel,
  locationPlaceholder,
  modulePermissions,
  moduleStatus,
  moduleSubtitle,
  parsePermissionText,
  permissionProblems,
  permissionSummary,
  permissionText,
  unsupportedSentence,
  type InstallForm,
  type Module,
  type ModulePermissionSet,
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

  const grant = async (module: Module, permissions: ModulePermissionSet) => {
    setBusy("Saving…");
    setError(null);
    setNote(null);
    try {
      await api.updateModule(module.id, { permissions });
      setNote(
        isClosed(permissions)
          ? `${module.name} may now reach nothing.`
          : `${module.name}'s permissions saved. It runs on its own worker while its permissions differ from the other modules'.`,
      );
      await load();
    } catch (e) {
      setError(errorMessage(e, "The module's permissions could not be saved."));
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
            This server has no Node.js on its PATH. Modules <em>run</em> inside Saltcorn and do
            not need it — but <code>npm</code> is what installs one, so nothing new can be
            installed until Node.js is.
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
            can run, and functions your formulas and code bodies can call. It runs inside
            Saltcorn, on a worker that reaches only what you grant it under
            <strong> Permissions</strong> — nothing, until you do.{" "}
            <strong>Installing</strong> one is a different matter and is not sandboxed:{" "}
            <code>npm install</code> runs the package&apos;s own install scripts with this
            server&apos;s privileges, so install modules you trust. Packages are installed under{" "}
            <code>{root}</code>.
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
            onGrant={(permissions) => void grant(module, permissions)}
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
  onGrant,
}: {
  module: Module;
  busy: boolean;
  onRemove: () => void;
  onConfigure: (values: Record<string, string>) => void;
  onGrant: (permissions: ModulePermissionSet) => void;
}) {
  const status = moduleStatus(module);
  const census = unsupportedSentence(module);
  const [values, setValues] = useState<Record<string, string>>(() =>
    initialValues(module.config_spec as FieldSpec[], configValues(module)),
  );
  const [open, setOpen] = useState(false);
  const [showPermissions, setShowPermissions] = useState(false);
  const permissions = modulePermissions(module);

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
          <Button
            variant="outline-secondary"
            size="sm"
            onClick={() => setShowPermissions((showing) => !showing)}
          >
            {showPermissions ? "Close permissions" : "Permissions"}
          </Button>
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

        {module.functions.length > 0 && (
          <>
            <div className="text-secondary mb-1">Functions</div>
            <ul className="list-unstyled mb-2">
              {module.functions.map((fn) => (
                <li key={fn.name}>
                  <code>
                    {fn.name}({fn.arguments.map((argument) => argument.name).join(", ")})
                  </code>
                  {fn.description && <span className="text-secondary"> — {fn.description}</span>}
                </li>
              ))}
            </ul>
            <div className="text-secondary mb-2">
              A code body calls these with <code>await</code>, including the ones this module
              wrote synchronously; a formula calls them by name, and the call is resolved before
              the formula runs.
            </div>
          </>
        )}

        {module.table_providers.length > 0 && (
          <>
            <div className="text-secondary mb-1">Table providers</div>
            <ul className="list-unstyled mb-2">
              {module.table_providers.map((provider) => (
                <li key={provider}>
                  <code>{provider}</code>
                </li>
              ))}
            </ul>
            <div className="text-secondary mb-2">
              Create a table from one under Data → Tables → New table. Saltcorn reads its rows;
              it does not write them.
            </div>
          </>
        )}

        {census && <div className="text-secondary">{census}</div>}

        <div className="text-secondary mt-2">{permissionSummary(permissions)}</div>

        {showPermissions && (
          <PermissionsForm
            module={module}
            busy={busy}
            permissions={permissions}
            onGrant={onGrant}
          />
        )}

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

/** What one module's worker may reach, edited.
 *
 * Four textareas, one entry per line, because the entries are hosts and absolute
 * paths and both may contain a comma. The sentence about `npm install` is here
 * rather than only at the top of the tab, and it is here **because** this form
 * exists: a screen that grants permissions invites the reading that installing
 * one is sandboxed too, and it is not.
 */
function PermissionsForm({
  module,
  busy,
  permissions,
  onGrant,
}: {
  module: Module;
  busy: boolean;
  permissions: ModulePermissionSet;
  onGrant: (permissions: ModulePermissionSet) => void;
}) {
  const [text, setText] = useState<Record<string, string>>(() =>
    Object.fromEntries(
      PERMISSION_KINDS.map(({ key }) => [key, permissionText(permissions[key])]),
    ),
  );
  const edited: ModulePermissionSet = {
    net: parsePermissionText(text.net ?? ""),
    read: parsePermissionText(text.read ?? ""),
    write: parsePermissionText(text.write ?? ""),
    env: parsePermissionText(text.env ?? ""),
  };
  const problems = permissionProblems(edited);

  return (
    <form
      className="mt-3"
      onSubmit={(e) => {
        e.preventDefault();
        onGrant(edited);
      }}
    >
      <p className="text-secondary">
        A module reaches nothing it is not granted here — no host, no file, no environment
        variable — and it always runs on a worker of its own while its permissions differ from
        the other modules&apos;. <strong>Installing</strong> a module is not sandboxed:{" "}
        <code>npm install</code> and the package&apos;s own install scripts run as this server,
        before any of this applies.
      </p>
      {PERMISSION_KINDS.map((kind) => (
        <Form.Group className="mb-2" controlId={`module-${module.id}-${kind.key}`} key={kind.key}>
          <Form.Label>{kind.label}</Form.Label>
          <Form.Control
            as="textarea"
            rows={2}
            value={text[kind.key] ?? ""}
            placeholder={kind.placeholder}
            onChange={(e) => setText((current) => ({ ...current, [kind.key]: e.target.value }))}
          />
          <Form.Text className="text-secondary">{kind.help}</Form.Text>
        </Form.Group>
      ))}
      {problems.length > 0 && (
        <Alert variant="warning">
          <AlertBody>
            <ul className="mb-0">
              {problems.map((problem) => (
                <li key={problem}>{problem}</li>
              ))}
            </ul>
          </AlertBody>
        </Alert>
      )}
      <Button type="submit" size="sm" disabled={busy || problems.length > 0}>
        Save permissions
      </Button>
    </form>
  );
}
