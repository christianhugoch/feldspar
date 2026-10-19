# Streaming backups and restores, with backup format v2

**Status: [x] done** (2026-10-19) — see the CHANGELOG entry "Backups and restores stream, in
backup format 2". Measured on the scratch database below (debug builds): backup peak RSS
3.7 GB → 98 MB, 162 s → 26 s; under `MemoryMax=2G` the old binary is OOM-killed and the new one
finishes; the restore into a fresh database peaks at 174 MB and every byte matches.

## Context

One instance's nightly `feldspar backup` is OOM-killed at its 2 GB cgroup limit. Run by hand it peaks at 3.4 GB to produce a 100 MB zip, for 73 MB of database and 177 MB of file stores. The cause, read from `crates/sc-server/src/backup/write.rs`:

- **bytea is written as one JSON number per byte.** `table_rows` → `rows::row_to_json` → `sc_types::value_to_json` turns `Value::Bytes` into an array of `Json::Number` at about 32 B each. The `video_teil` table's 43 MB becomes about 1.4 GB of `serde_json::Value`.
- **Every table is one `Vec<Json>`,** which `to_vec_pretty` then turns into about 500 MB of text: one number per indented line.
- **The zip is built in a `Cursor<Vec<u8>>`.** The CLI writes it out whole, the in-app backup sends it as `Bytes`, and the scheduler passes it as a `Vec` to `Connection::put` (S3 also SHA-256s the whole buffer).
- **Every file-store file is read whole** with `store.read`.
- **The Postgres driver buffers entire result sets** (`sc-db-postgres/src/exec.rs:97`, `client.query`).
- **Restore holds the whole archive in memory.** That is the upload `Bytes` plus every entry expanded into `Entries = BTreeMap<String, Vec<u8>>`. A 2 GB instance may not be able to restore its own backup.

Prototype policy applies: Feldspar's own backup format may change, and old Feldspar backups need not restore. Saltcorn 1 import must keep working; `backup/v1.rs` converts those archives into the current layout. The goals: memory bounded by about one page of rows plus I/O buffers, for backup *and* restore, from the CLI, the in-app dialog and the scheduler (local, SFTP, S3); and a format chosen for reliability.

## 1. The format, v2 (`FORMAT_VERSION = 2`; v1 Feldspar files are refused by the existing version check)

```text
manifest.json                         format, version 2, feldspar_version, created_at, source, contents
tables/<table>/table.json             unchanged (pretty JSON, source-language labels)
tables/<table>/rows.ndjson            NEW: one compact JSON object per line
users/users.json                      { roles, fields }          (was users.json with the rows inside)
users/rows.ndjson                     NEW: the users table's rows, same encoding
file-stores/<store>/store.json        { definition, files: [ { path, size, mode, meta } ] }
file-stores/<store>/files/<path>      bytes; stored uncompressed when the type is already compressed
analytics/draws/<instance>.ndjson     NEW: was .json
everything else                       unchanged: pretty JSON in the admin API's shapes
```

- **NDJSON rows.** Each row is complete on its own line. Both the writer and the reader handle one line at a time, so neither needs the table in memory. A damaged line costs one row, not the table.
- **Values are encoded by type**, in a new `backup/values.rs` used by both the writer and the restore:
  - **Bytes** → standard padded base64 string.
  - **Float** → JSON number (serde_json writes shortest round-trip f64). NaN, Infinity and -Infinity become the strings `"NaN"`, `"Infinity"`, `"-Infinity"`; `value_to_json` silently turns them into `null` today, which is a real accuracy bug.
  - **Timestamp** → RFC 3339 UTC with full sub-second precision.
  - **Unchanged:** Decimal (string, no precision loss), Uuid, Date, Time, Text, Json (as-is), Int (exact i64), Bool, Null.
- **Decoding goes by the restoring table's column type**, not by the backend that wrote the file. That is what lets a SQLite backup restore into Postgres and the reverse.
  - New `values::column_value(table, column, json)`: a `Bytes` column decodes base64, then `validate_with`. Every other type goes through the existing `sc_api::rows::column_value`, whose float parse already accepts `NaN` and `Infinity`.
  - The public row API (`sc_types::json`) is untouched.
- **Integrity:**
  - The manifest already records each table's row count. The restore now counts the lines it reads and reports a mismatch ("the backup says N rows of `t`, it holds M — the file may be damaged").
  - zip's CRC-32 is checked as each entry is read to the end, and a failure is reported against that entry.
- **Entries over 4 GB:** `ZipWriter::set_auto_large_file()` switches to Zip64.
- **i18n:** check that the zip crate sets the UTF-8 name flag for non-ASCII entry names (file-store paths, table names), and set it explicitly if it does not. A test round-trips a non-ASCII file path.
- **Compression:** file-store files whose extension shows an already-compressed type (images, audio and video, zip/gz/xz/zst/7z, woff2, office zip formats) use `CompressionMethod::Stored`. That saves the CPU spent deflating video. Everything else stays deflated.

