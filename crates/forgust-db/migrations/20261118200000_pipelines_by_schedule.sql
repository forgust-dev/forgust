-- A schedule's own runs (its card lists them), and deleting a schedule (its
-- pipelines' schedule_id is set to NULL) without a scan of every pipeline.
CREATE INDEX ci_pipelines_schedule_idx ON ci_pipelines (schedule_id, id DESC) WHERE schedule_id IS NOT NULL;
