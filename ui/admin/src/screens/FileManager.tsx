// The file manager: browse one store's tree, and act on what is in it.
//
// The MVP built `browseFiles`/`readFile`/`writeFile` — `readFile`'s UTF-8 `text`
// shortcut is commented in the server as being "for the text editor" — and then
// shipped no screen that called any of them. This is that screen, over the fuller
// operation set §1.4b added: create a folder, upload, download, rename, delete,
// edit a text file in place, and set the per-file access rule.
//
// One idea worth keeping in mind while reading: a file's *effective* minimum role
// is not the same as the rule set on it. A parent directory can be stricter, and
// the rule is cumulative down the path. The permissions dialog shows both, so an
// admin cannot conclude a file is reachable from a permissive rule on the file
// while its folder is locked.

import { useCallback, useEffect, useRef, useState } from "react";
import Alert from "react-bootstrap/Alert";
import Breadcrumb from "react-bootstrap/Breadcrumb";
import Button from "react-bootstrap/Button";
import Form from "react-bootstrap/Form";
import Modal from "react-bootstrap/Modal";
import Spinner from "react-bootstrap/Spinner";
import Table from "react-bootstrap/Table";

import { api, errorMessage, uploadFile } from "../api";
import type { BrowseFilesResponse, GetFileMetaResponse } from "../client";
import { navigate } from "../App";
import { PageBody, PageHeader, StatusBadge } from "../layout";

type Entry = BrowseFilesResponse[number];

