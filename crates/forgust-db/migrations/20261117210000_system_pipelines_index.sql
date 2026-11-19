-- The sweep's look for Forgust's own pipelines still going (Renovate,
-- security scans, AI reviews) every minute: without it, a scan of every
-- pipeline ever run.
CREATE INDEX ci_pipelines_system_idx ON ci_pipelines (source, created_at) WHERE source IN ('renovate', 'security', 'ai_review');
