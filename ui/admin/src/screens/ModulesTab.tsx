// The Settings screen's Modules tab: what is installed, and how to install
// more.
//
// A module is a plugin — an npm package or a Python distribution — and this
// version loads the **actions**, **functions** and **table providers** it
// supplies. So the list is written around the two questions an admin has about
// one: what did it give me (with its settings rendered from the module's own
// declaration), and what is wrong with it (it did not load, an action's name was
// taken, it also supplies things this version ignores).
//
// The arithmetic — what counts as a filled-in specifier, how a module's state
// reads — is in `modules.ts`, tested without a browser. What is here is the
// flow: install, configure, reload, delete, and the busy states between.
//
// **Two languages, and the screen is honest about what each one gets** (§8, §10).
// One list, one install form, one set of endpoints, because a module is a module
// to an admin. What the language changes is where the package comes from — npm
// or PyPI — and whether the permissions form exists at all: a JavaScript module
// runs on a worker with an allow-list, and a Python one runs in the server's own
// interpreter with the server's own privileges. The Python card says that in
// words, in the place the other card's form is.
//
// **And installing is not sandboxed in either language.** `npm install` and
// `pip install` both run somebody else's install scripts as the server, before
// any worker exists. That sentence is on the screen beside the Install button,
// because the presence of a permissions form would otherwise imply it had been
// solved too.

import { useCallback, useEffect, useState } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Form from "react-bootstrap/Form";

import { api, errorMessage } from "../api";
import { AlertBody, StatusBadge } from "../layout";
import {
  ALL_TOOLCHAINS,
  EMPTY_INSTALL,
  INSTALL_CHOICES,
  NO_SANDBOX,
  PERMISSION_KINDS,
  PYTHON_RELOAD,
  bundledBlocked,
  bundledSubtitle,
  catalogOrder,
  choiceFor,
  choiceValue,
  configValues,
  grantSentence,
  hasPermissions,
  installBlocked,
  installsSentence,
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
  suppliedSummary,
  toolchainSentence,
  unsupportedSentence,
  type BundledModule,
  type InstallForm,
  type Module,
  type ModulePermissionSet,
  type Toolchains,
} from "../modules";
import { SettingField, buildConfig, initialValues, type FieldSpec } from "../settings";