/** Human-readable byte size for the listing. */
function formatSize(size: number | null | undefined): string {
  if (size == null) return "";
  const units = ["B", "KB", "MB", "GB"];
  let value = size;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${unit === 0 ? value : value.toFixed(1)} ${units[unit]}`;
}

/** The parent of a store-relative directory path (`""` is the root). */
function parentOf(dir: string): string {
  const trimmed = dir.replace(/\/+$/, "");
  const slash = trimmed.lastIndexOf("/");
  return slash === -1 ? "" : trimmed.slice(0, slash);
}

/** Join a directory and a name into a store-relative path. */
function joinPath(dir: string, name: string): string {
  return dir ? `${dir}/${name}` : name;
}

export function FileManager({
  store,
  initialDir = "",
}: {
  store: string;
  /** The directory to open in; `""` is the store root. */
  initialDir?: string;
}) {
  const [dir, setDir] = useState(initialDir);
  const [entries, setEntries] = useState<Entry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [editing, setEditing] = useState<{ path: string; text: string } | null>(null);
  const [permissions, setPermissions] = useState<GetFileMetaResponse | null>(null);
  const uploadInput = useRef<HTMLInputElement>(null);

  const load = useCallback(
    async (target: string) => {
      setError(null);
      try {
        setEntries(await api.browseFiles(store, { dir: target }));
        setDir(target);
      } catch (err) {
        setEntries([]);
        setError(errorMessage(err, `Could not list "${target || "/"}".`));
      }
    },
    [store],
  );

  useEffect(() => {
    void load(initialDir);
  }, [load, initialDir]);

  /** Run a mutating action, then refresh — the shape every action here shares. */
  const act = async (what: string, run: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await run();
      await load(dir);
    } catch (err) {
      setError(errorMessage(err, what));
    } finally {
      setBusy(false);
    }
  };

  const makeFolder = () => {
    const name = window.prompt("New folder name");
    if (!name?.trim()) return;
    void act("Could not create the folder.", () =>
      api.makeDirectory(store, { path: joinPath(dir, name.trim()) }),
    );
  };

  const upload = (files: FileList | null) => {
    if (!files?.length) return;
    void act("Could not upload the file.", async () => {
      for (const file of Array.from(files)) {
        await uploadFile(store, joinPath(dir, file.name), file);
      }
    });
  };

  const rename = (entry: Entry) => {
    const next = window.prompt(`Rename "${entry.name}" to`, entry.name);
    if (!next?.trim() || next.trim() === entry.name) return;
    void act("Could not rename it.", () =>
      api.renameFile(store, {
        from: entry.path,
        to: joinPath(dir, next.trim()),
      }),
    );
  };

  const remove = (entry: Entry) => {
    const what = entry.is_dir
      ? `Delete the folder "${entry.name}" and everything in it?`
      : `Delete "${entry.name}"?`;
    if (!window.confirm(`${what}\n\nThis cannot be undone.`)) return;
    void act("Could not delete it.", () => api.deleteFile(store, { path: entry.path }));
  };

  const download = async (entry: Entry) => {
    setError(null);
    try {
      const file = await api.readFile(store, { path: entry.path });
      // Rebuild the bytes from base64 and hand them to the browser as a blob —
      // `readFile` always provides base64 precisely so binary survives.
      const binary = atob(file.base64);
      const bytes = Uint8Array.from(binary, (c) => c.charCodeAt(0));
      const url = URL.createObjectURL(new Blob([bytes]));
      const link = document.createElement("a");
      link.href = url;
      link.download = entry.name;
      link.click();
      URL.revokeObjectURL(url);
    } catch (err) {
      setError(errorMessage(err, "Could not download the file."));
    }
  };

  const openEditor = async (entry: Entry) => {
    setError(null);
    try {
      const file = await api.readFile(store, { path: entry.path });
      if (file.text == null) {
        setError(`"${entry.name}" is not a text file, so it cannot be edited here.`);
        return;
      }
      setEditing({ path: entry.path, text: file.text });
    } catch (err) {
      setError(errorMessage(err, "Could not open the file."));
    }
  };

  const saveEditor = async () => {
    if (!editing) return;
    await act("Could not save the file.", () =>
      api.writeFile(store, { path: editing.path, text: editing.text, base64: null }),
    );
    setEditing(null);
  };

  const openPermissions = async (entry: Entry) => {
    setError(null);
    try {
      setPermissions(await api.getFileMeta(store, { path: entry.path }));
    } catch (err) {
      setError(errorMessage(err, "Could not read the file's permissions."));
    }
  };

  const crumbs = dir ? dir.split("/") : [];

  return (
    <>
      <PageHeader
        pretitle="Storage"
        title={
          <>
            Files <span className="text-muted">— {store}</span>
          </>
        }
        actions={
          <>
            <Button variant="outline-secondary" onClick={() => navigate("/file-stores")}>
              File stores
            </Button>
            <Button variant="outline-primary" onClick={makeFolder}>
              New folder
            </Button>
            <Button onClick={() => uploadInput.current?.click()}>Upload</Button>
            <input
              ref={uploadInput}
              type="file"
              multiple
              className="d-none"
              onChange={(e) => {
                upload(e.target.files);
                e.target.value = "";
              }}
            />
          </>
        }
      />
      <PageBody>
        {error && (
          <Alert variant="danger" onClose={() => setError(null)} dismissible>
            {error}
          </Alert>
        )}

        <Breadcrumb>
          <Breadcrumb.Item active={dir === ""} onClick={() => void load("")}>
            {store}
          </Breadcrumb.Item>
          {crumbs.map((name, index) => {
            const target = crumbs.slice(0, index + 1).join("/");
            return (
              <Breadcrumb.Item
                key={target}
                active={index === crumbs.length - 1}
                onClick={() => void load(target)}
              >
                {name}
              </Breadcrumb.Item>
            );
          })}
        </Breadcrumb>

        {!entries ? (
          <div className="text-center py-5">
            <Spinner animation="border" role="status" />
          </div>
        ) : (
          <div className="card">
          <Table hover responsive className="card-table table-vcenter">
            <thead>
              <tr>
                <th>Name</th>
                <th>Size</th>
                <th className="text-end">Actions</th>
              </tr>
            </thead>
            <tbody>
              {dir !== "" && (
                <tr>
                  <td colSpan={3}>
                    <Button variant="link" className="p-0" onClick={() => void load(parentOf(dir))}>
                      ../
                    </Button>
                  </td>
                </tr>
              )}
              {entries.length === 0 && (
                <tr>
                  <td colSpan={3} className="text-muted">
                    This folder is empty.
                  </td>
                </tr>
              )}
              {entries.map((entry) => (
                <tr key={entry.path}>
                  <td>
                    {entry.is_dir ? (
                      <Button
                        variant="link"
                        className="p-0"
                        onClick={() => void load(entry.path)}
                      >
                        {entry.name}/
                      </Button>
                    ) : (
                      entry.name
                    )}
                  </td>
                  <td className="text-muted small">{formatSize(entry.size)}</td>
                  <td className="text-end">
                    {!entry.is_dir && (
                      <>
                        <Button
                          size="sm"
                          variant="outline-secondary"
                          className="me-2"
                          onClick={() => void openEditor(entry)}
                        >
                          Edit
                        </Button>
                        <Button
                          size="sm"
                          variant="outline-secondary"
                          className="me-2"
                          onClick={() => void download(entry)}
                        >
                          Download
                        </Button>
                      </>
                    )}
                    <Button
                      size="sm"
                      variant="outline-secondary"
                      className="me-2"
                      onClick={() => void openPermissions(entry)}
                    >
                      Permissions
                    </Button>
                    <Button
                      size="sm"
                      variant="outline-secondary"
                      className="me-2"
                      disabled={busy}
                      onClick={() => rename(entry)}
                    >
                      Rename
                    </Button>
                    <Button
                      size="sm"
                      variant="outline-danger"
                      disabled={busy}
                      onClick={() => remove(entry)}
                    >
                      Delete
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </Table>
          </div>
        )}

        <EditorModal
          editing={editing}
          busy={busy}
          onChange={(text) => setEditing((e) => (e ? { ...e, text } : e))}
          onCancel={() => setEditing(null)}
          onSave={() => void saveEditor()}
        />

        <PermissionsModal
          store={store}
          meta={permissions}
          onClose={() => setPermissions(null)}
          onSaved={(saved) => {
            setPermissions(null);
            void load(dir);
            return saved;
          }}
          onError={setError}
        />
      </PageBody>
    </>
  );
}

/** Edit a text file in place — what `readFile`'s UTF-8 shortcut was built for. */
function EditorModal({
  editing,
  busy,
  onChange,
  onCancel,
  onSave,
}: {
  editing: { path: string; text: string } | null;
  busy: boolean;
  onChange: (text: string) => void;
  onCancel: () => void;
  onSave: () => void;
}) {
  return (
    <Modal show={editing !== null} onHide={onCancel} size="lg">
      <Modal.Header closeButton>
        <Modal.Title className="h6">{editing?.path}</Modal.Title>
      </Modal.Header>
      <Modal.Body>
        <Form.Control
          as="textarea"
          rows={20}
          className="font-monospace"
          value={editing?.text ?? ""}
          onChange={(e) => onChange(e.target.value)}
        />
      </Modal.Body>
      <Modal.Footer>
        <Button variant="outline-secondary" onClick={onCancel}>
          Cancel
        </Button>
        <Button onClick={onSave} disabled={busy}>
          {busy ? "Saving…" : "Save"}
        </Button>
      </Modal.Footer>
    </Modal>
  );
}

/** View and set one entry's access rule.
 *
 * Shows the *effective* role beside the one set here, because they differ
 * whenever a parent directory or the store itself is stricter — and an admin who
 * saw only the entry's own rule could easily believe a file is reachable when its
 * folder has locked it. */
function PermissionsModal({
  store,
  meta,
  onClose,
  onSaved,
  onError,
}: {
  store: string;
  meta: GetFileMetaResponse | null;
  onClose: () => void;
  onSaved: (saved: GetFileMetaResponse) => void;
  onError: (message: string) => void;
}) {
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    setValue(meta?.min_role == null ? "" : String(meta.min_role));
  }, [meta]);

  if (!meta) return null;

  const save = async () => {
    setBusy(true);
    try {
      const saved = await api.setFileMeta(store, {
        path: meta.path,
        min_role: value.trim() === "" ? null : Number(value),
        // Attributes are round-tripped untouched: this dialog edits the access
        // rule, and silently dropping the free-form attributes stored beside it
        // would be a destructive side effect of opening it.
        attributes: meta.attributes ?? {},
      });
      onSaved(saved);
    } catch (err) {
      onError(errorMessage(err, "Could not save the permissions."));
    } finally {
      setBusy(false);
    }
  };

  const inherited =
    meta.effective_min_role != null && meta.effective_min_role !== meta.min_role;

  return (
    <Modal show onHide={onClose}>
      <Modal.Header closeButton>
        <Modal.Title className="h6">Permissions — {meta.path}</Modal.Title>
      </Modal.Header>
      <Modal.Body>
        <Form.Group className="mb-3" controlId="metaMinRole">
          <Form.Label>Minimum role</Form.Label>
          <Form.Control
            type="number"
            min={1}
            max={100}
            value={value}
            placeholder="unrestricted"
            onChange={(e) => setValue(e.target.value)}
          />
          <Form.Text muted>
            1 is admin, 100 is public; lower is more restrictive. Leave blank to set no rule
            here.
          </Form.Text>
        </Form.Group>

        <div className="mb-0">
          <span className="me-2">Effective:</span>
          {meta.effective_min_role == null ? (
            <StatusBadge tone="secondary">Unrestricted</StatusBadge>
          ) : (
            <StatusBadge tone="blue">Role {meta.effective_min_role} or lower</StatusBadge>
          )}
          {inherited && (
            <div className="text-muted small mt-2">
              Stricter than the rule set here, because a parent folder or the store itself
              restricts it. Access is cumulative down the path, so a rule here can only ever
              tighten it further.
            </div>
          )}
        </div>
      </Modal.Body>
      <Modal.Footer>
        <Button variant="outline-secondary" onClick={onClose}>
          Cancel
        </Button>
        <Button onClick={() => void save()} disabled={busy}>
          {busy ? "Saving…" : "Save"}
        </Button>
      </Modal.Footer>
    </Modal>
  );
}
