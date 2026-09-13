// The Settings screen's Backup tab: take a backup, or restore one.
//
// Two buttons and one dialog, twice. The dialog is the same component both times
// (`IncludeDialog`) because the two questions are the same question asked of two
// sources: "of everything here, what should this cover?" — where *here* is this
// installation when backing up, and an uploaded file when restoring. The server
// describes both in one shape, so this screen never asks which it is looking at.
//
// The arithmetic of the selection — that rows cannot be included without their
// table, what "everything" means, what the summary line says — is in `backup.ts`,
// where it is tested without a browser. What is left here is the flow: the
// buttons, the busy states, and what the admin is told afterwards.

import { useCallback, useEffect, useRef, useState } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Form from "react-bootstrap/Form";
import Modal from "react-bootstrap/Modal";
import Spinner from "react-bootstrap/Spinner";

import { api, createBackup, errorMessage, uploadBackup, type UploadedBackup } from "../api";
import {
  NO_CONTENTS,
  choices,
  dataChoices,
  isEmpty,
  summarise,
  withTableData,
  withTables,
  type BackupContents,
  type BackupSelection,
} from "../backup";
import { IconDownload, IconUpload } from "../icons";
import { AlertBody } from "../layout";
import { MultiSelect } from "../multiSelect";
import type { RestoreBackupResponse } from "../client";

/** What the restore flow is doing: nothing, holding an uploaded file's contents,
 * or showing what a finished restore did. */
type Restore =
  | { stage: "idle" }
  | { stage: "choosing"; uploaded: UploadedBackup; selection: BackupSelection }
  | { stage: "done"; report: RestoreBackupResponse };