## 2. Writing (`backup/write.rs`)

- `ZipBuilder<W: Write + Seek>`:
  - `json()` writes `to_writer_pretty` straight into the entry.
  - `start()` / `write_chunk()` handle streamed entries.
  - `rows()` writes one compact line per row.
- `write_backup_to(catalog, selection, std::fs::File) -> Result<u64>` writes through a `BufWriter` and returns the length. The in-memory `write_backup` goes; tests use the file API through a small helper.
- **One consistent snapshot on Postgres.**
  - All row reads go through one transaction: `begin()`, then a new `Transaction::set_snapshot()` (default `Err`). The Postgres version is `SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY`, like pg_dump. Tables are then consistent *with each other*, so foreign keys line up.
  - SQLite reads without a transaction. Its `begin()` is `BEGIN IMMEDIATE`, which would block every writer for the whole backup.
  - A small `RowSource` enum (`Snapshot(Box<dyn Transaction>)` / `Direct(&Catalog)`) hides the difference.
- **Paged row reads** (`write_table_rows`), used for table rows, user rows and draws:
  - With a single primary key: keyset pagination, `ORDER BY pk LIMIT n`, then `WHERE pk > last` (`Expr::binary(BinOp::Gt, …)`).
  - `n` adapts. It starts at 1000; after each page it is rescaled so the next page's payload (byte and text lengths) aims at about 8 MB, clamped to 1..=1000. `video_teil` is read a few rows at a time.
  - With no single pk: one query, still written row by row.
  - Bounds memory despite the buffering Postgres driver.
- **File-store files:** `store.open_read(path, None)` is copied in 64 KB chunks. `walk` returns each entry's size for `store.json` and for the compression choice.
- `fits.json` (instances and outputs) is encoded with the same value encoder, so non-finite floats in outputs survive.
- **Blocking I/O:** the zip writes are synchronous calls into a `BufWriter<File>`, i.e. the page cache, from async code, as the in-memory writes are today. That is acceptable.

## 3. Spool files and the three backup callers

- **Spool directory:** `sc_files::data_dir()/backup-spool`, falling back to `std::env::temp_dir()`. It is on real disk on purpose: Debian 13's `/tmp` is tmpfs, and tmpfs pages count against the cgroup memory limit.
  - New `backup/spool.rs` gives unique names and a drop guard that deletes the file.
  - When the server boots, spool files older than a day are removed.
- **CLI** (`sc-cli/src/main.rs` `backup_command`): writes straight to `.FILE.partial`, then renames it over FILE, removing the partial on error. The summary line reports the size.
- **In-app download** (`createBackup` in `handlers.rs:~4607`, `router.rs:~2436`):
  - `handler::Download` gets `body: DownloadBody { Bytes(Bytes) | File(SpooledFile) }`. `SpooledFile` is a cloneable `Arc<Mutex<Option<std::fs::File>>>` plus its length.
  - The backup is written to a spool file, reopened, and unlinked immediately on Unix. The router streams it with `Content-Length` through `file_response::chunks` (made `pub(crate)`), and dropping the response frees it.
  - Errors before the first byte still come back as a JSON error.
- **Scheduler** (`schedule.rs` `run_schedule`, `destination.rs`):
  - `Connection::put(name, &Path)`.
  - **Local:** `run_schedule` writes straight to `dir/.name.partial`, and `put` renames it into place, with copy + rename as the fallback on `EXDEV`.
  - **SFTP:** spool, then `SftpConnection::upload` copies the file with `tokio::io::copy` in place of `write(&[u8])`.
  - **S3:** spool; stream the file once through `Sha256`; then `PUT` with an explicit `Content-Length` and `reqwest::Body::wrap_stream`. This needs reqwest's `stream` feature in the root `Cargo.toml`, and `S3Client::send` takes a `reqwest::Body`. The 5 GB single-PUT limit stays; multipart is a later change.

## 4. Streaming restore (`backup/restore.rs`, a new `backup/archive.rs`)

- **`Archive` replaces `Entries` in restore:**
  - `Zip(ZipSource)` reads from the file. `ZipSource` wraps `zip::ZipArchive<SharedFile>`; `SharedFile` is an `Arc<File>` read with `read_at`/`seek_read` at its own position, so it is `Clone` and every reader gets a cheap clone sharing the parsed central directory.
  - `Memory(Entries)` holds what `v1::convert` produces.
