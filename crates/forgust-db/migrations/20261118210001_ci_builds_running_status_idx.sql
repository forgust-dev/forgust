-- no-transaction
-- The running jobs, by status alone (see live_job_log_rows): it replaces
-- ci_builds_running_idx, which held contacted_at.
-- Built CONCURRENTLY (so outside a transaction, one statement a file): a
-- plain CREATE INDEX holds every write to ci_builds (new jobs, log updates,
-- status changes) until it is built. IF NOT EXISTS: a migration that fails
-- here runs again on the next start.
CREATE INDEX CONCURRENTLY IF NOT EXISTS ci_builds_running_status_idx ON ci_builds (status) WHERE status IN ('running', 'canceling');
