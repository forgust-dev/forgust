-- The Smart View's history finds each chapter's merge request by its merge commit: one probe
-- per chapter, not a walk of every merged merge request of the project.
CREATE INDEX merge_requests_merge_commit_idx ON merge_requests (project_id, merge_commit_sha) WHERE state = 'merged';
