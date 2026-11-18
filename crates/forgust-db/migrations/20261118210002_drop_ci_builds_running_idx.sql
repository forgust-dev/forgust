-- no-transaction
-- The index ci_builds_running_status_idx replaces; dropped CONCURRENTLY, so
-- that no query on ci_builds waits for it.
DROP INDEX CONCURRENTLY IF EXISTS ci_builds_running_idx;