export function BackupTab() {
  const [contents, setContents] = useState<BackupContents>(NO_CONTENTS);
  const [selection, setSelection] = useState<BackupSelection | null>(null);
  const [choosing, setChoosing] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [taken, setTaken] = useState(false);
  const [restore, setRestore] = useState<Restore>({ stage: "idle" });
  const fileInput = useRef<HTMLInputElement>(null);

  /** What this server has, and the selection the admin last backed up with. */
  const load = useCallback(async () => {
    try {
      const options = await api.getBackupOptions();
      setContents(options.available);
      setSelection(options.include);
    } catch (e) {
      setError(errorMessage(e, "Could not read what there is to back up."));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const takeBackup = async (include: BackupSelection) => {
    setBusy("Building the backup…");
    setError(null);
    setTaken(false);
    try {
      await createBackup(include);
      setChoosing(false);
      setTaken(true);
      // The server remembers the selection as part of taking the backup, so the
      // options are read back: the next dialog opens on what was just used.
      await load();
    } catch (e) {
      setError(errorMessage(e, "Could not build the backup."));
    } finally {
      setBusy(null);
    }
  };

  const readFile = async (file: File | undefined) => {
    if (!file) return;
    setBusy("Reading the backup…");
    setError(null);
    setRestore({ stage: "idle" });
    try {
      const uploaded = await uploadBackup(file);
      setRestore({ stage: "choosing", uploaded, selection: uploaded.include });
    } catch (e) {
      setError(errorMessage(e, "That file could not be read as a backup."));
    } finally {
      setBusy(null);
    }
  };

  const runRestore = async (uploaded: UploadedBackup, include: BackupSelection) => {
    setBusy("Restoring…");
    setError(null);
    try {
      const report = await api.restoreBackup({ id: uploaded.id, include });
      setRestore({ stage: "done", report });
      // A restore can add tables, stores and applications: what there is to back
      // up has changed.
      await load();
    } catch (e) {
      setError(errorMessage(e, "The restore failed."));
      setRestore({ stage: "idle" });
    } finally {
      setBusy(null);
    }
  };

  return (
    <>
      {error && (
        <Alert variant="danger" dismissible onClose={() => setError(null)}>
          <AlertBody>{error}</AlertBody>
        </Alert>
      )}
      {taken && (
        <Alert variant="success" dismissible onClose={() => setTaken(false)}>
          <AlertBody>
            The backup has been downloaded. What it includes has been saved, so the next
            one covers the same things.
          </AlertBody>
        </Alert>
      )}

      <div className="card mb-4">
        <div className="card-header">
          <div>
            <h3 className="card-title">Backup</h3>
            <p className="card-subtitle text-secondary mb-0">
              One zip file holding this installation: table definitions and their rows,
              applications, file stores and their contents, users, agents, triggers and the
              SSL settings. Choose what goes in when you take it.
            </p>
          </div>
        </div>
        <div className="card-body">
          {selection === null ? (
            <Spinner animation="border" role="status" size="sm" />
          ) : (
            <>
              <p className="text-secondary mb-3">
                Currently included: {summarise(selection, contents)}
              </p>
              <div className="btn-list">
                <Button onClick={() => setChoosing(true)} disabled={busy !== null}>
                  <IconDownload /> Backup now
                </Button>
              </div>
              {/* Said where the choice is made, not in a footnote: a backup carries
                  password hashes, a file store's credentials and the TLS private
                  key, so the file is exactly as sensitive as the database. */}
              <p className="form-hint mt-3 mb-0 text-secondary">
                A backup contains everything needed to restore this installation, including
                password hashes, file-store credentials and the SSL private key. Keep it
                somewhere you would keep a database dump.
              </p>
            </>
          )}
        </div>
      </div>

      <div className="card">
        <div className="card-header">
          <div>
            <h3 className="card-title">Restore</h3>
            <p className="card-subtitle text-secondary mb-0">
              Read a backup file and put back the parts of it you choose. A Saltcorn 1
              backup works too: its tables, rows, users, files and actions are imported,
              and the restore says what it could not bring across. Nothing already
              on this server is deleted or overwritten: tables, users and file stores that
              are already here are left as they are, and the restore says what it skipped.
              Restored applications are built and start serving straight away, so a restore
              that includes one takes as long as its build does.
            </p>
          </div>
        </div>
        <div className="card-body">
          <div className="btn-list">
            <Button
              variant="outline-primary"
              disabled={busy !== null}
              onClick={() => fileInput.current?.click()}
            >
              <IconUpload /> Restore
            </Button>
          </div>
          <input
            ref={fileInput}
            type="file"
            accept=".zip,application/zip"
            className="d-none"
            onChange={(e) => {
              void readFile(e.target.files?.[0]);
              // Cleared so choosing the same file twice fires a change both times.
              e.target.value = "";
            }}
          />

          {restore.stage === "done" && (
            <div className="mt-3">
              <RestoreReport report={restore.report} />
            </div>
          )}
        </div>
      </div>

      {busy !== null && (
        <div className="mt-3 text-secondary" role="status">
          <Spinner animation="border" size="sm" className="me-2" />
          {busy}
        </div>
      )}

      {selection !== null && (
        <IncludeDialog
          show={choosing}
          title="What should the backup include?"
          confirm="Backup now"
          busy={busy}
          contents={contents}
          selection={selection}
          onChange={setSelection}
          onCancel={() => setChoosing(false)}
          onConfirm={() => void takeBackup(selection)}
        />
      )}

      {restore.stage === "choosing" && (
        <IncludeDialog
          show
          title="What should be restored?"
          subtitle={restoreSubtitle(restore.uploaded)}
          confirm="Restore"
          busy={busy}
          contents={restore.uploaded.available}
          selection={restore.selection}
          onChange={(selection) => setRestore({ ...restore, selection })}
          onCancel={() => setRestore({ stage: "idle" })}
          onConfirm={() => void runRestore(restore.uploaded, restore.selection)}
        />
      )}
    </>
  );
}

/** What the restore dialog says above the tick boxes: when the backup was taken
 * and what wrote it.
 *
 * The source is worth a line of its own because one answer to it is "Saltcorn
 * 1.7.0, imported" — a file this server translated, which carries tables, rows,
 * files, users and actions and leaves v1's views and pages behind. An admin
 * should see that before they press Restore, not afterwards in the report. */
function restoreSubtitle(uploaded: UploadedBackup): string | undefined {
  const parts: string[] = [];
  if (uploaded.created_at)
    parts.push(`Backup taken ${new Date(uploaded.created_at).toLocaleString()}.`);
  if (uploaded.source) parts.push(`From ${uploaded.source}.`);
  return parts.length > 0 ? parts.join(" ") : undefined;
}

/** The tick boxes and pickers, over whatever is on offer.
 *
 * Every row is conditional on the offering holding something of that kind, which
 * is what lets one dialog serve both flows: a backup with no applications in it
 * simply has no applications row to untick. */
function IncludeDialog({
  show,
  title,
  subtitle,
  confirm,
  busy,
  contents,
  selection,
  onChange,
  onCancel,
  onConfirm,
}: {
  show: boolean;
  title: string;
  subtitle?: string;
  confirm: string;
  busy: string | null;
  contents: BackupContents;
  selection: BackupSelection;
  onChange: (selection: BackupSelection) => void;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const nothing = isEmpty(selection);
  return (
    <Modal show={show} onHide={onCancel} size="lg" scrollable>
      <Modal.Header closeButton>
        <Modal.Title className="h4">{title}</Modal.Title>
      </Modal.Header>
      <Modal.Body>
        {subtitle && <p className="text-secondary">{subtitle}</p>}

        {contents.tables.length > 0 && (
          <>
            <Form.Group className="mb-3" controlId="backup-tables">
              <Form.Label>Table definitions</Form.Label>
              <MultiSelect
                id="backup-tables"
                options={choices(contents.tables, "row")}
                selected={selection.tables}
                onChange={(tables) => onChange(withTables(selection, tables))}
                placeholder="No tables"
              />
              <Form.Text muted>
                A table's columns, its access rules and its ownership formula.
              </Form.Text>
            </Form.Group>

            <Form.Group className="mb-3" controlId="backup-table-data">
              <Form.Label>Table data</Form.Label>
              <MultiSelect
                id="backup-table-data"
                options={dataChoices(contents, selection)}
                selected={selection.table_data}
                onChange={(data) => onChange(withTableData(selection, data))}
                placeholder="No rows"
                emptyText="Choose a table above first."
              />
              {/* The rule, where it applies: the picker above is the list this one
                  offers, so unticking a table takes its rows with it. */}
              <Form.Text muted>
                The rows themselves. Only a table whose definition is included can have its
                rows included.
              </Form.Text>
            </Form.Group>
          </>
        )}

        {contents.applications.length > 0 && (
          <Form.Group className="mb-3" controlId="backup-applications">
            <Form.Label>Applications</Form.Label>
            <MultiSelect
              id="backup-applications"
              options={choices(contents.applications, "")}
              selected={selection.applications}
              onChange={(applications) => onChange({ ...selection, applications })}
              placeholder="No applications"
            />
            <Form.Text muted>
              An application's definition — its framework, its API and the tables it
              exposes. The built bundle is not in the backup: a restored application is
              rebuilt from the source its file store carries, which is what makes it serve
              again without anybody pressing Build.
            </Form.Text>
          </Form.Group>
        )}

        {contents.file_stores.length > 0 && (
          <Form.Group className="mb-3" controlId="backup-file-stores">
            <Form.Label>Files</Form.Label>
            <MultiSelect
              id="backup-file-stores"
              options={choices(contents.file_stores, "file")}
              selected={selection.file_stores}
              onChange={(file_stores) => onChange({ ...selection, file_stores })}
              placeholder="No file stores"
            />
            <Form.Text muted>
              Each store's definition, every file in it, and each file's access rules.
            </Form.Text>
          </Form.Group>
        )}

        <div className="mb-2">
          {contents.users > 0 && (
            <Form.Check
              type="checkbox"
              id="backup-users"
              className="mb-2"
              checked={selection.users}
              onChange={(e) => onChange({ ...selection, users: e.target.checked })}
              label={
                <>
                  <span className="fw-semibold">Users and roles</span>
                  <div className="text-muted small">
                    {contents.users} {contents.users === 1 ? "account" : "accounts"}, with
                    their password hashes and the roles they hold.
                  </div>
                </>
              }
            />
          )}
          {contents.agents > 0 && (
            <Form.Check
              type="checkbox"
              id="backup-agents"
              className="mb-2"
              checked={selection.agents}
              onChange={(e) => onChange({ ...selection, agents: e.target.checked })}
              label={
                <>
                  <span className="fw-semibold">Agents</span>
                  <div className="text-muted small">
                    {contents.agents} {contents.agents === 1 ? "agent" : "agents"}, with
                    their prompts and enabled traits. Their runs are not included.
                  </div>
                </>
              }
            />
          )}
          {contents.triggers > 0 && (
            <Form.Check
              type="checkbox"
              id="backup-triggers"
              className="mb-2"
              checked={selection.triggers}
              onChange={(e) => onChange({ ...selection, triggers: e.target.checked })}
              label={
                <>
                  <span className="fw-semibold">Triggers</span>
                  <div className="text-muted small">
                    {contents.triggers} {contents.triggers === 1 ? "trigger" : "triggers"}. A
                    trigger that fires on a table whose definition is not included is left
                    out with it.
                  </div>
                </>
              }
            />
          )}
          {contents.views > 0 && (
            <Form.Check
              type="checkbox"
              id="backup-views"
              className="mb-2"
              checked={selection.views}
              disabled={selection.applications.length === 0}
              onChange={(e) => onChange({ ...selection, views: e.target.checked })}
              label={
                <>
                  <span className="fw-semibold">Views</span>
                  <div className="text-muted small">
                    {contents.views} Saltcorn UI {contents.views === 1 ? "view" : "views"}, in
                    the applications chosen above. Restored, they replace the views the
                    application has.
                  </div>
                </>
              }
            />
          )}
          {contents.pages > 0 && (
            <Form.Check
              type="checkbox"
              id="backup-pages"
              className="mb-2"
              checked={selection.pages}
              disabled={selection.applications.length === 0}
              onChange={(e) => onChange({ ...selection, pages: e.target.checked })}
              label={
                <>
                  <span className="fw-semibold">Pages</span>
                  <div className="text-muted small">
                    {contents.pages} Saltcorn UI {contents.pages === 1 ? "page" : "pages"}, in
                    the applications chosen above. Restored, they replace the pages the
                    application has.
                  </div>
                </>
              }
            />
          )}
          {contents.ssl && (
            <Form.Check
              type="checkbox"
              id="backup-ssl"
              className="mb-2"
              checked={selection.ssl}
              onChange={(e) => onChange({ ...selection, ssl: e.target.checked })}
              label={
                <>
                  <span className="fw-semibold">SSL settings</span>
                  <div className="text-muted small">
                    The certificate source and, in <code>custom</code> mode, the certificate
                    and its private key.
                  </div>
                </>
              }
            />
          )}
        </div>
      </Modal.Body>
      <Modal.Footer>
        <Button variant="secondary" type="button" onClick={onCancel}>
          Cancel
        </Button>
        <Button type="button" disabled={busy !== null || nothing} onClick={onConfirm}>
          {busy !== null ? "Working…" : confirm}
        </Button>
      </Modal.Footer>
    </Modal>
  );
}

/** What a finished restore did and did not do.
 *
 * Both lists, always: a restore that skipped an account already on the server has
 * not failed, and reporting only the successes would hide the one thing the admin
 * needs to know. */
function RestoreReport({ report }: { report: RestoreBackupResponse }) {
  return (
    <>
      <Alert variant={report.warnings.length > 0 ? "warning" : "success"}>
        <AlertBody>
          <strong>
            {report.restored.length === 0
              ? "Nothing was restored."
              : `Restored ${report.restored.length} ${
                  report.restored.length === 1 ? "thing" : "things"
                }.`}
          </strong>
          {report.warnings.length > 0 && (
            <>
              <div className="mt-2">Skipped:</div>
              <ul className="mb-0">
                {report.warnings.map((warning, i) => (
                  <li key={i}>{warning}</li>
                ))}
              </ul>
            </>
          )}
        </AlertBody>
      </Alert>
      {report.restored.length > 0 && (
        <details>
          <summary className="text-secondary">What was restored</summary>
          <ul className="mt-2 text-secondary">
            {report.restored.map((line, i) => (
              <li key={i}>{line}</li>
            ))}
          </ul>
        </details>
      )}
    </>
  );
}
