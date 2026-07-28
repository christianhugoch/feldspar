// File stores list: every store an admin has configured, plus any connected by
// the `--file-store` flag.
//
// Two states this screen exists to make visible, which the MVP could not show at
// all (there was no screen, and stores were command-line only):
//
//   - A store can be **defined but not connected** — its directory unmounted or
//     renamed since it was saved. It stays listed, with the reason, and stays
//     editable, because editing it is the repair.
//   - A store can be **connected but not defined** — supplied by `--file-store`,
//     which is ephemeral by design (§1.3). It has no row, so there is nothing to
//     edit or delete; the API reports a null id and this screen renders it
//     read-only rather than offering actions that would fail.

import { useEffect, useState } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Table from "react-bootstrap/Table";

import { api, errorMessage } from "../api";
import type { ListFileStoresResponse } from "../client";
import { navigate } from "../App";
import { IconPlus } from "../icons";
import { PageBody, PageHeader, StatusBadge } from "../layout";
import { asString } from "../settings";

type StoreItem = ListFileStoresResponse[number];

/** A one-line summary of where a store's data lives, from its backend config.
 * Generic on purpose: the screen knows no backend's settings, so it shows the
 * settings there are rather than reaching for a `path` that only `local` has.
 *
 * Long values are elided rather than special-cased away: a git store's public
 * key is a legitimate setting and belongs in the summary, but at full length it
 * would push everything else off the row. */
function locationSummary(store: StoreItem): string {
  const config = store.config;
  if (!config || typeof config !== "object") return "";
  return Object.entries(config as Record<string, unknown>)
    .map(([key, value]) => `${key}: ${elide(asString(value))}`)
    .join(", ");
}

/** A value shortened to fit a table cell. */
function elide(value: string, max = 60): string {
  return value.length > max ? `${value.slice(0, max)}…` : value;
}

export function FileStores() {
  const [stores, setStores] = useState<StoreItem[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    try {
      setStores(await api.listFileStores());
    } catch {
      setError("Could not load file stores.");
    }
  };

  useEffect(() => {
    void load();
  }, []);

  const remove = async (store: StoreItem) => {
    if (!store.id) return;
    if (
      !window.confirm(
        `Remove the file store "${store.name}"?\n\n` +
          "This disconnects it from Saltcorn. The directory and its files are left untouched.",
      )
    ) {
      return;
    }
    setError(null);
    try {
      await api.deleteFileStore(store.id);
      await load();
    } catch (err) {
      // Typically "still used by application X" — the server names the referent,
      // so surface its message rather than a generic failure.
      setError(errorMessage(err, "Could not remove the file store."));
    }
  };

  return (
    <>
      <PageHeader
        pretitle="Storage"
        title="File stores"
        actions={
          <Button onClick={() => navigate("/file-stores/new")}>
            <IconPlus className="icon-2" />
            New file store
          </Button>
        }
      />
      <PageBody>
        {error && <Alert variant="danger">{error}</Alert>}

        <div className="card">
          <Table hover responsive className="card-table table-vcenter">
            <thead>
              <tr>
                <th>Name</th>
                <th>Backend</th>
                <th>Location</th>
                <th>Status</th>
                <th className="text-end">Actions</th>
              </tr>
            </thead>
            <tbody>
              {stores?.length === 0 && (
                <tr>
                  <td colSpan={5} className="text-muted">
                    No file stores yet.
                  </td>
                </tr>
              )}
              {stores?.map((store) => (
                <tr key={store.id ?? `flag:${store.name}`}>
                  <td>
                    {store.name}
                    {store.description && (
                      <div className="text-muted small">{store.description}</div>
                    )}
                    {store.min_role != null && (
                      <div className="text-muted small">Minimum role {store.min_role}</div>
                    )}
                  </td>
                  <td>{store.backend}</td>
                  <td className="text-break small">{locationSummary(store)}</td>
                  <td>
                    <StatusCell store={store} />
                  </td>
                  <td className="text-end">
                    <div className="btn-list justify-content-end flex-nowrap align-items-center">
                      {store.id ? (
                        <>
                          <Button
                            size="sm"
                            variant="outline-secondary"
                            href={`#/file-stores/${encodeURIComponent(store.id)}/edit`}
                          >
                            Edit
                          </Button>
                          <Button
                            size="sm"
                            variant="outline-primary"
                            disabled={!store.connected}
                            href={`#/files/${encodeURIComponent(store.name)}`}
                          >
                            Browse
                          </Button>
                          <Button
                            size="sm"
                            variant="outline-danger"
                            onClick={() => void remove(store)}
                          >
                            Remove
                          </Button>
                        </>
                      ) : (
                        <>
                          <Button
                            size="sm"
                            variant="outline-primary"
                            href={`#/files/${encodeURIComponent(store.name)}`}
                          >
                            Browse
                          </Button>
                          {/* No row behind it, so nothing to edit or delete.
                              Saying so beats offering buttons that would 404. */}
                          <span className="text-muted small">Not editable</span>
                        </>
                      )}
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </Table>
        </div>

        {stores?.some((s) => !s.id) && (
          <p className="text-muted small mt-3">
            Stores shown as <em>from --file-store</em> were supplied on the command line. They
            are not saved and will be gone when the server restarts unless the flag is passed
            again.
          </p>
        )}
      </PageBody>
    </>
  );
}

/** Connected, or not connected with the reason — the state that makes a broken
 * store fixable instead of merely absent. */
function StatusCell({ store }: { store: StoreItem }) {
  if (!store.connected) {
    return (
      <>
        <StatusBadge tone="red">Not connected</StatusBadge>
        {store.error && (
          <div className="text-danger small text-break mt-1">{store.error}</div>
        )}
      </>
    );
  }
  return (
    <>
      <StatusBadge tone="green">Connected</StatusBadge>
      {store.is_git_repo && (
        <StatusBadge tone="secondary" className="ms-1">
          git
        </StatusBadge>
      )}
      {!store.id && (
        <div className="text-muted small mt-1">
          <em>from --file-store</em>
        </div>
      )}
    </>
  );
}