- **Methods:**
  - `contains`, `names_under(prefix)`, and `json(path)` (replaces `json_entry`: a synchronous whole-entry read, fine for metadata documents).
  - `rows(path)`: a `spawn_blocking` thread reads the entry line by line and sends batches (at most 250 rows or about 4 MB) over a `tokio::mpsc::channel(2)` with `blocking_send`. The consumer awaits `next_batch()`. Errors, a bad line (with its line number) and a CRC failure come back through the channel.
  - `copy_into(path, store, store_path)`: 64 KB chunks over a channel, written with `store.write(path, empty)` and then `store.open_append`. The local store's `open_append` streams; other backends' default buffers, as it does today.
- **The `restore_*` functions** change mechanically from `&Entries` to `&Archive`. The users, table-row and draws paths consume `rows()` batches; file stores use `names_under` + `copy_into`.
- **Row inserts are batched:** one multi-row INSERT per 250-row batch (`insert_rows_batched`). When a batch fails, it is retried row by row, so one bad row is reported and skipped as today instead of sinking the batch. `repoint_users` is applied per batch.
- **Signatures:** `inspect(&Path)` and `restore_backup(…, &Path, …)`.
  - The CLI passes FILE directly instead of `std::fs::read`.
  - `read_backup` opens the zip. If it has no `manifest.json` and the entry *names* look like a Saltcorn 1 backup, it expands the entries and runs `v1::convert` (the v1 path stays in memory, as before), giving `Archive::Memory`.
- **Upload** (`router.rs` `upload_backup`, `PendingUploads` in `backup/mod.rs`):
  - The request body is streamed into a spool file, still capped by `--max-upload-bytes`.
  - `HandlerCtx` gets `raw_file: Option<PathBuf>` (9 struct literals to update).
  - `PendingUploads` holds spool paths and deletes them on take, expiry or eviction.
- **`v1.rs` writes the v2 layout:** `tables/<t>/rows.ndjson`, `users/users.json` + `users/rows.ndjson`, with values through `values.rs`. Its unit tests move to the new paths.

## 5. Docs and bookkeeping

- Rewrite the layout block and the “does not stream” paragraphs in `backup/mod.rs`, `write.rs` and `restore.rs`. Update §16 of `docs/TECHNICAL_DESIGN.md` where it describes the format.
- `CHANGELOG`: the cause, streaming on both sides, format v2 (NDJSON, typed values, row-count check, Stored compression, the Postgres snapshot), the spool directory, and that older Feldspar backups are refused.
- No `TABLES_RENAME.sql` change: there is no schema change.

## Tests

- `values.rs` unit tests:
  - Every `Value` variant round-trips through encode/decode for its column type, including NaN/±Inf, extreme i64, a decimal with 28 digits, nanosecond timestamps, and empty and binary bytes.
  - Bad base64 is refused.
- `write.rs` / `archive.rs` unit tests:
  - An NDJSON entry written and read back through `Archive::rows`, with batching at the size limit.
  - A truncated line gives a reported error.
  - A row-count mismatch is reported.
  - A non-ASCII path round-trips.
  - Stored vs deflated is chosen by extension.
- `sc-server/tests/backup_api.rs`, new `large_binary_rows_and_files_stream_through_a_backup_and_restore`:
  - A `Bytes` table with a few rows of several MB each, so the adaptive pager takes several pages.
  - A table of more than 1000 small rows, so keyset paging runs past the first page.
  - A file store holding a multi-MB `.mp4` and a non-ASCII-named text file.
  - Back up through `/backup/create`, upload through `/backup/upload`, and restore. Then check exact bytes, row counts, that `rows.ndjson` holds base64, and that the mp4 entry is Stored.
- Update the existing helpers and assertions that read `rows.json`/`users.json`, in `backup_api.rs`, `code_libraries.rs` and the CLI test.
- These must pass:
  - the whole of `backup_api.rs` (including the Saltcorn 1 import tests);
  - `backup_remote.rs`, where the fake S3 checks the SigV4 payload hash against the streamed body and the fake SFTP checks the file it receives;
  - `crates/sc-cli/tests/backup.rs`, which gets a bytea column in its fixture.
- Run `cargo test -p sc-server --lib backup`, `--test backup_api --test backup_remote`, `-p sc-cli --test backup`, and clippy. Follow the memory notes on build flags, disk space and oomd.

## Verification of the actual fix

On a scratch Postgres database, set up 41 rows totalling about 43 MB of bytea plus a file store of about 180 MB. Then:

1. Run `/usr/bin/time -v feldspar backup` before and after the change, and compare maximum RSS. The expected result is gigabytes before and tens of MB after.
2. Run it under `systemd-run --user --scope -p MemoryMax=2G` to reproduce the customer's limit.
3. Run `feldspar restore` into a fresh database under the same limit, and check that the bytes match.
