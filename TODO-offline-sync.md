# Saltcorn v2 — Offline sync (milestones S1–S5)

Offline-first applications: a device keeps a local copy of an application's synced tables and
exchanges changes with the server when it can. The design is
[docs/TECHNICAL_DESIGN.md](./docs/TECHNICAL_DESIGN.md) §14.11. Milestones are prefixed **S**;
work through them in order.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

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

- [ ] S3.1 The push endpoint: a device's inserts, updates and deletes applied in the device's
      order through the row layer, with the user's rights and the device's id and change numbers
      set for the trigger. Each change has its own savepoint: a refused one is reported with its
      reason and the others go on.
- [ ] S3.2 Retrying is safe: changes already applied are skipped.
- [ ] S3.3 Field-wise merge, server wins on a field changed on both sides. Delete against change:
      a device's delete of a row changed on the server since the device saw it is refused (the
      device gets the row back); a device's change of a row deleted on the server is refused (the
      device removes it). The answer lists accepted, merged and refused changes.
- [ ] S3.4 Refused for now, and reported: a change of a row's primary key from a device; a row
      pointing at a row that no longer exists (deleted meanwhile); a delete the database's foreign
      keys forbid.
- [ ] S3.5 Tests: each conflict case; a child inserted offline with its parent; a delete made
      offline; a refused write (access, validation, foreign key) reported without stopping the
      others.

## S4 — The client

- [ ] S4.1 The generated client (`src/feldspar/`) reads and writes a local copy with an outbox:
      `expo-sqlite` in React Native, IndexedDB in the browser.
- [ ] S4.2 Background sync: pull and push when online, and a pull when a live `table_changes`
      stream says a synced table changed.
- [ ] S4.3 A pull is applied in one local transaction, foreign-key checks deferred to its end;
      every insert or update entry fetches the current row and overwrites the local one, so a
      local cascade or a key change cannot lose a row.
- [ ] S4.4 A React Native example app that works offline. Tests with the client under Node.

## S5 — Cleanup and documentation

- [ ] S5.1 Removing superseded log entries after a retention period; a device older than that
      reloads its tables.
- [ ] S5.2 A tutorial for an offline-first app; TECHNICAL_DESIGN §14.11 brought up to date.

## Later

- [ ] Resolving instead of refusing a change whose referenced row was deleted meanwhile
      (restore the parent, clear the reference, or keep it in a conflict list for a person).
- [ ] Conflict strategies besides "server wins", chosen per table (`attributes.sync`).
