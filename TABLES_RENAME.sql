-- ---------------------------------------------------------------------------
-- Feldspar: rename the metadata tables from the `_sc_` prefix to `_fd_`.
-- ---------------------------------------------------------------------------
--
-- WHY
--   Saltcorn v1 keeps its own metadata in `_sc_*` tables. A transition project
--   runs v1 and Feldspar against the same schema, so the two servers cannot
--   both own that prefix: Feldspar bootstrapping `_sc_config` would write into
--   v1's row store, and `Table::is_system` would hide v1's tables from the very
--   admin who came to look at them. Feldspar now reserves `_fd_` instead, and
--   treats `_sc_*` as ordinary tables it can be pointed at like any other.
--
-- WHAT TO RUN
--   Postgres primary  -> section 1 (and 2, if the database also serves an
--                        application whose tables live here).
--   SQLite primary    -> section 3.
--   SQLite non-primary databases and SQLite file stores -> section 4, in each
--                        such file (they carry `_sc_object_comments`).
--
--   Take a backup first. Run each section as one transaction, with the server
--   stopped: Feldspar caches the catalog in memory and will not notice a table
--   changing name underneath it.
--
--   Run it against a Feldspar-only database, BEFORE anything of v1's is put in
--   the same schema. The rename cannot tell whose `_sc_config` it is looking at,
--   so on a schema that already holds v1's tables it would rename those instead.
--   Re-running it on an already-migrated database is a no-op.
--
-- SCOPE
--   Renames the tables, and the indexes / constraints / sequences / triggers
--   that carry the old prefix in their own names (Postgres does *not* rename
--   those when a table is renamed, and `_sc_config_pkey` would collide with
--   v1's index of that name in a shared schema). Nothing in the stored *data*
--   needs rewriting: `_fd_tables` and `_fd_fields` refuse rows for system
--   tables, so no row anywhere names one.
--
-- WHAT THIS DOES NOT COVER — read this before sharing a schema with v1
--   * `users` is not prefixed in either system, and Feldspar's `users` table is
--     NOT the same shape as v1's. Two servers cannot share one schema until one
--     of them is moved: put v1 and Feldspar in separate Postgres *schemas* (the
--     `search_path` / `schema` setting on each connection), or in separate
--     databases. This rename removes the `_sc_*` collision; it does not remove
--     that one.
--   * Application tables (`books`, `orders`, …) are unprefixed and collide by
--     name, with the constraints and indexes Feldspar derives for them
--     (`sc_uq_*`, `sc_ix_*`, `sc_fts_*`, `sc_ck_*`) collide alongside. Same
--     answer: separate schemas.
--   * Feldspar keeps its own name for a few things on purpose, and they are not
--     touched here: `@saltcorn/…` (the npm scope v1's modules are published
--     under), `globalThis.saltcorn` (the API a v1 module is handed at run time),
--     and `saltcorn_constraint` (the key Feldspar's constraint metadata sits
--     under inside an object's comment — comments are per-object, so it cannot
--     collide).
--
-- ---------------------------------------------------------------------------
-- 1. Postgres: the primary database's metadata tables.
-- ---------------------------------------------------------------------------

BEGIN;

ALTER TABLE IF EXISTS "_sc_tables"            RENAME TO "_fd_tables";
ALTER TABLE IF EXISTS "_sc_fields"            RENAME TO "_fd_fields";
ALTER TABLE IF EXISTS "_sc_triggers"          RENAME TO "_fd_triggers";
ALTER TABLE IF EXISTS "_sc_workflow_versions" RENAME TO "_fd_workflow_versions";
ALTER TABLE IF EXISTS "_sc_agents"            RENAME TO "_fd_agents";
ALTER TABLE IF EXISTS "_sc_llm_providers"     RENAME TO "_fd_llm_providers";
ALTER TABLE IF EXISTS "_sc_runs"              RENAME TO "_fd_runs";
ALTER TABLE IF EXISTS "_sc_run_traces"        RENAME TO "_fd_run_traces";
ALTER TABLE IF EXISTS "_sc_config"            RENAME TO "_fd_config";
ALTER TABLE IF EXISTS "_sc_acme_cache"        RENAME TO "_fd_acme_cache";
ALTER TABLE IF EXISTS "_sc_applications"      RENAME TO "_fd_applications";
ALTER TABLE IF EXISTS "_sc_models"            RENAME TO "_fd_models";
ALTER TABLE IF EXISTS "_sc_model_instances"   RENAME TO "_fd_model_instances";
ALTER TABLE IF EXISTS "_sc_modules"           RENAME TO "_fd_modules";
ALTER TABLE IF EXISTS "_sc_roles"             RENAME TO "_fd_roles";
ALTER TABLE IF EXISTS "_sc_sessions"          RENAME TO "_fd_sessions";
ALTER TABLE IF EXISTS "_sc_api_tokens"        RENAME TO "_fd_api_tokens";
ALTER TABLE IF EXISTS "_sc_db_connections"    RENAME TO "_fd_db_connections";
ALTER TABLE IF EXISTS "_sc_file_stores"       RENAME TO "_fd_file_stores";
-- Present only in a SQLite database (Postgres has native COMMENT ON), listed
-- here so the set is complete; it is a no-op on Postgres.
ALTER TABLE IF EXISTS "_sc_object_comments"   RENAME TO "_fd_object_comments";

-- The objects hanging off those tables. Postgres leaves their names alone when
-- a table is renamed, so `_fd_config` would still be indexed by
-- `_sc_config_pkey` — the name v1's own `_sc_config` wants. Each rename
-- rewrites the *first* `_sc_` in the name, which is also what turns the derived
-- constraint name `sc_uq__sc_workflow_versions_workflow_version` into
-- `sc_uq__fd_workflow_versions_workflow_version`, the name Feldspar derives
-- for it now.
DO $rename$
DECLARE
    r record;
BEGIN
    -- Table constraints (primary keys, unique keys, checks, foreign keys).
    FOR r IN
        SELECT t.relname AS tbl, c.conname AS old
          FROM pg_constraint c
          JOIN pg_class t ON t.oid = c.conrelid
          JOIN pg_namespace n ON n.oid = t.relnamespace
         WHERE n.nspname = current_schema()
           AND t.relname LIKE '\_fd\_%'
           AND c.conname LIKE '%\_sc\_%'
    LOOP
        EXECUTE format(
            'ALTER TABLE %I RENAME CONSTRAINT %I TO %I',
            r.tbl, r.old, regexp_replace(r.old, '_sc_', '_fd_')
        );
    END LOOP;

    -- Indexes that are not backed by a constraint (a constraint's index was
    -- renamed with the constraint above).
    FOR r IN
        SELECT i.relname AS old
          FROM pg_index x
          JOIN pg_class i ON i.oid = x.indexrelid
          JOIN pg_class t ON t.oid = x.indrelid
          JOIN pg_namespace n ON n.oid = i.relnamespace
         WHERE n.nspname = current_schema()
           AND t.relname LIKE '\_fd\_%'
           AND i.relname LIKE '%\_sc\_%'
           AND NOT EXISTS (
               SELECT 1 FROM pg_constraint c WHERE c.conindid = i.oid
           )
    LOOP
        EXECUTE format(
            'ALTER INDEX %I RENAME TO %I',
            r.old, regexp_replace(r.old, '_sc_', '_fd_')
        );
    END LOOP;

    -- Sequences owned by a column of one of those tables (serial / identity).
    FOR r IN
        SELECT s.relname AS old
          FROM pg_class s
          JOIN pg_depend d ON d.objid = s.oid AND d.classid = 'pg_class'::regclass
          JOIN pg_class t ON t.oid = d.refobjid
          JOIN pg_namespace n ON n.oid = s.relnamespace
         WHERE s.relkind = 'S'
           AND n.nspname = current_schema()
           AND t.relname LIKE '\_fd\_%'
           AND s.relname LIKE '%\_sc\_%'
    LOOP
        EXECUTE format(
            'ALTER SEQUENCE %I RENAME TO %I',
            r.old, regexp_replace(r.old, '_sc_', '_fd_')
        );
    END LOOP;

    -- Triggers on those tables.
    FOR r IN
        SELECT t.relname AS tbl, g.tgname AS old
          FROM pg_trigger g
          JOIN pg_class t ON t.oid = g.tgrelid
          JOIN pg_namespace n ON n.oid = t.relnamespace
         WHERE NOT g.tgisinternal
           AND n.nspname = current_schema()
           AND t.relname LIKE '\_fd\_%'
           AND g.tgname LIKE '%\_sc\_%'
    LOOP
        EXECUTE format(
            'ALTER TRIGGER %I ON %I RENAME TO %I',
            r.old, r.tbl, regexp_replace(r.old, '_sc_', '_fd_')
        );
    END LOOP;
END
$rename$;

COMMIT;

-- Check: this should return no rows.
--
--   SELECT c.relname, c.relkind
--     FROM pg_class c
--     JOIN pg_namespace n ON n.oid = c.relnamespace
--    WHERE n.nspname = current_schema()
--      AND c.relname LIKE '\_sc\_%';

-- ---------------------------------------------------------------------------
-- 2. Postgres: a non-primary application database.
-- ---------------------------------------------------------------------------
--
-- Nothing to do. Only the primary carries `_fd_*` tables; a connected
-- application database holds application tables and nothing of Feldspar's.

-- ---------------------------------------------------------------------------
-- 3. SQLite primary.
-- ---------------------------------------------------------------------------
--
-- SQLite has no `ALTER TABLE IF EXISTS`, so run only the lines for tables the
-- file actually has (`.tables` in the sqlite3 shell lists them). SQLite renames
-- a table's indexes with it, so there is no second pass.
--
--   BEGIN;
--   ALTER TABLE "_sc_tables"            RENAME TO "_fd_tables";
--   ALTER TABLE "_sc_fields"            RENAME TO "_fd_fields";
--   ALTER TABLE "_sc_triggers"          RENAME TO "_fd_triggers";
--   ALTER TABLE "_sc_workflow_versions" RENAME TO "_fd_workflow_versions";
--   ALTER TABLE "_sc_agents"            RENAME TO "_fd_agents";
--   ALTER TABLE "_sc_llm_providers"     RENAME TO "_fd_llm_providers";
--   ALTER TABLE "_sc_runs"              RENAME TO "_fd_runs";
--   ALTER TABLE "_sc_run_traces"        RENAME TO "_fd_run_traces";
--   ALTER TABLE "_sc_config"            RENAME TO "_fd_config";
--   ALTER TABLE "_sc_acme_cache"        RENAME TO "_fd_acme_cache";
--   ALTER TABLE "_sc_applications"      RENAME TO "_fd_applications";
--   ALTER TABLE "_sc_models"            RENAME TO "_fd_models";
--   ALTER TABLE "_sc_model_instances"   RENAME TO "_fd_model_instances";
--   ALTER TABLE "_sc_modules"           RENAME TO "_fd_modules";
--   ALTER TABLE "_sc_roles"             RENAME TO "_fd_roles";
--   ALTER TABLE "_sc_sessions"          RENAME TO "_fd_sessions";
--   ALTER TABLE "_sc_api_tokens"        RENAME TO "_fd_api_tokens";
--   ALTER TABLE "_sc_db_connections"    RENAME TO "_fd_db_connections";
--   ALTER TABLE "_sc_file_stores"       RENAME TO "_fd_file_stores";
--   ALTER TABLE "_sc_object_comments"   RENAME TO "_fd_object_comments";
--   COMMIT;

-- ---------------------------------------------------------------------------
-- 4. Every other SQLite file Feldspar has ever written a comment into.
-- ---------------------------------------------------------------------------
--
-- SQLite has no `COMMENT ON`, so the SQLite driver keeps object comments — a
-- unique constraint's error message, a row constraint's formula, and the
-- `saltcorn_constraint` metadata that says a constraint is Feldspar's — in a
-- side table of that file's own. It is created on demand, in whichever SQLite
-- database a comment was set in: a connected application database as well as
-- the primary. Run this in each:
--
--   ALTER TABLE "_sc_object_comments" RENAME TO "_fd_object_comments";
--
-- Skip a file where the table is absent: nothing there has a comment, and
-- Feldspar creates it under the new name the first time one is set. Missing
-- this step is not silent — introspection reads no comments back, so that
-- database's constraints lose their messages and stop being recognised as
-- Feldspar's, and the next comment set there recreates the table empty.
