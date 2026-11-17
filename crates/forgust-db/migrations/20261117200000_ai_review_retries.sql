-- The review job whose report settled a run: a retried job (another model,
-- an endpoint that was down) settles it again with its own report.
ALTER TABLE ai_review_runs ADD COLUMN build_id bigint REFERENCES ci_builds(id) ON DELETE SET NULL;
