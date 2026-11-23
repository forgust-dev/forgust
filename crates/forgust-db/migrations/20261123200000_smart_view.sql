-- Smart View: with it on for a project, Forgust runs a pipeline of its own
-- (source `smart_view`) on each new commit of the default branch and of an
-- open merge request, whose job maps the clone with the assay image and
-- uploads the map as a report. The server keeps the latest map of each (the
-- default branch, each open merge request): older ones go.

ALTER TABLE users DROP CONSTRAINT users_user_type_check;
ALTER TABLE users ADD CONSTRAINT users_user_type_check
    CHECK (user_type IN ('human', 'project_bot', 'group_bot', 'renovate_bot', 'security_bot', 'ai_review_bot', 'smart_view_bot'));

ALTER TABLE ci_pipelines DROP CONSTRAINT ci_pipelines_source_check;
ALTER TABLE ci_pipelines ADD CONSTRAINT ci_pipelines_source_check
    CHECK (source IN ('push', 'web', 'api', 'schedule', 'merge_request_event', 'trigger', 'parent_pipeline', 'pipeline', 'renovate', 'security', 'ai_review', 'sbom', 'smart_view'));
CREATE OR REPLACE FUNCTION is_system_source(source text) RETURNS boolean
LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$ SELECT source IN ('renovate', 'security', 'ai_review', 'sbom', 'smart_view') $$;
DROP INDEX ci_pipelines_system_idx;
CREATE INDEX ci_pipelines_system_idx ON ci_pipelines (source, created_at)
    WHERE source IN ('renovate', 'security', 'ai_review', 'sbom', 'smart_view');

-- what the admin changed over [smart_view] in the config file
CREATE TABLE smart_view_instance (
    id          boolean PRIMARY KEY DEFAULT true CHECK (id),
    overrides   jsonb   NOT NULL DEFAULT '{}',
    updated_at  timestamptz NOT NULL DEFAULT now()
);

-- a project with no row never turned it on
CREATE TABLE smart_view_settings (
    project_id   bigint  PRIMARY KEY REFERENCES projects (id) ON DELETE CASCADE,
    enabled      boolean NOT NULL DEFAULT false,
    bot_user_id  bigint  REFERENCES users (id) ON DELETE SET NULL,
    -- who turned it on
    user_id      bigint  REFERENCES users (id) ON DELETE SET NULL,
    updated_at   timestamptz NOT NULL DEFAULT now()
);

-- one per commit mapped or being mapped: of the default branch (no merge
-- request) or of an open merge request's head. A map that is ready takes the
-- place of the older ones of the same branch or merge request.
CREATE TABLE smart_view_maps (
    id                bigserial PRIMARY KEY,
    project_id        bigint  NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    merge_request_id  bigint  REFERENCES merge_requests (id) ON DELETE CASCADE,
    git_ref           text    NOT NULL,
    sha               text    NOT NULL,
    pipeline_id       bigint  REFERENCES ci_pipelines (id) ON DELETE SET NULL,
    state             text    NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'ready', 'failed')),
    -- the job's report (assay's), once read
    report            jsonb,
    error             text,
    created_at        timestamptz NOT NULL DEFAULT now(),
    finished_at       timestamptz
);
-- a commit is mapped once per branch or merge request; the latest of each is also read by
-- this index (few rows each: older ones go)
CREATE UNIQUE INDEX smart_view_maps_commit_idx ON smart_view_maps (project_id, coalesce(merge_request_id, 0), sha);
CREATE INDEX smart_view_maps_mr_idx ON smart_view_maps (merge_request_id) WHERE merge_request_id IS NOT NULL;
CREATE INDEX smart_view_maps_pipeline_idx ON smart_view_maps (pipeline_id);
CREATE INDEX smart_view_maps_pending_idx ON smart_view_maps (created_at) WHERE state = 'pending';
