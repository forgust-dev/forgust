-- no-transaction
-- What the foreign key to ci_job_definitions looks up when a project's
-- definitions go with it.
-- Built CONCURRENTLY (so outside a transaction, one statement a file): a
-- plain CREATE INDEX holds every write to ci_builds (new jobs, log updates,
-- status changes) until it is built. IF NOT EXISTS: a migration that fails
-- here runs again on the next start.
CREATE INDEX CONCURRENTLY IF NOT EXISTS ci_builds_definition_idx ON ci_builds (definition_id) WHERE definition_id IS NOT NULL;
