# Saltcorn v2 — Offline sync (milestones S1–S5)

Offline-first applications: a device keeps a local copy of an application's synced tables and
exchanges changes with the server when it can. The design is
[docs/TECHNICAL_DESIGN.md](./docs/TECHNICAL_DESIGN.md) §14.11. Milestones are prefixed **S**;
work through them in order.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

Decided (2026-10-12): offline inserts need a `uuid` key, integer keys are left possible for
later; conflicts are merged field by field with the server winning a field changed on both
sides; a device gets everything the user may read; changes are recorded by database triggers.

## S1 — Recording changes

- [x] S1.1 `_fd_changes` and the shared trigger function, created at bootstrap (Postgres).
- [x] S1.2 Switching sync on and off for a table (`_fd_tables.attributes.sync`): creates or drops
      its trigger; refused for a table without a single-column primary key. Admin UI and MCP.
- [x] S1.3 Tests: insert, update with its changed fields, an update that changes nothing, a
      change of key, delete, and a write made with plain SQL.
- [ ] S1.4 SQLite: a trigger per table, regenerated whenever the table's columns change.

## S2 — Pull

- [ ] S2.1 `_fd_sync_clients` and registering a device for a user and an application.
- [ ] S2.2 The pull endpoint: entries between the device's watermark and the current one, with
      the current rows read as the user; a row the user can no longer read is sent as removed.
- [ ] S2.3 Tests: a transaction that commits late is not skipped; access rules and ownership
      apply; a first pull returns every readable row.

## S3 — Push and conflicts

- [ ] S3.1 The push endpoint: a device's changes applied in order through the row layer in one
      transaction, with the device's id and change numbers set for the trigger.
- [ ] S3.2 Retrying is safe: changes already applied are skipped.
- [ ] S3.3 Field-wise merge, server wins on a field changed on both sides; an update of a
      deleted row loses. The answer lists accepted and merged changes.
- [ ] S3.4 Tests: each conflict case; a child inserted offline with its parent; a refused write
      (access, validation) reported without stopping the others.

## S4 — The client

- [ ] S4.1 The generated client (`src/feldspar/`) reads and writes a local copy with an outbox:
      `expo-sqlite` in React Native, IndexedDB in the browser.
- [ ] S4.2 Background sync: pull and push when online, and a pull when a live `table_changes`
      stream says a synced table changed.
- [ ] S4.3 A React Native example app that works offline. Tests with the client under Node.

## S5 — Cleanup and documentation

- [ ] S5.1 Removing superseded log entries after a retention period; a device older than that
      reloads its tables.
- [ ] S5.2 A tutorial for an offline-first app; TECHNICAL_DESIGN §14.11 brought up to date.
