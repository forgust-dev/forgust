-- Mirrors of a project's repository: pulled from another forge, or pushed to it.
CREATE TABLE project_mirrors (
    id                 bigserial PRIMARY KEY,
    project_id         bigint  NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    -- pull: this repository follows `url` (and is read-only until the mirror is
    -- removed, paused or not: resuming would overwrite what changed meanwhile);
    -- push: `url` follows this repository
    direction          text    NOT NULL CHECK (direction IN ('pull', 'push')),
    -- http(s) without credentials. Never changed: a stored token must not be
    -- sent to another address, so a new address is a new mirror.
    url                text    NOT NULL,
    username           text    NOT NULL DEFAULT '',
    -- encrypted (SecretBox)
    token_enc          text,
    enabled            boolean NOT NULL DEFAULT true,
    -- push: only protected branches, and nothing deleted on the remote
    only_protected     boolean NOT NULL DEFAULT false,
    -- pull: pipelines for what arrives, run as the mirror's creator
    trigger_pipelines  boolean NOT NULL DEFAULT false,
    -- pull: minutes between updates
    interval_minutes   integer NOT NULL DEFAULT 60 CHECK (interval_minutes BETWEEN 15 AND 10080),
    created_by         bigint  REFERENCES users (id) ON DELETE SET NULL,
    created_at         timestamptz NOT NULL DEFAULT now(),
    -- the last attempt, the last one that worked, and why the last one failed
    last_update_at     timestamptz,
    last_success_at    timestamptz,
    last_error         text
);
CREATE INDEX project_mirrors_project_idx ON project_mirrors (project_id, id);
CREATE UNIQUE INDEX project_mirrors_one_pull_idx ON project_mirrors (project_id) WHERE direction = 'pull';
