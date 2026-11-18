-- no-transaction
-- The jobs whose spec is still to move to its definition
-- (ci::move_inline_specs): every job made before job definitions, none once
-- the sweep is through.
-- Built CONCURRENTLY (so outside a transaction, one statement a file): a
-- plain CREATE INDEX holds every write to ci_builds (new jobs, log updates,
-- status changes) until it is built. IF NOT EXISTS: a migration that fails
-- here runs again on the next start.
CREATE INDEX CONCURRENTLY IF NOT EXISTS ci_builds_inline_spec_idx ON ci_builds (id) WHERE spec IS NOT NULL;
