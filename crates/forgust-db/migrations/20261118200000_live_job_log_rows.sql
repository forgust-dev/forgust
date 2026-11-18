-- Live job logs (ADR-0014): one row per runner update, appended, instead of
-- 128 KiB chunks that every update rewrote whole. Postgres never updates a
-- value in place: a chunk filled by 32 updates of 4 KiB was rewritten 32
-- times, 2 MiB through TOAST and the WAL for 128 KiB of log. `byte_offset` is
-- where a row's bytes start in the log; the chunks of the jobs running now
-- start at chunk_index * 128 KiB. Only logs not archived yet are here, so the
-- rewrite is small.
ALTER TABLE ci_job_trace_chunks RENAME COLUMN chunk_index TO byte_offset;
ALTER TABLE ci_job_trace_chunks ALTER COLUMN byte_offset TYPE bigint USING byte_offset::bigint * 131072;

-- Each of those updates also sets ci_builds.contacted_at, which this index
-- held: none of them could be HOT, so each wrote a new version of the row
-- and an entry in every index of the table (16 of 107 updates were HOT in a
-- development database). stale_running filters on coalesce(contacted_at,
-- started_at), which the index never served: the status alone finds the
-- running jobs.
DROP INDEX ci_builds_running_idx;
CREATE INDEX ci_builds_running_idx ON ci_builds (status) WHERE status IN ('running', 'canceling');
-- a HOT update needs room on its row's page; and vacuum at 2% of dead rows,
-- not 20%: a table of ten million jobs would wait for two million
ALTER TABLE ci_builds SET (fillfactor = 90, autovacuum_vacuum_scale_factor = 0.02);
