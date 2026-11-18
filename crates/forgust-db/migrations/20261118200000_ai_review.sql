-- AI review: on request, Forgust runs a pipeline of its own on a merge request
-- that sends the changes to a model endpoint the admin set, and posts what
-- comes back as notes of a bot of the project. The job holds no write
-- credential: it uploads a report the server checks before posting.

ALTER TABLE users DROP CONSTRAINT users_user_type_check;
ALTER TABLE users ADD CONSTRAINT users_user_type_check
    CHECK (user_type IN ('human', 'project_bot', 'group_bot', 'renovate_bot', 'security_bot', 'ai_review_bot'));

ALTER TABLE ci_pipelines DROP CONSTRAINT ci_pipelines_source_check;
ALTER TABLE ci_pipelines ADD CONSTRAINT ci_pipelines_source_check
    CHECK (source IN ('push', 'web', 'api', 'schedule', 'merge_request_event', 'trigger', 'parent_pipeline', 'pipeline', 'renovate', 'security', 'ai_review'));

CREATE OR REPLACE FUNCTION is_system_source(source text) RETURNS boolean
LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$ SELECT source IN ('renovate', 'security', 'ai_review') $$;

-- what the admin changed over [ai_review] in the config file; the endpoint's
-- key and the reviewer's secret options (a JSON map) are stored encrypted,
-- never sent back
CREATE TABLE ai_review_instance (
    id                  boolean PRIMARY KEY DEFAULT true CHECK (id),
    overrides           jsonb   NOT NULL DEFAULT '{}',
    api_key_enc         text,
    secret_options_enc  text,
    updated_at          timestamptz NOT NULL DEFAULT now()
);

-- a project with no row never turned it on
CREATE TABLE ai_review_settings (
    project_id   bigint  PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    enabled      boolean NOT NULL DEFAULT false,
    bot_user_id  bigint  REFERENCES users(id) ON DELETE SET NULL,
    -- who turned it on
    user_id      bigint  REFERENCES users(id) ON DELETE SET NULL,
    updated_at   timestamptz NOT NULL DEFAULT now()
);

-- one per review pipeline: what it looked at and what came of it
CREATE TABLE ai_review_runs (
    pipeline_id       bigint  PRIMARY KEY REFERENCES ci_pipelines(id) ON DELETE CASCADE,
    merge_request_id  bigint  NOT NULL REFERENCES merge_requests(id) ON DELETE CASCADE,
    base_sha          text    NOT NULL,
    head_sha          text    NOT NULL,
    requested_by      bigint  REFERENCES users(id) ON DELETE SET NULL,
    -- the job whose report settled it: a retried job (another model, an
    -- endpoint that was down) settles it again with its own report
    build_id          bigint  REFERENCES ci_builds(id) ON DELETE SET NULL,
    state             text    NOT NULL DEFAULT 'pending'
                      CHECK (state IN ('pending', 'posted', 'outdated', 'failed')),
    comments_posted   integer NOT NULL DEFAULT 0,
    comments_dropped  integer NOT NULL DEFAULT 0,
    error             text,
    created_at        timestamptz NOT NULL DEFAULT now(),
    finished_at       timestamptz
);
CREATE INDEX ai_review_runs_mr_idx ON ai_review_runs (merge_request_id, created_at DESC);
CREATE INDEX ai_review_runs_pending_idx ON ai_review_runs (created_at) WHERE state = 'pending';