export function ModulesTab() {
  const [modules, setModules] = useState<Module[] | null>(null);
  const [bundled, setBundled] = useState<BundledModule[]>([]);
  const [root, setRoot] = useState("");
  // Everything present until the server has answered, so the form is not
  // briefly and wrongly disabled on the way in.
  const [tools, setTools] = useState<Toolchains>(ALL_TOOLCHAINS);
  const [pythonDir, setPythonDir] = useState<string | null>(null);
  const [form, setForm] = useState<InstallForm>(EMPTY_INSTALL);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const response = await api.listModules();
      setModules(response.modules);
      setBundled(catalogOrder(response.bundled));
      setRoot(response.root);
      setTools({
        npm: response.npm,
        node: response.node,
        python: response.python,
        pip: response.pip,
      });
      setPythonDir(response.python_dir ?? null);
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
        language: form.language,
        source: form.source,
        location: form.location.trim(),
      });
      setForm(EMPTY_INSTALL);
      setNote(
        `${installed.name} ${installed.version ?? ""} installed, supplying ${suppliedSummary(
          installed,
        )}.`,
      );
      await load();
    } catch (e) {
      setError(errorMessage(e, "The module could not be installed."));
    } finally {
      setBusy(null);
    }
  };

  /** One click on a catalog card. The id is the whole request: the language,
   * the directory it is installed from and the permissions it is granted are
   * the server's answers, not this form's. */
  const installBundled = async (entry: BundledModule) => {
    setBusy(`Installing ${entry.id}…`);
    setError(null);
    setNote(null);
    try {
      const installed = await api.installModule({ source: "bundled", location: entry.id });
      setNote(
        `${installed.name} ${installed.version ?? ""} installed, supplying ${suppliedSummary(
          installed,
        )}.`,
      );
      await load();
    } catch (e) {
      setError(errorMessage(e, `${entry.title} could not be installed.`));
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

  const blocked = installBlocked(form, tools);

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
      {!tools.node && (
        <Alert variant="warning">
          <AlertBody>
            This server has no Node.js on its PATH. Modules <em>run</em> inside Saltcorn and do
            not need it — but <code>npm</code> is what installs one, so no new JavaScript module
            can be installed until Node.js is.
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
            A module is a Saltcorn plugin: a package that supplies actions your triggers can
            run, functions your formulas and code bodies can call, and tables Saltcorn can read.
            A <strong>JavaScript</strong> module is an npm package and runs on a worker that
            reaches only what you grant it under <strong>Permissions</strong> — nothing, until
            you do. A <strong>Python</strong> module is a distribution installed into this
            server&apos;s Python environment and runs in the server&apos;s own interpreter, with
            the server&apos;s own privileges: there is no sandbox for one and nothing to grant.{" "}
            <strong>Installing</strong> either is not sandboxed: <code>npm install</code> and{" "}
            <code>pip install</code> run the package&apos;s own install scripts with this
            server&apos;s privileges, so install only modules you trust. npm packages are
            installed under <code>{root}</code>
            {pythonDir ? (
              <>
                {" "}
                and Python distributions into <code>{pythonDir}</code>
              </>
            ) : null}
            .
          </p>
          <div className="row g-2 align-items-end">
            <div className="col-md-3">
              <Form.Group controlId="module-source">
                <Form.Label>Type</Form.Label>
                <Form.Select
                  value={choiceValue(form)}
                  onChange={(e) => {
                    const { language, source } = choiceFor(e.target.value);
                    // The specifier is cleared with the choice: a distribution
                    // name is not an npm package name and a path is neither.
                    setForm({ language, source, location: "" });
                  }}
                >
                  {INSTALL_CHOICES.map((choice) => (
                    <option key={choiceValue(choice)} value={choiceValue(choice)}>
                      {choice.label}
                    </option>
                  ))}
                </Form.Select>
              </Form.Group>
            </div>
            <div className="col-md-7">
              <Form.Group controlId="module-location">
                <Form.Label>{locationLabel(form)}</Form.Label>
                <Form.Control
                  type="text"
                  value={form.location}
                  placeholder={locationPlaceholder(form)}
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
          {/* Which toolchain this server actually has, said before an admin
              types a name rather than by a failed install — and both languages'
              at once, because a server with one and not the other is ordinary. */}
          <div className="text-secondary mt-2">{toolchainSentence(tools)}</div>
        </div>
      </div>

      {bundled.length > 0 && (
        <div className="card mb-3">
          <div className="card-header">
            <h3 className="card-title">Modules that ship with Saltcorn</h3>
          </div>
          <div className="card-body">
            <p className="text-secondary">
              These are written and maintained here and travel inside Saltcorn itself, so there
              is no package name to look up and nothing to trust beyond what you already run.
              They are still modules: nothing below does anything until you install it. What is
              <em> not</em> shipped is what each one depends on — installing fetches that from{" "}
              <code>npm</code> or <code>PyPI</code>, which is the one part of the click that
              reaches the network.
            </p>
            <div className="row g-3">
              {bundled.map((entry) => (
                <BundledCard
                  key={entry.id}
                  entry={entry}
                  busy={busy}
                  blocked={bundledBlocked(entry, tools)}
                  onInstall={() => void installBundled(entry)}
                />
              ))}
            </div>
          </div>
        </div>
      )}

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

/** One entry in the bundled catalog: what it is, what installing it downloads,
 * what installing it grants, and the button that does all three.
 *
 * The two sentences under the list are the reason a single click is allowed to
 * grant a permission at all. A package cannot grant itself one — that is the
 * whole permission model — but an admin can, and an admin who pressed a button
 * with "installing lets it connect to any host" written beside it has.
 */
function BundledCard({
  entry,
  busy,
  blocked,
  onInstall,
}: {
  entry: BundledModule;
  busy: string | null;
  blocked: string | null;
  onInstall: () => void;
}) {
  const downloads = installsSentence(entry);
  const grant = grantSentence(entry);
  return (
    <div className="col-md-6">
      <div className="card h-100">
        <div className="card-body">
          <h4 className="card-title mb-1">
            {entry.title}{" "}
            {entry.installed && <StatusBadge tone="green">Installed</StatusBadge>}
          </h4>
          <div className="text-secondary mb-2">{bundledSubtitle(entry)}</div>
          <p className="mb-2">{entry.description}</p>
          {entry.supplies.length > 0 && (
            <ul className="text-secondary mb-2">
              {entry.supplies.map((line) => (
                <li key={line}>{line}</li>
              ))}
            </ul>
          )}
          {downloads && <div className="text-secondary mb-1">{downloads}</div>}
          {grant && <div className="text-secondary mb-1">{grant}</div>}
          {blocked && <div className="text-secondary mb-1">{blocked}</div>}
        </div>
        <div className="card-footer">
          {/* Installed is **Reinstall**, not a disabled button. A bundled module
              is upgraded by the release it ships in, so after a Saltcorn upgrade
              the copy on disk is newer than the one installed — and reinstalling
              is the whole of that upgrade. A card that went grey when installed
              would leave no way to do it. */}
          <Button
            size="sm"
            variant={entry.installed ? "outline-secondary" : "primary"}
            disabled={!!busy || blocked !== null}
            title={blocked ?? undefined}
            onClick={onInstall}
          >
            {busy === `Installing ${entry.id}…`
              ? entry.installed
                ? "Reinstalling…"
                : "Installing…"
              : entry.installed
                ? "Reinstall"
                : "Install"}
          </Button>
          {entry.installed && (
            <span className="text-secondary ms-2">
              Reinstalling takes the version in this release, keeping its settings and
              permissions.
            </span>
          )}
        </div>
      </div>
    </div>
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
          {hasPermissions(module) && (
            <Button
              variant="outline-secondary"
              size="sm"
              onClick={() => setShowPermissions((showing) => !showing)}
            >
              {showPermissions ? "Close permissions" : "Permissions"}
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

        {/* What this module may reach. For JavaScript that is the allow-list and
            a form to change it; for Python there is no such thing (§10), and the
            sentence is the whole of what this screen can say — which is why it
            is said here, in the place the other language's summary is, rather
            than left to be inferred from a missing button. */}
        {hasPermissions(module) ? (
          <>
            <div className="text-secondary mt-2">{permissionSummary(permissions)}</div>
            {showPermissions && (
              <PermissionsForm
                module={module}
                busy={busy}
                permissions={permissions}
                onGrant={onGrant}
              />
            )}
          </>
        ) : (
          <>
            <Alert variant="warning" className="mt-2 mb-2">
              <AlertBody>{NO_SANDBOX}</AlertBody>
            </Alert>
            <div className="text-secondary">{PYTHON_RELOAD}</div>
          </>
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
