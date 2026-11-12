-- Forgust schema, version 0.1. One file: earlier development migrations were
-- squashed before the first public release. New changes go in new files.

-- ==== init ====

-- Forgust initial schema (M0/M1/M2).
-- Access levels: 10 Guest, 20 Reporter, 30 Developer, 40 Maintainer, 50 Owner.
-- Visibility: 0 private, 10 internal, 20 public.

CREATE TABLE instance_settings (
    id                      boolean PRIMARY KEY DEFAULT true CHECK (id),
    signup_enabled          boolean NOT NULL DEFAULT false,
    allow_personal_projects boolean NOT NULL DEFAULT true,
    default_branch_name     text    NOT NULL DEFAULT 'main',
    updated_at              timestamptz NOT NULL DEFAULT now()
);
INSERT INTO instance_settings DEFAULT VALUES;

-- ---------------------------------------------------------------- users

CREATE TABLE namespaces (
    id              bigserial PRIMARY KEY,
    kind            text      NOT NULL CHECK (kind IN ('user', 'group')),
    name            text      NOT NULL,
    path            text      NOT NULL,
    parent_id       bigint    REFERENCES namespaces (id) ON DELETE RESTRICT,
    -- ancestors from the root down to and including this namespace
    traversal_ids   bigint[]  NOT NULL DEFAULT '{}',
    visibility      smallint  NOT NULL DEFAULT 0 CHECK (visibility IN (0, 10, 20)),
    description     text      NOT NULL DEFAULT '',
    owner_id        bigint,   -- user namespaces only
    -- org policy, only meaningful on top-level groups
    max_visibility          smallint CHECK (max_visibility IN (0, 10, 20)),
    require_two_factor      boolean  NOT NULL DEFAULT false,
    project_creation_level  smallint NOT NULL DEFAULT 40,
    subgroup_creation_level smallint NOT NULL DEFAULT 50,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    CHECK (kind = 'group' OR parent_id IS NULL)
);
CREATE INDEX namespaces_traversal_ids_idx ON namespaces USING gin (traversal_ids);
CREATE INDEX namespaces_parent_idx ON namespaces (parent_id);
CREATE UNIQUE INDEX namespaces_sibling_path_idx ON namespaces (coalesce(parent_id, 0), lower(path));

CREATE TABLE users (
    id                  bigserial PRIMARY KEY,
    username            text      NOT NULL,
    email               text      NOT NULL,
    name                text      NOT NULL DEFAULT '',
    password_hash       text,
    user_type           text      NOT NULL DEFAULT 'human'
                        CHECK (user_type IN ('human', 'project_bot', 'group_bot')),
    -- for bots: the project or namespace whose access token created them
    bot_source_type     text      CHECK (bot_source_type IN ('project', 'namespace')),
    bot_source_id       bigint,
    is_admin            boolean   NOT NULL DEFAULT false,
    is_external         boolean   NOT NULL DEFAULT false,
    state               text      NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'blocked')),
    namespace_id        bigint    REFERENCES namespaces (id) ON DELETE SET NULL,
    totp_secret_enc     text,
    totp_enabled        boolean   NOT NULL DEFAULT false,
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX users_username_idx ON users (lower(username));
CREATE UNIQUE INDEX users_email_idx ON users (lower(email));
ALTER TABLE namespaces ADD CONSTRAINT namespaces_owner_fk
    FOREIGN KEY (owner_id) REFERENCES users (id) ON DELETE CASCADE;

CREATE TABLE identities (
    id          bigserial PRIMARY KEY,
    user_id     bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    provider    text   NOT NULL,
    subject     text   NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (provider, subject)
);

CREATE TABLE sessions (
    id            bigserial PRIMARY KEY,
    token_hash    bytea  NOT NULL UNIQUE,
    user_id       bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    csrf_token    text   NOT NULL,
    ip            text,
    user_agent    text,
    created_at    timestamptz NOT NULL DEFAULT now(),
    last_seen_at  timestamptz NOT NULL DEFAULT now(),
    expires_at    timestamptz NOT NULL
);
CREATE INDEX sessions_user_idx ON sessions (user_id);

-- password accepted, second factor pending
CREATE TABLE login_challenges (
    token_hash  bytea  PRIMARY KEY,
    user_id     bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    expires_at  timestamptz NOT NULL
);

-- OIDC state between redirect and callback
CREATE TABLE oidc_states (
    state          text PRIMARY KEY,
    provider       text NOT NULL,
    nonce          text NOT NULL,
    pkce_verifier  text NOT NULL,
    expires_at     timestamptz NOT NULL
);

-- ---------------------------------------------------------------- projects and routes

CREATE TABLE projects (
    id                bigserial PRIMARY KEY,
    namespace_id      bigint   NOT NULL REFERENCES namespaces (id) ON DELETE RESTRICT,
    name              text     NOT NULL,
    path              text     NOT NULL,
    description       text     NOT NULL DEFAULT '',
    visibility        smallint NOT NULL DEFAULT 0 CHECK (visibility IN (0, 10, 20)),
    default_branch    text,
    ref_format        text     NOT NULL DEFAULT 'files' CHECK (ref_format IN ('files', 'reftable')),
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    last_activity_at  timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX projects_ns_path_idx ON projects (namespace_id, lower(path));

-- the current full path of every namespace and project
CREATE TABLE routes (
    id           bigserial PRIMARY KEY,
    source_type  text   NOT NULL CHECK (source_type IN ('namespace', 'project')),
    source_id    bigint NOT NULL,
    path         text   NOT NULL,
    UNIQUE (source_type, source_id)
);
CREATE UNIQUE INDEX routes_path_idx ON routes (lower(path));
CREATE INDEX routes_path_prefix_idx ON routes (lower(path) text_pattern_ops);

-- old paths after a rename or transfer; looked up only when routes has no match
CREATE TABLE redirect_routes (
    id           bigserial PRIMARY KEY,
    source_type  text   NOT NULL CHECK (source_type IN ('namespace', 'project')),
    source_id    bigint NOT NULL,
    path         text   NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX redirect_routes_path_idx ON redirect_routes (lower(path));

-- ---------------------------------------------------------------- membership and access

CREATE TABLE members (
    id            bigserial PRIMARY KEY,
    source_type   text     NOT NULL CHECK (source_type IN ('namespace', 'project')),
    source_id     bigint   NOT NULL,
    user_id       bigint   NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    access_level  smallint NOT NULL CHECK (access_level IN (10, 20, 30, 40, 50)),
    expires_at    timestamptz,
    created_by    bigint   REFERENCES users (id) ON DELETE SET NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    UNIQUE (source_type, source_id, user_id)
);
CREATE INDEX members_user_idx ON members (user_id);

CREATE TABLE project_group_links (
    id                bigserial PRIMARY KEY,
    project_id        bigint   NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    group_id          bigint   NOT NULL REFERENCES namespaces (id) ON DELETE CASCADE,
    max_access_level  smallint NOT NULL CHECK (max_access_level IN (10, 20, 30, 40, 50)),
    expires_at        timestamptz,
    UNIQUE (project_id, group_id)
);

CREATE TABLE group_group_links (
    id                    bigserial PRIMARY KEY,
    shared_group_id       bigint   NOT NULL REFERENCES namespaces (id) ON DELETE CASCADE,
    shared_with_group_id  bigint   NOT NULL REFERENCES namespaces (id) ON DELETE CASCADE,
    max_access_level      smallint NOT NULL CHECK (max_access_level IN (10, 20, 30, 40, 50)),
    expires_at            timestamptz,
    UNIQUE (shared_group_id, shared_with_group_id)
);

CREATE TABLE project_authorizations (
    user_id       bigint   NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    project_id    bigint   NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    access_level  smallint NOT NULL,
    PRIMARY KEY (user_id, project_id)
);
CREATE INDEX project_authorizations_project_idx ON project_authorizations (project_id);

CREATE TABLE namespace_authorizations (
    user_id       bigint   NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    namespace_id  bigint   NOT NULL REFERENCES namespaces (id) ON DELETE CASCADE,
    access_level  smallint NOT NULL,
    PRIMARY KEY (user_id, namespace_id)
);
CREATE INDEX namespace_authorizations_ns_idx ON namespace_authorizations (namespace_id);

-- Recompute effective access for the given users (ADR-0006).
CREATE FUNCTION refresh_authorizations(uids bigint[]) RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM namespace_authorizations WHERE user_id = ANY (uids);
    DELETE FROM project_authorizations WHERE user_id = ANY (uids);

    -- group membership, inherited down the tree
    WITH direct AS (
        SELECT m.user_id, n.id AS ns_id, m.access_level AS lvl
        FROM members m
        JOIN namespaces n ON n.traversal_ids @> ARRAY[m.source_id]
        WHERE m.source_type = 'namespace'
          AND m.user_id = ANY (uids)
          AND (m.expires_at IS NULL OR m.expires_at > now())
    ),
    -- a group shared with another group: members of the invited group (and its
    -- ancestors) get the lower of their role and the link's maximum
    shared AS (
        SELECT m.user_id, n.id AS ns_id, LEAST(m.access_level, l.max_access_level) AS lvl
        FROM group_group_links l
        JOIN namespaces w ON w.id = l.shared_with_group_id
        JOIN members m ON m.source_type = 'namespace' AND m.source_id = ANY (w.traversal_ids)
        JOIN namespaces n ON n.traversal_ids @> ARRAY[l.shared_group_id]
        WHERE m.user_id = ANY (uids)
          AND (m.expires_at IS NULL OR m.expires_at > now())
          AND (l.expires_at IS NULL OR l.expires_at > now())
    ),
    owners AS (
        SELECT owner_id AS user_id, id AS ns_id, 50::smallint AS lvl
        FROM namespaces WHERE kind = 'user' AND owner_id = ANY (uids)
    )
    INSERT INTO namespace_authorizations (user_id, namespace_id, access_level)
    SELECT user_id, ns_id, max(lvl)
    FROM (SELECT * FROM direct UNION ALL SELECT * FROM shared UNION ALL SELECT * FROM owners) x
    GROUP BY user_id, ns_id;

    INSERT INTO project_authorizations (user_id, project_id, access_level)
    SELECT user_id, project_id, max(lvl)
    FROM (
        SELECT na.user_id, p.id AS project_id, na.access_level AS lvl
        FROM namespace_authorizations na
        JOIN projects p ON p.namespace_id = na.namespace_id
        WHERE na.user_id = ANY (uids)
        UNION ALL
        SELECT m.user_id, m.source_id, m.access_level
        FROM members m
        WHERE m.source_type = 'project' AND m.user_id = ANY (uids)
          AND (m.expires_at IS NULL OR m.expires_at > now())
        UNION ALL
        SELECT m.user_id, l.project_id, LEAST(m.access_level, l.max_access_level)
        FROM project_group_links l
        JOIN namespaces g ON g.id = l.group_id
        JOIN members m ON m.source_type = 'namespace' AND m.source_id = ANY (g.traversal_ids)
        WHERE m.user_id = ANY (uids)
          AND (m.expires_at IS NULL OR m.expires_at > now())
          AND (l.expires_at IS NULL OR l.expires_at > now())
    ) x
    GROUP BY user_id, project_id;
END;
$$;

-- Every user whose access may depend on the tree under `root` (inclusive).
CREATE FUNCTION users_affected_by_namespace(root bigint) RETURNS bigint[]
LANGUAGE sql STABLE AS $$
    SELECT coalesce(array_agg(DISTINCT u), '{}') FROM (
        -- members of the namespace, its ancestors and descendants
        SELECT m.user_id AS u FROM members m
        JOIN namespaces n ON n.id = m.source_id AND m.source_type = 'namespace'
        WHERE n.traversal_ids @> ARRAY[root]
           OR n.id IN (SELECT unnest(traversal_ids) FROM namespaces WHERE id = root)
        UNION
        -- members of projects in the subtree
        SELECT m.user_id FROM members m
        JOIN projects p ON p.id = m.source_id AND m.source_type = 'project'
        JOIN namespaces n ON n.id = p.namespace_id
        WHERE n.traversal_ids @> ARRAY[root]
        UNION
        -- members of groups this subtree (or its projects) is shared with
        SELECT m.user_id FROM group_group_links l
        JOIN namespaces s ON s.id = l.shared_group_id
        JOIN namespaces w ON w.id = l.shared_with_group_id
        JOIN members m ON m.source_type = 'namespace' AND m.source_id = ANY (w.traversal_ids)
        WHERE s.traversal_ids @> ARRAY[root]
           OR s.id IN (SELECT unnest(traversal_ids) FROM namespaces WHERE id = root)
        UNION
        SELECT m.user_id FROM project_group_links l
        JOIN projects p ON p.id = l.project_id
        JOIN namespaces pn ON pn.id = p.namespace_id
        JOIN namespaces g ON g.id = l.group_id
        JOIN members m ON m.source_type = 'namespace' AND m.source_id = ANY (g.traversal_ids)
        WHERE pn.traversal_ids @> ARRAY[root]
        UNION
        SELECT owner_id FROM namespaces WHERE id = root AND owner_id IS NOT NULL
    ) x
$$;

-- ---------------------------------------------------------------- credentials

CREATE TABLE personal_access_tokens (
    id            bigserial PRIMARY KEY,
    user_id       bigint  NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name          text    NOT NULL,
    token_hash    bytea   NOT NULL UNIQUE,
    scopes        text[]  NOT NULL,
    expires_at    timestamptz,
    revoked       boolean NOT NULL DEFAULT false,
    last_used_at  timestamptz,
    created_at    timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX personal_access_tokens_user_idx ON personal_access_tokens (user_id);

CREATE TABLE deploy_tokens (
    id            bigserial PRIMARY KEY,
    name          text    NOT NULL,
    username      text    NOT NULL UNIQUE,
    token_hash    bytea   NOT NULL UNIQUE,
    project_id    bigint  REFERENCES projects (id) ON DELETE CASCADE,
    namespace_id  bigint  REFERENCES namespaces (id) ON DELETE CASCADE,
    scopes        text[]  NOT NULL,
    expires_at    timestamptz,
    revoked       boolean NOT NULL DEFAULT false,
    created_at    timestamptz NOT NULL DEFAULT now(),
    CHECK ((project_id IS NULL) <> (namespace_id IS NULL))
);

CREATE TABLE ssh_keys (
    id            bigserial PRIMARY KEY,
    kind          text   NOT NULL CHECK (kind IN ('user', 'deploy')),
    user_id       bigint REFERENCES users (id) ON DELETE CASCADE,
    title         text   NOT NULL,
    public_key    text   NOT NULL,
    fingerprint   text   NOT NULL UNIQUE,  -- SHA256:...
    expires_at    timestamptz,
    last_used_at  timestamptz,
    created_at    timestamptz NOT NULL DEFAULT now(),
    CHECK ((kind = 'user') = (user_id IS NOT NULL))
);
CREATE INDEX ssh_keys_user_idx ON ssh_keys (user_id);

CREATE TABLE deploy_keys_projects (
    deploy_key_id  bigint  NOT NULL REFERENCES ssh_keys (id) ON DELETE CASCADE,
    project_id     bigint  NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    can_push       boolean NOT NULL DEFAULT false,
    PRIMARY KEY (deploy_key_id, project_id)
);

-- CI job tokens (ADR-0015). Issued by CI in M4; accepted by git HTTP behind a flag.
CREATE TABLE ci_job_tokens (
    id          bigserial PRIMARY KEY,
    token_hash  bytea  NOT NULL UNIQUE,
    project_id  bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    user_id     bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    expires_at  timestamptz NOT NULL
);

-- short-lived tokens handed out by `git-lfs-authenticate` over SSH
CREATE TABLE lfs_ssh_tokens (
    token_hash      bytea  PRIMARY KEY,
    project_id      bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    principal_kind  text   NOT NULL,
    principal_id    bigint NOT NULL,
    can_write       boolean NOT NULL,
    expires_at      timestamptz NOT NULL
);

-- ---------------------------------------------------------------- repository rules

CREATE TABLE protected_branches (
    id                  bigserial PRIMARY KEY,
    project_id          bigint   NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name                text     NOT NULL,   -- branch name or glob (release/*)
    push_access_level   smallint NOT NULL DEFAULT 40 CHECK (push_access_level IN (0, 30, 40)),
    merge_access_level  smallint NOT NULL DEFAULT 40 CHECK (merge_access_level IN (0, 30, 40)),
    allow_force_push    boolean  NOT NULL DEFAULT false,
    allow_deploy_keys   boolean  NOT NULL DEFAULT false,
    created_at          timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id, name)
);

CREATE TABLE repo_maintenance (
    project_id                bigint PRIMARY KEY REFERENCES projects (id) ON DELETE CASCADE,
    pushes_since_maintenance  integer NOT NULL DEFAULT 0,
    last_push_at              timestamptz,
    last_maintenance_at       timestamptz
);

-- ---------------------------------------------------------------- LFS

CREATE TABLE lfs_objects (
    oid         text   PRIMARY KEY CHECK (oid ~ '^[0-9a-f]{64}$'),
    size        bigint NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE lfs_objects_projects (
    project_id  bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    oid         text   NOT NULL REFERENCES lfs_objects (oid) ON DELETE CASCADE,
    PRIMARY KEY (project_id, oid)
);

-- ---------------------------------------------------------------- audit

CREATE TABLE audit_events (
    id              bigserial PRIMARY KEY,
    created_at      timestamptz NOT NULL DEFAULT now(),
    principal_kind  text   NOT NULL,
    principal_id    bigint,
    action          text   NOT NULL,
    target_type     text,
    target_id       bigint,
    details         jsonb  NOT NULL DEFAULT '{}',
    ip              text
);
CREATE INDEX audit_events_target_idx ON audit_events (target_type, target_id, created_at DESC);
CREATE INDEX audit_events_created_idx ON audit_events (created_at DESC);

-- ==== merge_requests ====

-- M3: merge requests, review, approvals, webhooks.

-- Per-project counters (MR `!12`, later issues `#12`).
CREATE TABLE internal_ids (
    project_id  bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    usage       text   NOT NULL,
    last_value  bigint NOT NULL,
    PRIMARY KEY (project_id, usage)
);

CREATE TABLE merge_requests (
    id                bigserial PRIMARY KEY,
    project_id        bigint  NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    iid               bigint  NOT NULL,
    title             text    NOT NULL,
    description       text    NOT NULL DEFAULT '',
    author_id         bigint  REFERENCES users (id) ON DELETE SET NULL,
    source_branch     text    NOT NULL,
    target_branch     text    NOT NULL,
    state             text    NOT NULL DEFAULT 'opened' CHECK (state IN ('opened', 'closed', 'merged')),
    draft             boolean NOT NULL DEFAULT false,
    -- default merge method chosen at creation; the merge call may override it
    squash            boolean NOT NULL DEFAULT false,
    merge_commit_sha  text,
    merged_by         bigint  REFERENCES users (id) ON DELETE SET NULL,
    merged_at         timestamptz,
    closed_at         timestamptz,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id, iid)
);
CREATE INDEX merge_requests_open_source_idx ON merge_requests (project_id, source_branch) WHERE state = 'opened';
CREATE INDEX merge_requests_list_idx ON merge_requests (project_id, state, updated_at DESC);

-- One row per version (each push to the source branch while the MR is open).
CREATE TABLE merge_request_diffs (
    id                bigserial PRIMARY KEY,
    merge_request_id  bigint NOT NULL REFERENCES merge_requests (id) ON DELETE CASCADE,
    version           integer NOT NULL,
    base_sha          text   NOT NULL,  -- merge base of start and head
    start_sha         text   NOT NULL,  -- target branch head when the version was made
    head_sha          text   NOT NULL,  -- source branch head
    commits_count     integer NOT NULL,
    files_count       integer NOT NULL,
    additions         integer NOT NULL,
    deletions         integer NOT NULL,
    created_at        timestamptz NOT NULL DEFAULT now(),
    UNIQUE (merge_request_id, version)
);

-- Comments: general (no position) or anchored to a diff line of a version.
CREATE TABLE notes (
    id                bigserial PRIMARY KEY,
    merge_request_id  bigint NOT NULL REFERENCES merge_requests (id) ON DELETE CASCADE,
    author_id         bigint REFERENCES users (id) ON DELETE SET NULL,
    -- the first note of a thread has no parent; replies point at it
    parent_id         bigint REFERENCES notes (id) ON DELETE CASCADE,
    body              text   NOT NULL,
    system            boolean NOT NULL DEFAULT false,
    -- position (all NULL for general comments)
    diff_id           bigint REFERENCES merge_request_diffs (id) ON DELETE SET NULL,
    base_sha          text,
    start_sha         text,
    head_sha          text,
    old_path          text,
    new_path          text,
    old_line          integer,
    new_line          integer,
    resolved_by       bigint REFERENCES users (id) ON DELETE SET NULL,
    resolved_at       timestamptz,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    CHECK (old_line IS NOT NULL OR new_line IS NOT NULL OR head_sha IS NULL)
);
CREATE INDEX notes_mr_idx ON notes (merge_request_id, id);

-- Minimal approval rules: N approvals from users with at least a role.
CREATE TABLE approval_rules (
    id                  bigserial PRIMARY KEY,
    project_id          bigint   NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name                text     NOT NULL,
    approvals_required  smallint NOT NULL CHECK (approvals_required BETWEEN 1 AND 20),
    min_access_level    smallint NOT NULL DEFAULT 30 CHECK (min_access_level IN (20, 30, 40, 50)),
    created_at          timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id, name)
);

CREATE TABLE approvals (
    merge_request_id  bigint NOT NULL REFERENCES merge_requests (id) ON DELETE CASCADE,
    user_id           bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at        timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (merge_request_id, user_id)
);

CREATE TABLE webhooks (
    id            bigserial PRIMARY KEY,
    project_id    bigint  REFERENCES projects (id) ON DELETE CASCADE,
    namespace_id  bigint  REFERENCES namespaces (id) ON DELETE CASCADE,
    url           text    NOT NULL,
    secret_enc    text,
    events        text[]  NOT NULL,
    enabled       boolean NOT NULL DEFAULT true,
    created_at    timestamptz NOT NULL DEFAULT now(),
    CHECK ((project_id IS NULL) <> (namespace_id IS NULL))
);

CREATE TABLE webhook_deliveries (
    id            bigserial PRIMARY KEY,
    webhook_id    bigint  NOT NULL REFERENCES webhooks (id) ON DELETE CASCADE,
    event         text    NOT NULL,
    payload       jsonb   NOT NULL,
    status        text    NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'delivered', 'failed')),
    attempts      integer NOT NULL DEFAULT 0,
    response_code integer,
    error         text,
    duration_ms   integer,
    created_at    timestamptz NOT NULL DEFAULT now(),
    delivered_at  timestamptz
);
CREATE INDEX webhook_deliveries_hook_idx ON webhook_deliveries (webhook_id, id DESC);

-- ==== ci ====

-- M4: CI compatible with GitLab Runner (ADR-0003, ADR-0014, ADR-0017)

-- per-project CI settings (one row per project, created lazily)
CREATE TABLE ci_project_settings (
    project_id                 bigint  PRIMARY KEY REFERENCES projects (id) ON DELETE CASCADE,
    ci_config_path             text    NOT NULL DEFAULT '.gitlab-ci.yml',
    instance_runners_enabled   boolean NOT NULL DEFAULT true,
    default_timeout_secs       integer NOT NULL DEFAULT 3600 CHECK (default_timeout_secs BETWEEN 600 AND 2592000),
    default_git_depth          integer NOT NULL DEFAULT 20 CHECK (default_git_depth BETWEEN 0 AND 1000),
    -- MR merge check
    only_merge_if_pipeline_succeeds boolean NOT NULL DEFAULT false,
    -- inbound job-token access: only projects on the allowlist may use their job tokens here
    job_token_scope_enabled    boolean NOT NULL DEFAULT true
);

-- runners: created in the UI/API, authenticate with a glrt- token (ADR-0017)
CREATE TABLE ci_runners (
    id               bigserial PRIMARY KEY,
    kind             text    NOT NULL CHECK (kind IN ('instance', 'group', 'project')),
    group_id         bigint  REFERENCES namespaces (id) ON DELETE CASCADE,
    project_id       bigint  REFERENCES projects (id) ON DELETE CASCADE,
    description      text    NOT NULL DEFAULT '',
    token_hash       bytea   NOT NULL UNIQUE,
    token_last4      text    NOT NULL DEFAULT '',
    tags             text[]  NOT NULL DEFAULT '{}',
    run_untagged     boolean NOT NULL DEFAULT true,
    protected_only   boolean NOT NULL DEFAULT false,
    paused           boolean NOT NULL DEFAULT false,
    maximum_timeout_secs integer,
    created_by       bigint  REFERENCES users (id) ON DELETE SET NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    contacted_at     timestamptz,
    CHECK ((kind = 'instance' AND group_id IS NULL AND project_id IS NULL)
        OR (kind = 'group' AND group_id IS NOT NULL AND project_id IS NULL)
        OR (kind = 'project' AND project_id IS NOT NULL AND group_id IS NULL))
);

-- one machine using a runner's token, keyed by the runner's system_id
CREATE TABLE ci_runner_managers (
    id            bigserial PRIMARY KEY,
    runner_id     bigint NOT NULL REFERENCES ci_runners (id) ON DELETE CASCADE,
    system_id     text   NOT NULL,
    version       text   NOT NULL DEFAULT '',
    revision      text   NOT NULL DEFAULT '',
    platform      text   NOT NULL DEFAULT '',
    architecture  text   NOT NULL DEFAULT '',
    executor      text   NOT NULL DEFAULT '',
    ip_address    text   NOT NULL DEFAULT '',
    created_at    timestamptz NOT NULL DEFAULT now(),
    contacted_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (runner_id, system_id)
);

CREATE TABLE ci_pipelines (
    id            bigserial PRIMARY KEY,
    project_id    bigint  NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    iid           bigint  NOT NULL,
    ref           text    NOT NULL,
    tag           boolean NOT NULL DEFAULT false,
    sha           text    NOT NULL,
    before_sha    text    NOT NULL DEFAULT '0000000000000000000000000000000000000000',
    source        text    NOT NULL CHECK (source IN ('push', 'web', 'api', 'schedule', 'merge_request_event', 'trigger')),
    status        text    NOT NULL DEFAULT 'created',
    protected     boolean NOT NULL DEFAULT false,
    merge_request_id bigint REFERENCES merge_requests (id) ON DELETE SET NULL,
    schedule_id   bigint,
    user_id       bigint  REFERENCES users (id) ON DELETE SET NULL,
    -- config errors: the pipeline exists (so people see why) but runs nothing
    yaml_errors   text,
    -- pipeline-level variables (web/API/schedule), encrypted
    variables_enc text,
    created_at    timestamptz NOT NULL DEFAULT now(),
    started_at    timestamptz,
    finished_at   timestamptz,
    updated_at    timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id, iid)
);
CREATE INDEX ci_pipelines_project_idx ON ci_pipelines (project_id, id DESC);
CREATE INDEX ci_pipelines_sha_idx ON ci_pipelines (project_id, sha);
CREATE INDEX ci_pipelines_ref_idx ON ci_pipelines (project_id, ref, id DESC);
CREATE INDEX ci_pipelines_mr_idx ON ci_pipelines (merge_request_id, id DESC) WHERE merge_request_id IS NOT NULL;

-- jobs ("builds", GitLab's name, kept for the payload)
CREATE TABLE ci_builds (
    id             bigserial PRIMARY KEY,
    pipeline_id    bigint  NOT NULL REFERENCES ci_pipelines (id) ON DELETE CASCADE,
    project_id     bigint  NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name           text    NOT NULL,
    stage          text    NOT NULL,
    stage_idx      integer NOT NULL,
    status         text    NOT NULL DEFAULT 'created'
                   CHECK (status IN ('created', 'waiting_for_resource', 'preparing', 'pending', 'running',
                                     'success', 'failed', 'canceling', 'canceled', 'skipped', 'manual', 'scheduled')),
    -- on_success, on_failure, always, manual, delayed
    when_          text    NOT NULL DEFAULT 'on_success',
    allow_failure  boolean NOT NULL DEFAULT false,
    -- names of jobs this one needs (DAG); NULL means "all jobs of earlier stages"
    needs          text[],
    -- the normalized job definition the payload is built from
    spec           jsonb   NOT NULL,
    retry_max      integer NOT NULL DEFAULT 0,
    retry_when     text[]  NOT NULL DEFAULT '{}',
    attempt        integer NOT NULL DEFAULT 1,
    -- replaced by a newer attempt (kept for history)
    retried        boolean NOT NULL DEFAULT false,
    tags           text[]  NOT NULL DEFAULT '{}',
    timeout_secs   integer NOT NULL DEFAULT 3600,
    user_id        bigint  REFERENCES users (id) ON DELETE SET NULL,
    runner_id      bigint  REFERENCES ci_runners (id) ON DELETE SET NULL,
    runner_manager_id bigint REFERENCES ci_runner_managers (id) ON DELETE SET NULL,
    failure_reason text,
    exit_code      integer,
    trace_size     bigint  NOT NULL DEFAULT 0,
    trace_archived boolean NOT NULL DEFAULT false,
    created_at     timestamptz NOT NULL DEFAULT now(),
    queued_at      timestamptz,
    started_at     timestamptz,
    finished_at    timestamptz,
    -- last time the runner talked about this job (PUT or PATCH)
    contacted_at   timestamptz,
    cancel_requested_at timestamptz,
    updated_at     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ci_builds_pipeline_idx ON ci_builds (pipeline_id, stage_idx, id);
CREATE INDEX ci_builds_running_idx ON ci_builds (status, contacted_at) WHERE status IN ('running', 'canceling');

-- the pending queue runners claim from (ADR-0014): only waiting jobs live here
CREATE TABLE ci_pending_jobs (
    build_id       bigint  PRIMARY KEY REFERENCES ci_builds (id) ON DELETE CASCADE,
    project_id     bigint  NOT NULL,
    tags           text[]  NOT NULL DEFAULT '{}',
    protected      boolean NOT NULL DEFAULT false,
    traversal_ids  bigint[] NOT NULL,
    instance_runners_enabled boolean NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ci_pending_jobs_project_idx ON ci_pending_jobs (project_id);

-- live logs in 128 KB chunks; moved to the blob store when the job ends
CREATE TABLE ci_job_trace_chunks (
    build_id     bigint  NOT NULL REFERENCES ci_builds (id) ON DELETE CASCADE,
    chunk_index  integer NOT NULL,
    data         bytea   NOT NULL,
    PRIMARY KEY (build_id, chunk_index)
);
ALTER TABLE ci_job_trace_chunks SET (autovacuum_vacuum_scale_factor = 0.02, autovacuum_analyze_scale_factor = 0.05);

CREATE TABLE ci_job_artifacts (
    id            bigserial PRIMARY KEY,
    build_id      bigint NOT NULL REFERENCES ci_builds (id) ON DELETE CASCADE,
    project_id    bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    file_type     text   NOT NULL,   -- archive, metadata, trace, junit, ...
    file_format   text   NOT NULL,   -- zip, gzip, raw
    filename      text   NOT NULL,
    size          bigint NOT NULL,
    blob_key      text   NOT NULL,
    expire_at     timestamptz,
    created_at    timestamptz NOT NULL DEFAULT now(),
    UNIQUE (build_id, file_type)
);
CREATE INDEX ci_job_artifacts_expire_idx ON ci_job_artifacts (expire_at) WHERE expire_at IS NOT NULL;

-- CI/CD variables of projects and groups; values encrypted at rest
CREATE TABLE ci_variables (
    id            bigserial PRIMARY KEY,
    project_id    bigint REFERENCES projects (id) ON DELETE CASCADE,
    group_id      bigint REFERENCES namespaces (id) ON DELETE CASCADE,
    key           text    NOT NULL CHECK (key ~ '^[A-Za-z0-9_]{1,255}$'),
    value_enc     text    NOT NULL,
    variable_type text    NOT NULL DEFAULT 'env_var' CHECK (variable_type IN ('env_var', 'file')),
    protected     boolean NOT NULL DEFAULT false,
    masked        boolean NOT NULL DEFAULT false,
    raw           boolean NOT NULL DEFAULT false,
    environment_scope text NOT NULL DEFAULT '*',
    description   text    NOT NULL DEFAULT '',
    created_at    timestamptz NOT NULL DEFAULT now(),
    CHECK ((project_id IS NULL) <> (group_id IS NULL))
);
CREATE UNIQUE INDEX ci_variables_project_key_idx ON ci_variables (project_id, key, environment_scope) WHERE project_id IS NOT NULL;
CREATE UNIQUE INDEX ci_variables_group_key_idx ON ci_variables (group_id, key, environment_scope) WHERE group_id IS NOT NULL;

-- projects whose job tokens may access this project (inbound allowlist)
CREATE TABLE ci_job_token_allowlist (
    project_id          bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    source_project_id   bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    created_at          timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, source_project_id)
);

CREATE TABLE ci_pipeline_schedules (
    id            bigserial PRIMARY KEY,
    project_id    bigint  NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    description   text    NOT NULL DEFAULT '',
    ref           text    NOT NULL,
    cron          text    NOT NULL,
    cron_timezone text    NOT NULL DEFAULT 'UTC',
    active        boolean NOT NULL DEFAULT true,
    owner_id      bigint  REFERENCES users (id) ON DELETE SET NULL,
    variables_enc text,
    next_run_at   timestamptz,
    last_pipeline_id bigint REFERENCES ci_pipelines (id) ON DELETE SET NULL,
    created_at    timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ci_pipeline_schedules_due_idx ON ci_pipeline_schedules (next_run_at) WHERE active;
ALTER TABLE ci_pipelines ADD CONSTRAINT ci_pipelines_schedule_fk FOREIGN KEY (schedule_id) REFERENCES ci_pipeline_schedules (id) ON DELETE SET NULL;

-- a job token belongs to one build and dies with it
ALTER TABLE ci_job_tokens ADD COLUMN build_id bigint REFERENCES ci_builds (id) ON DELETE CASCADE;
CREATE INDEX ci_job_tokens_build_idx ON ci_job_tokens (build_id);

-- ==== ci_tuning ====

-- M4 review: data kept on rows instead of recomputed, and indexes for sweeps

-- list pages show it without starting git per row
ALTER TABLE ci_pipelines ADD COLUMN commit_title text;

-- final updates refused because log bytes were missing (bounded retries)
ALTER TABLE ci_builds ADD COLUMN trace_rewinds smallint NOT NULL DEFAULT 0;

-- the per-minute sweep: finished jobs whose log is not archived yet
CREATE INDEX ci_builds_unarchived_idx ON ci_builds (finished_at) WHERE NOT trace_archived;
-- ON DELETE SET NULL from runners and runner managers
CREATE INDEX ci_builds_runner_idx ON ci_builds (runner_id) WHERE runner_id IS NOT NULL;
CREATE INDEX ci_builds_runner_manager_idx ON ci_builds (runner_manager_id) WHERE runner_manager_id IS NOT NULL;

-- ==== issues ====

-- M5: minimal issues and labels. The importer moves both, so they come first
-- (vision: title, body, comments, labels, state; a per-project `#iid`).

-- Per-project labels, shared by issues and merge requests.
CREATE TABLE labels (
    id           bigserial PRIMARY KEY,
    project_id   bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name         text   NOT NULL CHECK (length(name) BETWEEN 1 AND 255),
    -- #rrggbb
    color        text   NOT NULL DEFAULT '#6366f1' CHECK (color ~ '^#[0-9a-fA-F]{6}$'),
    description  text   NOT NULL DEFAULT '',
    created_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id, name)
);

CREATE TABLE issues (
    id               bigserial PRIMARY KEY,
    project_id       bigint  NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    -- from internal_ids (usage 'issues')
    iid              bigint  NOT NULL,
    title            text    NOT NULL,
    description      text    NOT NULL DEFAULT '',
    author_id        bigint  REFERENCES users (id) ON DELETE SET NULL,
    -- who wrote it on the source forge, when they have no account here
    imported_author  text,
    state            text    NOT NULL DEFAULT 'opened' CHECK (state IN ('opened', 'closed')),
    closed_by        bigint  REFERENCES users (id) ON DELETE SET NULL,
    closed_at        timestamptz,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id, iid)
);
CREATE INDEX issues_list_idx ON issues (project_id, state, updated_at DESC);

CREATE TABLE issue_labels (
    issue_id  bigint NOT NULL REFERENCES issues (id) ON DELETE CASCADE,
    label_id  bigint NOT NULL REFERENCES labels (id) ON DELETE CASCADE,
    PRIMARY KEY (issue_id, label_id)
);
CREATE INDEX issue_labels_label_idx ON issue_labels (label_id);

CREATE TABLE merge_request_labels (
    merge_request_id  bigint NOT NULL REFERENCES merge_requests (id) ON DELETE CASCADE,
    label_id          bigint NOT NULL REFERENCES labels (id) ON DELETE CASCADE,
    PRIMARY KEY (merge_request_id, label_id)
);
CREATE INDEX merge_request_labels_label_idx ON merge_request_labels (label_id);

-- Comments on issues: flat, no diff positions or resolution (unlike MR notes).
CREATE TABLE issue_notes (
    id               bigserial PRIMARY KEY,
    issue_id         bigint  NOT NULL REFERENCES issues (id) ON DELETE CASCADE,
    author_id        bigint  REFERENCES users (id) ON DELETE SET NULL,
    imported_author  text,
    body             text    NOT NULL,
    -- "closed this", "added ~bug": written by the server, not editable
    system           boolean NOT NULL DEFAULT false,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX issue_notes_issue_idx ON issue_notes (issue_id, id);

-- Imported merge requests and their comments keep their source author too.
ALTER TABLE merge_requests ADD COLUMN imported_author text;
ALTER TABLE notes ADD COLUMN imported_author text;

-- ==== imports ====

-- M5: importing groups of projects from GitLab and GitHub (ADR-0018).

CREATE TABLE imports (
    id                   bigserial PRIMARY KEY,
    user_id              bigint  NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    source               text    NOT NULL CHECK (source IN ('gitlab', 'github')),
    -- the forge's web URL (https://gitlab.example.com, https://github.com)
    base_url             text    NOT NULL,
    -- encrypted (SecretBox); erased when the import ends
    token_enc            text,
    target_namespace_id  bigint  NOT NULL REFERENCES namespaces (id) ON DELETE CASCADE,
    -- planning (dry run) → planned → running → finished / failed / canceled
    status               text    NOT NULL DEFAULT 'planning'
                         CHECK (status IN ('planning', 'planned', 'running', 'finished', 'failed', 'canceled')),
    -- what to bring besides the repository: issues, merge_requests, labels,
    -- variables, protected_branches, members, lfs
    options              jsonb   NOT NULL DEFAULT '{}',
    created_at           timestamptz NOT NULL DEFAULT now(),
    finished_at          timestamptz
);
CREATE INDEX imports_user_idx ON imports (user_id, id DESC);

CREATE TABLE import_projects (
    id            bigserial PRIMARY KEY,
    import_id     bigint NOT NULL REFERENCES imports (id) ON DELETE CASCADE,
    -- GitLab project id or GitHub `owner/repo`
    source_id     text   NOT NULL,
    -- full path on the source (group/sub/project), for display
    source_path   text   NOT NULL,
    -- path under the target group (sub/project); subgroups are created
    target_path   text   NOT NULL,
    project_id    bigint REFERENCES projects (id) ON DELETE SET NULL,
    status        text   NOT NULL DEFAULT 'pending'
                  CHECK (status IN ('pending', 'planning', 'planned', 'running', 'finished', 'failed', 'skipped', 'canceled')),
    -- current step while running (repository, labels, issues, merge_requests, ...)
    stage         text,
    -- {"issues": {"done": 3, "total": 10}, ...}
    progress      jsonb  NOT NULL DEFAULT '{}',
    -- dry-run findings and what was left out: {"counts": {...}, "warnings": [...]}
    report        jsonb  NOT NULL DEFAULT '{}',
    error         text,
    started_at    timestamptz,
    finished_at   timestamptz,
    UNIQUE (import_id, source_id)
);

-- What has been imported already, so a rerun skips it (ADR-0018).
CREATE TABLE imported_objects (
    project_id   bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    -- label, issue, issue_note, merge_request, note, ...
    kind         text   NOT NULL,
    external_id  text   NOT NULL,
    local_id     bigint NOT NULL,
    PRIMARY KEY (project_id, kind, external_id)
);

-- People seen on the source, and who they are here (matched by email).
CREATE TABLE import_users (
    import_id      bigint NOT NULL REFERENCES imports (id) ON DELETE CASCADE,
    source_user_id text   NOT NULL,
    username       text   NOT NULL,
    name           text   NOT NULL DEFAULT '',
    email          text,
    user_id        bigint REFERENCES users (id) ON DELETE SET NULL,
    PRIMARY KEY (import_id, source_user_id)
);

-- ==== commit_statuses ====

-- M5: statuses that tools outside Forgust set on commits (Renovate's stability
-- checks, an external CI), through GitLab's POST /projects/:id/statuses/:sha.
CREATE TABLE commit_statuses (
    id           bigserial PRIMARY KEY,
    project_id   bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    sha          text   NOT NULL,
    ref          text,
    -- the context ("renovate/stability-days", "ci/jenkins")
    name         text   NOT NULL,
    status       text   NOT NULL CHECK (status IN ('pending', 'running', 'success', 'failed', 'canceled', 'skipped')),
    description  text,
    target_url   text,
    author_id    bigint REFERENCES users (id) ON DELETE SET NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id, sha, name)
);

-- ==== assignees ====

-- M6: assignees and reviewers.

CREATE TABLE merge_request_assignees (
    merge_request_id  bigint NOT NULL REFERENCES merge_requests (id) ON DELETE CASCADE,
    user_id           bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at        timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (merge_request_id, user_id)
);
CREATE INDEX merge_request_assignees_user_idx ON merge_request_assignees (user_id);

-- A review request; `state` follows what the reviewer did since it was made.
CREATE TABLE merge_request_reviewers (
    merge_request_id  bigint NOT NULL REFERENCES merge_requests (id) ON DELETE CASCADE,
    user_id           bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    state             text   NOT NULL DEFAULT 'unreviewed'
                      CHECK (state IN ('unreviewed', 'reviewed', 'requested_changes', 'approved')),
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (merge_request_id, user_id)
);
CREATE INDEX merge_request_reviewers_user_idx ON merge_request_reviewers (user_id);

CREATE TABLE issue_assignees (
    issue_id    bigint NOT NULL REFERENCES issues (id) ON DELETE CASCADE,
    user_id     bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (issue_id, user_id)
);
CREATE INDEX issue_assignees_user_idx ON issue_assignees (user_id);

-- ==== stacks ====

-- M6: stacked merge requests (ADR-0019).

CREATE TABLE merge_request_stacks (
    id          bigserial PRIMARY KEY,
    project_id  bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    -- the target branch of the bottom merge request
    trunk       text   NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);

ALTER TABLE merge_requests
    ADD COLUMN stack_id bigint REFERENCES merge_request_stacks (id) ON DELETE SET NULL,
    -- the head of the branch below when this MR was last placed on it: the
    -- commits after it are this MR's own (the upstream of a stack rebase)
    ADD COLUMN stack_base_sha text;
CREATE INDEX merge_requests_stack_idx ON merge_requests (stack_id) WHERE stack_id IS NOT NULL;

-- ==== todos ====

-- M6: to-dos (assignments, review requests, mentions).

CREATE TABLE todos (
    id           bigserial PRIMARY KEY,
    user_id      bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    project_id   bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    target_type  text   NOT NULL CHECK (target_type IN ('merge_request', 'issue')),
    target_iid   bigint NOT NULL,
    action       text   NOT NULL CHECK (action IN ('assigned', 'review_requested', 'mentioned')),
    author_id    bigint REFERENCES users (id) ON DELETE SET NULL,
    -- what was written, shortened (mentions)
    body         text   NOT NULL DEFAULT '',
    state        text   NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'done')),
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX todos_user_idx ON todos (user_id, state, created_at DESC);
CREATE INDEX todos_target_idx ON todos (project_id, target_type, target_iid);

-- ==== contributions ====

-- M6: contributions (the calendar on a user's page). Merge requests, issues,
-- comments and approvals are counted from their own tables; pushes need a record.

CREATE TABLE push_events (
    id            bigserial PRIMARY KEY,
    user_id       bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    project_id    bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    ref_name      text   NOT NULL,
    commit_count  integer NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX push_events_user_idx ON push_events (user_id, created_at);

CREATE INDEX merge_requests_author_idx ON merge_requests (author_id, created_at);
CREATE INDEX merge_requests_merged_by_idx ON merge_requests (merged_by, merged_at) WHERE merged_by IS NOT NULL;
CREATE INDEX issues_author_idx ON issues (author_id, created_at);
CREATE INDEX issues_closed_by_idx ON issues (closed_by, closed_at) WHERE closed_by IS NOT NULL;
CREATE INDEX notes_author_idx ON notes (author_id, created_at) WHERE NOT system;
CREATE INDEX issue_notes_author_idx ON issue_notes (author_id, created_at) WHERE NOT system;
CREATE INDEX approvals_user_idx ON approvals (user_id, created_at);

-- ==== auto_merge ====

-- M6: auto-merge. Set by a user who may merge; the MR merges as them once every
-- check passes and its pipeline has succeeded. New commits or a failed
-- pipeline cancel it.

ALTER TABLE merge_requests
    ADD COLUMN auto_merge_by bigint REFERENCES users (id) ON DELETE SET NULL,
    ADD COLUMN auto_merge_method text,
    -- the source head that was reviewed when it was set
    ADD COLUMN auto_merge_sha text,
    ADD COLUMN auto_merge_remove_source boolean NOT NULL DEFAULT false;
CREATE INDEX merge_requests_auto_merge_idx ON merge_requests (project_id, iid) WHERE auto_merge_by IS NOT NULL;

-- ==== auto_merge_target ====

-- Auto-merge is consent to merge one head into one target: changing the
-- target by hand cancels it (a stack retarget carries it to the trunk).
ALTER TABLE merge_requests ADD COLUMN auto_merge_target text;

-- ==== suggestions ====

-- M6: suggested changes; the commit that applied a note's suggestion.
ALTER TABLE notes ADD COLUMN suggestion_applied_sha text;

-- ==== note_ranges ====

-- M6: a diff comment on a range of lines: where the range starts (the
-- position's own line is where it ends and where the thread is shown).
ALTER TABLE notes ADD COLUMN start_old_line integer, ADD COLUMN start_new_line integer;

-- ==== account_email ====

-- M6: password reset and email verification.

-- Accounts that exist were made by administrators, or before verification
-- existed: they count as verified. Self-registered accounts start unverified.
ALTER TABLE users
    ADD COLUMN email_verified_at timestamptz,
    -- a new address waiting for confirmation (the current one stays until then)
    ADD COLUMN unconfirmed_email text;
UPDATE users SET email_verified_at = created_at;

-- Single-use links sent by email; only a hash of the token is kept.
CREATE TABLE user_tokens (
    token_hash  bytea  PRIMARY KEY,
    user_id     bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    purpose     text   NOT NULL CHECK (purpose IN ('password_reset', 'email')),
    -- for `email`: the address the link confirms
    email       text,
    expires_at  timestamptz NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX user_tokens_user_idx ON user_tokens (user_id, purpose);

-- ==== avatars ====

-- M6: avatars (256 px PNGs in the blob store); when each was last set.
ALTER TABLE users ADD COLUMN avatar_at timestamptz;
ALTER TABLE projects ADD COLUMN avatar_at timestamptz;
ALTER TABLE namespaces ADD COLUMN avatar_at timestamptz;

-- ==== mr_remove_source ====

-- M7: delete the source branch on merge, chosen when the MR is opened (GitLab's
-- force_remove_source_branch); the merge box uses it as its default.
ALTER TABLE merge_requests ADD COLUMN remove_source_branch boolean NOT NULL DEFAULT true;

-- ==== releases ====

-- M7: project uploads, releases and their asset links.

-- Blobs whose rows are gone (deleted one by one or with their project): the CI
-- tick deletes them from the blob store. A trigger feeds it, so no path that
-- removes rows (including ON DELETE CASCADE) can leak a blob.
CREATE TABLE blob_deletions (
    id         bigserial PRIMARY KEY,
    blob_key   text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE FUNCTION queue_blob_deletion() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO blob_deletions (blob_key) VALUES (OLD.blob_key);
    RETURN OLD;
END $$;

-- Files attached to a project (comment attachments, release files): GitLab's
-- markdown uploads, served at /-/project/<id>/uploads/<secret>/<filename>.
CREATE TABLE uploads (
    id           bigserial PRIMARY KEY,
    project_id   bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    secret       text   NOT NULL,
    filename     text   NOT NULL,
    size         bigint NOT NULL,
    content_type text   NOT NULL,
    blob_key     text   NOT NULL,
    uploaded_by  bigint REFERENCES users (id) ON DELETE SET NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id, secret)
);
CREATE TRIGGER uploads_blob AFTER DELETE ON uploads FOR EACH ROW EXECUTE FUNCTION queue_blob_deletion();

-- CI artifacts of a deleted project used to stay in the blob store.
CREATE TRIGGER ci_job_artifacts_blob AFTER DELETE ON ci_job_artifacts FOR EACH ROW EXECUTE FUNCTION queue_blob_deletion();

CREATE TABLE releases (
    id          bigserial PRIMARY KEY,
    project_id  bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    tag_name    text   NOT NULL,
    name        text   NOT NULL,
    description text   NOT NULL DEFAULT '',
    author_id   bigint REFERENCES users (id) ON DELETE SET NULL,
    released_at timestamptz NOT NULL DEFAULT now(),
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id, tag_name)
);
CREATE INDEX releases_by_date ON releases (project_id, released_at DESC, id DESC);

CREATE TABLE release_links (
    id                bigserial PRIMARY KEY,
    release_id        bigint NOT NULL REFERENCES releases (id) ON DELETE CASCADE,
    name              text   NOT NULL,
    url               text   NOT NULL,
    -- served at /-/releases/<tag>/downloads/<path> (a redirect to url)
    direct_asset_path text,
    link_type         text   NOT NULL DEFAULT 'other' CHECK (link_type IN ('other', 'runbook', 'image', 'package')),
    created_at        timestamptz NOT NULL DEFAULT now(),
    UNIQUE (release_id, name),
    UNIQUE (release_id, direct_asset_path)
);

-- ==== milestones ====

-- M7: milestones (project-level) on issues and merge requests; due dates on issues.
CREATE TABLE milestones (
    id          bigserial PRIMARY KEY,
    project_id  bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    -- from internal_ids (usage 'milestones')
    iid         bigint NOT NULL,
    title       text   NOT NULL,
    description text   NOT NULL DEFAULT '',
    state       text   NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'closed')),
    start_date  date,
    due_date    date,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id, iid),
    UNIQUE (project_id, title)
);

ALTER TABLE issues ADD COLUMN milestone_id bigint REFERENCES milestones (id) ON DELETE SET NULL;
ALTER TABLE issues ADD COLUMN due_date date;
ALTER TABLE merge_requests ADD COLUMN milestone_id bigint REFERENCES milestones (id) ON DELETE SET NULL;
CREATE INDEX issues_milestone_idx ON issues (milestone_id) WHERE milestone_id IS NOT NULL;
CREATE INDEX merge_requests_milestone_idx ON merge_requests (milestone_id) WHERE milestone_id IS NOT NULL;

-- ==== award_emoji ====

-- M7: emoji reactions on merge requests, issues and their comments. One column
-- per kind of target (real foreign keys, cascading), exactly one set.
CREATE TABLE award_emoji (
    id               bigserial PRIMARY KEY,
    user_id          bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- GitLab's names for GitHub's eight reactions
    name             text   NOT NULL CHECK (name IN ('thumbsup', 'thumbsdown', 'laughing', 'tada', 'confused', 'heart', 'rocket', 'eyes')),
    merge_request_id bigint REFERENCES merge_requests (id) ON DELETE CASCADE,
    issue_id         bigint REFERENCES issues (id) ON DELETE CASCADE,
    note_id          bigint REFERENCES notes (id) ON DELETE CASCADE,
    issue_note_id    bigint REFERENCES issue_notes (id) ON DELETE CASCADE,
    created_at       timestamptz NOT NULL DEFAULT now(),
    CHECK (num_nonnulls(merge_request_id, issue_id, note_id, issue_note_id) = 1)
);
CREATE UNIQUE INDEX award_emoji_mr ON award_emoji (merge_request_id, user_id, name) WHERE merge_request_id IS NOT NULL;
CREATE UNIQUE INDEX award_emoji_issue ON award_emoji (issue_id, user_id, name) WHERE issue_id IS NOT NULL;
CREATE UNIQUE INDEX award_emoji_note ON award_emoji (note_id, user_id, name) WHERE note_id IS NOT NULL;
CREATE UNIQUE INDEX award_emoji_issue_note ON award_emoji (issue_note_id, user_id, name) WHERE issue_note_id IS NOT NULL;

-- ==== notification_settings ====

-- M7: notification levels (ADR-0021). A person's default, and per-project choices.
-- watch > participating > mention > disabled.
ALTER TABLE users ADD COLUMN notification_level text NOT NULL DEFAULT 'participating'
    CHECK (notification_level IN ('watch', 'participating', 'mention', 'disabled'));

CREATE TABLE notification_settings (
    user_id    bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    project_id bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    level      text   NOT NULL CHECK (level IN ('watch', 'participating', 'mention', 'disabled')),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, project_id)
);
CREATE INDEX notification_settings_watchers ON notification_settings (project_id) WHERE level = 'watch';

-- ==== auto_cancel ====

-- M7: auto-cancel redundant pipelines (`interruptible`). The project setting (on by
-- default, as GitLab), and each pipeline's `workflow:auto_cancel:on_new_commit`.
ALTER TABLE ci_project_settings ADD COLUMN auto_cancel_redundant boolean NOT NULL DEFAULT true;
ALTER TABLE ci_pipelines ADD COLUMN auto_cancel text NOT NULL DEFAULT 'conservative'
    CHECK (auto_cancel IN ('conservative', 'interruptible', 'none'));

-- ==== ci_reports ====

-- M7: what the server reads from report artifacts. `dotenv` variables per job
-- (passed to the jobs that download its artifacts); JUnit summaries per job.
CREATE TABLE ci_build_dotenv (
    build_id bigint NOT NULL REFERENCES ci_builds (id) ON DELETE CASCADE,
    position int    NOT NULL,
    key      text   NOT NULL,
    value    text   NOT NULL,
    PRIMARY KEY (build_id, key)
);

CREATE TABLE ci_build_test_reports (
    build_id    bigint PRIMARY KEY REFERENCES ci_builds (id) ON DELETE CASCADE,
    pipeline_id bigint NOT NULL REFERENCES ci_pipelines (id) ON DELETE CASCADE,
    total       int    NOT NULL,
    failed      int    NOT NULL,
    errored     int    NOT NULL,
    skipped     int    NOT NULL,
    duration    double precision NOT NULL DEFAULT 0,
    -- the failing and erroring cases (capped): suite, name, classname, status, message, output
    failures    jsonb  NOT NULL DEFAULT '[]',
    -- every case's identity and status (capped), to compare with the target branch
    cases       jsonb  NOT NULL DEFAULT '[]',
    error       text,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ci_build_test_reports_pipeline ON ci_build_test_reports (pipeline_id);

-- ==== protected_tags ====

-- Protected tags (GitLab's): who may create a matching tag; nobody moves or
-- deletes one by pushing; Maintainers delete them from the UI or API.
CREATE TABLE protected_tags (
    id                  bigserial PRIMARY KEY,
    project_id          bigint   NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name                text     NOT NULL,   -- tag name or glob (v*)
    create_access_level smallint NOT NULL DEFAULT 40 CHECK (create_access_level IN (0, 30, 40)),
    created_at          timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id, name)
);

-- ==== artifact_quota ====

-- per-project artifact storage (the quota check sums it on every upload)
CREATE INDEX ci_job_artifacts_project_idx ON ci_job_artifacts (project_id);

-- ==== review_rules ====

-- Push rules, OpenPGP keys for signed commits, Git LFS file locks, and
-- code owner approval on protected branches.

CREATE TABLE push_rules (
    project_id                     bigint  PRIMARY KEY REFERENCES projects (id) ON DELETE CASCADE,
    commit_message_regex           text    NOT NULL DEFAULT '',
    commit_message_negative_regex  text    NOT NULL DEFAULT '',
    author_email_regex             text    NOT NULL DEFAULT '',
    branch_name_regex              text    NOT NULL DEFAULT '',
    max_file_size_mb               integer NOT NULL DEFAULT 0 CHECK (max_file_size_mb >= 0),
    prevent_secrets                boolean NOT NULL DEFAULT false,
    reject_unsigned_commits        boolean NOT NULL DEFAULT false,
    updated_at                     timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE gpg_keys (
    id           bigserial PRIMARY KEY,
    user_id      bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    armored      text   NOT NULL,
    fingerprint  text   NOT NULL UNIQUE,   -- primary key, upper-case hex
    key_ids      text[] NOT NULL,          -- 16-hex ids: the primary key and its subkeys
    emails       text[] NOT NULL,          -- from the key's user IDs, lower-case
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX gpg_keys_user_idx ON gpg_keys (user_id);
CREATE INDEX gpg_keys_key_ids_idx ON gpg_keys USING gin (key_ids);

CREATE TABLE lfs_locks (
    id          bigserial PRIMARY KEY,
    project_id  bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    path        text   NOT NULL,
    owner_id    bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    ref_name    text,
    created_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id, path)
);

ALTER TABLE protected_branches ADD COLUMN code_owner_approval_required boolean NOT NULL DEFAULT false;

-- ==== registry ====

-- The container registry (ADR-0022).

-- An image repository: a project's path plus an optional name (`""`, `web`, `tools/ci`).
CREATE TABLE registry_repositories (
    id          bigserial PRIMARY KEY,
    project_id  bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name        text   NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id, name)
);

-- Content-addressed blobs (layers, configs), stored once at registry/blobs/sha256/<hex>.
CREATE TABLE registry_blobs (
    digest      text   PRIMARY KEY,          -- sha256:<hex>
    size        bigint NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);

-- A repository may serve a blob only once it is linked (uploaded, mounted, referenced).
CREATE TABLE registry_repository_blobs (
    repository_id  bigint NOT NULL REFERENCES registry_repositories (id) ON DELETE CASCADE,
    digest         text   NOT NULL REFERENCES registry_blobs (digest) ON DELETE CASCADE,
    created_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (repository_id, digest)
);
CREATE INDEX registry_repository_blobs_digest_idx ON registry_repository_blobs (digest);

CREATE TABLE registry_manifests (
    id             bigserial PRIMARY KEY,
    repository_id  bigint NOT NULL REFERENCES registry_repositories (id) ON DELETE CASCADE,
    digest         text   NOT NULL,
    media_type     text   NOT NULL,
    payload        bytea  NOT NULL,
    -- the config and layers it references (blobs), and the manifests an index lists
    blob_digests      text[] NOT NULL DEFAULT '{}',
    manifest_digests  text[] NOT NULL DEFAULT '{}',
    -- total size: payload + config + layers (for an index, its children's)
    total_size     bigint NOT NULL DEFAULT 0,
    subject        text,                     -- OCI referrers
    created_at     timestamptz NOT NULL DEFAULT now(),
    UNIQUE (repository_id, digest)
);

CREATE TABLE registry_tags (
    repository_id  bigint NOT NULL REFERENCES registry_repositories (id) ON DELETE CASCADE,
    name           text   NOT NULL,
    manifest_id    bigint NOT NULL REFERENCES registry_manifests (id) ON DELETE CASCADE,
    updated_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (repository_id, name)
);
CREATE INDEX registry_tags_manifest_idx ON registry_tags (manifest_id);

-- Blob uploads in progress: one part per PATCH, hashed while it streamed.
CREATE TABLE registry_uploads (
    id             uuid   PRIMARY KEY,
    repository_id  bigint NOT NULL REFERENCES registry_repositories (id) ON DELETE CASCADE,
    size           bigint NOT NULL DEFAULT 0,
    parts          integer NOT NULL DEFAULT 0,
    -- digest of the single part, when there is exactly one (rename, no copy)
    single_digest  text,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now()
);

-- Cleanup policy per project (GitLab's): runs with the CI sweep.
CREATE TABLE registry_cleanup_policies (
    project_id       bigint  PRIMARY KEY REFERENCES projects (id) ON DELETE CASCADE,
    enabled          boolean NOT NULL DEFAULT false,
    keep_n           integer,                  -- keep the newest N tags per repository
    older_than_days  integer,                  -- remove tags older than this
    name_regex       text    NOT NULL DEFAULT '.*',   -- tags that may be removed
    name_regex_keep  text    NOT NULL DEFAULT '',     -- tags that are always kept
    last_run_at      timestamptz
);

-- Registry blobs go through the same deletion queue as other blobs (ADR-0020).
CREATE OR REPLACE FUNCTION registry_blob_deleted() RETURNS trigger AS $$
BEGIN
    INSERT INTO blob_deletions (blob_key) VALUES ('registry/blobs/' || replace(OLD.digest, ':', '/'));
    RETURN OLD;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER registry_blobs_deleted AFTER DELETE ON registry_blobs FOR EACH ROW EXECUTE FUNCTION registry_blob_deleted();

-- ==== ci_keywords ====

-- CI keywords GitLab pipelines use. A job's settings stay in its
-- stored spec (JSON); these are the runtime parts.

-- downstream pipelines (`trigger:`): children of the same project, and
-- pipelines of other projects
ALTER TABLE ci_pipelines DROP CONSTRAINT ci_pipelines_source_check;
ALTER TABLE ci_pipelines ADD CONSTRAINT ci_pipelines_source_check
    CHECK (source IN ('push', 'web', 'api', 'schedule', 'merge_request_event', 'trigger', 'parent_pipeline', 'pipeline'));
ALTER TABLE ci_pipelines ADD COLUMN upstream_build_id bigint REFERENCES ci_builds (id) ON DELETE SET NULL;
CREATE INDEX ci_pipelines_upstream_idx ON ci_pipelines (upstream_build_id) WHERE upstream_build_id IS NOT NULL;

ALTER TABLE ci_builds ADD COLUMN resource_group text;
ALTER TABLE ci_builds ADD COLUMN scheduled_at timestamptz;          -- `when: delayed`
ALTER TABLE ci_builds ADD COLUMN coverage double precision;         -- from the log, `coverage:` regex
ALTER TABLE ci_builds ADD COLUMN downstream_pipeline_id bigint REFERENCES ci_pipelines (id) ON DELETE SET NULL;
CREATE INDEX ci_builds_resource_group_idx ON ci_builds (project_id, resource_group, id) WHERE resource_group IS NOT NULL;
CREATE INDEX ci_builds_scheduled_idx ON ci_builds (scheduled_at) WHERE status = 'scheduled';

-- environments and deployments
CREATE TABLE environments (
    id            bigserial PRIMARY KEY,
    project_id    bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name          text   NOT NULL,
    slug          text   NOT NULL,
    external_url  text,
    tier          text   NOT NULL DEFAULT 'other'
                  CHECK (tier IN ('production', 'staging', 'testing', 'development', 'other')),
    state         text   NOT NULL DEFAULT 'available' CHECK (state IN ('available', 'stopping', 'stopped')),
    -- the job that stops it (`on_stop`), from the latest deployment
    stop_build_id bigint REFERENCES ci_builds (id) ON DELETE SET NULL,
    auto_stop_at  timestamptz,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id, name)
);
CREATE INDEX environments_auto_stop_idx ON environments (auto_stop_at) WHERE auto_stop_at IS NOT NULL AND state = 'available';

CREATE TABLE deployments (
    id              bigserial PRIMARY KEY,
    project_id      bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    iid             bigint NOT NULL,
    environment_id  bigint NOT NULL REFERENCES environments (id) ON DELETE CASCADE,
    build_id        bigint REFERENCES ci_builds (id) ON DELETE SET NULL,
    ref             text   NOT NULL,
    tag             boolean NOT NULL DEFAULT false,
    sha             text   NOT NULL,
    status          text   NOT NULL DEFAULT 'created'
                    CHECK (status IN ('created', 'running', 'success', 'failed', 'canceled', 'skipped', 'blocked')),
    user_id         bigint REFERENCES users (id) ON DELETE SET NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),
    finished_at     timestamptz,
    UNIQUE (project_id, iid)
);
CREATE INDEX deployments_environment_idx ON deployments (environment_id, id DESC);
CREATE UNIQUE INDEX deployments_build_idx ON deployments (build_id) WHERE build_id IS NOT NULL;

-- who may deploy to an environment (GitLab's protected environments)
CREATE TABLE protected_environments (
    id                   bigserial PRIMARY KEY,
    project_id           bigint   NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name                 text     NOT NULL,   -- a name or a glob (review/*)
    deploy_access_level  smallint NOT NULL DEFAULT 40 CHECK (deploy_access_level IN (30, 40, 50)),
    UNIQUE (project_id, name)
);

-- `workflow:auto_cancel:on_job_failure`
ALTER TABLE ci_pipelines ADD COLUMN auto_cancel_on_job_failure text NOT NULL DEFAULT 'none'
    CHECK (auto_cancel_on_job_failure IN ('none', 'all'));

-- keys the instance signs with (the RSA key of CI `id_tokens`), encrypted
CREATE TABLE instance_keys (
    name        text PRIMARY KEY,
    value_enc   text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);

-- ==== merge_trains ====

-- Merge trains. Each car is a merge request queued for its target
-- branch, tested on the merge of the target, the cars ahead of it and itself.
ALTER TABLE ci_project_settings ADD COLUMN merge_trains_enabled boolean NOT NULL DEFAULT false;

CREATE TABLE merge_train_cars (
    id                bigserial PRIMARY KEY,
    project_id        bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    target_branch     text   NOT NULL,
    merge_request_id  bigint NOT NULL UNIQUE REFERENCES merge_requests (id) ON DELETE CASCADE,
    user_id           bigint REFERENCES users (id) ON DELETE SET NULL,
    -- what the car was built on: the target (or the car ahead), the MR's head,
    -- and the merge of the two that its pipeline tests
    base_sha          text,
    head_sha          text,
    train_sha         text,
    pipeline_id       bigint REFERENCES ci_pipelines (id) ON DELETE SET NULL,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX merge_train_cars_train_idx ON merge_train_cars (project_id, target_branch, id);
CREATE INDEX merge_train_cars_pipeline_idx ON merge_train_cars (pipeline_id) WHERE pipeline_id IS NOT NULL;

-- ==== renovate ====

-- ADR-0023: built-in Renovate. A project turns it on; Forgust runs
-- Renovate as a CI job of its own (pipeline source `renovate`) with a bot
-- user that is a Developer of that one project.

ALTER TABLE users DROP CONSTRAINT users_user_type_check;
ALTER TABLE users ADD CONSTRAINT users_user_type_check
    CHECK (user_type IN ('human', 'project_bot', 'group_bot', 'renovate_bot'));

ALTER TABLE ci_pipelines DROP CONSTRAINT ci_pipelines_source_check;
ALTER TABLE ci_pipelines ADD CONSTRAINT ci_pipelines_source_check
    CHECK (source IN ('push', 'web', 'api', 'schedule', 'merge_request_event', 'trigger', 'parent_pipeline', 'pipeline', 'renovate'));

CREATE TABLE renovate_settings (
    project_id        bigint      PRIMARY KEY REFERENCES projects (id) ON DELETE CASCADE,
    enabled           boolean     NOT NULL DEFAULT false,
    -- hours between runs (spread with jitter)
    interval_hours    integer     NOT NULL DEFAULT 24 CHECK (interval_hours BETWEEN 1 AND 168),
    -- Renovate opens its onboarding merge request; false: defaults, no config file needed
    onboarding        boolean     NOT NULL DEFAULT true,
    bot_user_id       bigint      REFERENCES users (id) ON DELETE SET NULL,
    next_run_at       timestamptz,
    last_pipeline_id  bigint      REFERENCES ci_pipelines (id) ON DELETE SET NULL,
    updated_at        timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX renovate_settings_due_idx ON renovate_settings (next_run_at) WHERE enabled;

-- ==== instance_only_jobs ====

-- Built-in Renovate jobs carry instance secrets (a github.com token): only
-- instance runners take them, whatever the project's shared-runner setting.
ALTER TABLE ci_pending_jobs ADD COLUMN instance_only boolean NOT NULL DEFAULT false;

-- ==== renovate_admin ====

-- ADR-0023: the rest of built-in Renovate.

-- what an administrator changed over the config file's [renovate] section
-- (JSON of the fields set); the github.com token is stored encrypted
CREATE TABLE renovate_instance (
    id                    boolean PRIMARY KEY DEFAULT true CHECK (id),
    overrides             jsonb   NOT NULL DEFAULT '{}',
    github_com_token_enc  text,
    updated_at            timestamptz NOT NULL DEFAULT now()
);

-- credentials Renovate uses for a project's private registries (hostRules);
-- the password or token is stored encrypted
CREATE TABLE renovate_host_rules (
    id            bigserial PRIMARY KEY,
    project_id    bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    match_host    text   NOT NULL,
    host_type     text,
    username      text   NOT NULL DEFAULT '',
    password_enc  text   NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX renovate_host_rules_project_idx ON renovate_host_rules (project_id);

-- a group turns dependency updates on for its projects (those that never
-- chose, and new ones)
CREATE TABLE renovate_group_defaults (
    namespace_id  bigint PRIMARY KEY REFERENCES namespaces (id) ON DELETE CASCADE,
    enabled       boolean NOT NULL DEFAULT false,
    updated_at    timestamptz NOT NULL DEFAULT now()
);

-- ==== review_comments ====

-- Requesting changes says what to change: the comment carries the verdict
-- (`review`), and the review request points at it.
ALTER TABLE notes ADD COLUMN review text CHECK (review IN ('requested_changes'));
ALTER TABLE merge_request_reviewers ADD COLUMN review_note_id bigint REFERENCES notes (id) ON DELETE SET NULL;

-- ==== renovate_instance_registries ====

-- Built-in Renovate, instance-wide registries: host rules with no project
-- apply to every run, and the runner may need credentials to pull the
-- Renovate image itself (a private registry or a mirror).
ALTER TABLE renovate_host_rules ALTER COLUMN project_id DROP NOT NULL;
ALTER TABLE renovate_instance
    ADD COLUMN image_pull_username      text NOT NULL DEFAULT '',
    ADD COLUMN image_pull_password_enc  text;

-- ==== merge_train_options ====

-- How a merge train car merges, chosen when it is added: squashed into one
-- commit on the target (instead of a merge commit), and whether the source
-- branch is deleted after.
ALTER TABLE merge_train_cars
    ADD COLUMN squash                boolean NOT NULL DEFAULT false,
    ADD COLUMN remove_source_branch  boolean NOT NULL DEFAULT false;

-- ==== admin_tools ====

-- Admin tools (read-only views).

-- a repository's size on disk, refreshed after pushes and by the hourly sweep,
-- so the instance's storage adds up without walking every repository
ALTER TABLE projects
    ADD COLUMN repository_bytes     bigint NOT NULL DEFAULT 0,
    ADD COLUMN repository_bytes_at  timestamptz;

-- the last scan of the blob store: objects and bytes per top-level prefix
CREATE TABLE admin_blob_scans (
    id           boolean PRIMARY KEY DEFAULT true CHECK (id),
    started_at   timestamptz NOT NULL DEFAULT now(),
    finished_at  timestamptz,
    -- [{"prefix": "ci", "objects": 12, "bytes": 3400}, ...]
    prefixes     jsonb NOT NULL DEFAULT '[]',
    -- false while it runs, or when it stopped at its limits
    complete     boolean NOT NULL DEFAULT false,
    error        text
);

-- ==== admin_actions ====

-- Admin actions.

-- when someone last signed in (password, two-factor or SSO: a session started)
ALTER TABLE users ADD COLUMN last_sign_in_at timestamptz;

-- a message for everyone, on every page, until it ends
ALTER TABLE instance_settings
    ADD COLUMN banner_text   text NOT NULL DEFAULT '',
    ADD COLUMN banner_tone   text NOT NULL DEFAULT 'info' CHECK (banner_tone IN ('info', 'warn')),
    ADD COLUMN banner_until  timestamptz;

-- ==== archived_projects ====

-- An archived project is read-only (its Owner unarchives it).
ALTER TABLE projects ADD COLUMN archived_at timestamptz;

-- ==== impersonation ====

-- An administrator signed in as someone else. The session
-- is the person's, with the administrator who started it (its writes are
-- audited under both); it goes when that administrator's account goes.
ALTER TABLE sessions ADD COLUMN impersonator_id bigint REFERENCES users (id) ON DELETE CASCADE;

-- ==== orphan_blobs ====

-- The blob scan also reports what nothing references (a dry run).
-- [{"family": "lfs", "objects": 3, "bytes": 1200, "sample": ["lfs/ab/cd/abcd..."]}, ...]
ALTER TABLE admin_blob_scans ADD COLUMN orphans jsonb NOT NULL DEFAULT '[]';

-- The scan looks keys up by batch.
CREATE INDEX ci_job_artifacts_blob_key_idx ON ci_job_artifacts (blob_key);
CREATE INDEX uploads_blob_key_idx ON uploads (blob_key);
CREATE INDEX blob_deletions_blob_key_idx ON blob_deletions (blob_key);

-- Avatars used to stay in the blob store after their user, project or group was
-- deleted: they go through the deletion queue like other blobs.
CREATE FUNCTION queue_avatar_deletion() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.avatar_at IS NOT NULL THEN
        INSERT INTO blob_deletions (blob_key) VALUES ('avatars/' || TG_ARGV[0] || '/' || OLD.id || '.png');
    END IF;
    RETURN OLD;
END $$;
CREATE TRIGGER users_avatar AFTER DELETE ON users FOR EACH ROW EXECUTE FUNCTION queue_avatar_deletion('user');
CREATE TRIGGER projects_avatar AFTER DELETE ON projects FOR EACH ROW EXECUTE FUNCTION queue_avatar_deletion('project');
CREATE TRIGGER namespaces_avatar AFTER DELETE ON namespaces FOR EACH ROW EXECUTE FUNCTION queue_avatar_deletion('group');

-- ==== storage_quotas ====

-- Storage quotas per top-level namespace and per project. What counts is
-- what Admin > Storage shows for a project: its repository, the LFS objects and
-- image layers it uses, its CI artifacts and logs, its uploads.

-- For every top-level group and personal namespace without its own; 0: none.
ALTER TABLE instance_settings ADD COLUMN namespace_quota_bytes bigint NOT NULL DEFAULT 0 CHECK (namespace_quota_bytes >= 0);
-- Top-level namespaces only. NULL: the instance's; 0: none.
ALTER TABLE namespaces ADD COLUMN quota_bytes bigint CHECK (quota_bytes >= 0);
-- NULL or 0: none (the namespace's still applies).
ALTER TABLE projects ADD COLUMN quota_bytes bigint CHECK (quota_bytes >= 0);
-- How full each was when last measured (0, 80 or 100 %): banners, and one email
-- to the Owners when it goes up.
ALTER TABLE namespaces ADD COLUMN quota_level smallint NOT NULL DEFAULT 0;
ALTER TABLE projects ADD COLUMN quota_level smallint NOT NULL DEFAULT 0;

-- What a project keeps, in bytes (each part by index).
CREATE FUNCTION project_storage_bytes(pid bigint) RETURNS bigint LANGUAGE sql STABLE AS $$
    SELECT (SELECT repository_bytes FROM projects WHERE id = pid)
         + coalesce((SELECT sum(o.size) FROM lfs_objects_projects l JOIN lfs_objects o ON o.oid = l.oid WHERE l.project_id = pid), 0)
         + coalesce((SELECT sum(size) FROM ci_job_artifacts WHERE project_id = pid), 0)
         + coalesce((SELECT sum(b.size)
                       FROM (SELECT DISTINCT l.digest FROM registry_repository_blobs l
                               JOIN registry_repositories r ON r.id = l.repository_id WHERE r.project_id = pid) d
                       JOIN registry_blobs b ON b.digest = d.digest), 0)
         + coalesce((SELECT sum(size) FROM uploads WHERE project_id = pid), 0)
$$;

-- ==== webhook_formats ====

-- A webhook posts the JSON payload (generic) or one chat message.
ALTER TABLE webhooks ADD COLUMN format text NOT NULL DEFAULT 'generic'
    CHECK (format IN ('generic', 'slack', 'mattermost', 'discord', 'matrix', 'teams'));

-- ==== merge_train_rebase ====

-- A car may land as its own commits rebased onto what is ahead
-- (rebase / fast-forward merges), instead of a merge or a squashed commit.
ALTER TABLE merge_train_cars ADD COLUMN rebase boolean NOT NULL DEFAULT false;

-- ==== pages ====

-- Pages. A project's site is the `publish` folder of a passing Pages
-- job's artifacts archive; the archive is kept (no expiry) while it is the site.
CREATE TABLE pages_deployments (
    project_id   bigint PRIMARY KEY REFERENCES projects (id) ON DELETE CASCADE,
    build_id     bigint NOT NULL REFERENCES ci_builds (id) ON DELETE CASCADE,
    artifact_id  bigint NOT NULL REFERENCES ci_job_artifacts (id) ON DELETE CASCADE,
    publish      text   NOT NULL,
    -- when the archive expired before the site kept it (restored when replaced)
    artifact_expire_at timestamptz,
    created_at   timestamptz NOT NULL DEFAULT now()
);

-- ==== security_findings ====

-- ADR-0024: security findings read from the reports pipelines write
-- (GitLab's security report JSON and SARIF), one per fingerprint per project.

CREATE TABLE security_findings (
    id              bigserial PRIMARY KEY,
    project_id      bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    -- stable across pipelines: report type, primary identifier, location
    fingerprint     text   NOT NULL,
    -- sast, secret_detection, dependency_scanning, container_scanning
    report_type     text   NOT NULL,
    scanner         text   NOT NULL,
    -- critical, high, medium, low, info, unknown
    severity        text   NOT NULL,
    title           text   NOT NULL,
    description     text   NOT NULL DEFAULT '',
    solution        text   NOT NULL DEFAULT '',
    -- the rule (code, secrets) or advisory (dependencies, containers)
    rule_id         text   NOT NULL,
    -- [{"type": "cve", "name": "CVE-2024-1", "value": "CVE-2024-1", "url": "..."}]
    identifiers     jsonb  NOT NULL DEFAULT '[]',
    file            text,
    start_line      integer,
    end_line        integer,
    package_name    text,
    package_version text,
    fixed_version   text,
    image           text,
    -- detected, dismissed, resolved
    state           text   NOT NULL DEFAULT 'detected' CHECK (state IN ('detected', 'dismissed', 'resolved')),
    dismissal_reason text  CHECK (dismissal_reason IN ('false_positive', 'acceptable_risk', 'used_in_tests', 'mitigating_control', 'not_applicable')),
    dismissal_comment text,
    dismissed_by    bigint REFERENCES users (id) ON DELETE SET NULL,
    dismissed_at    timestamptz,
    first_pipeline_id bigint REFERENCES ci_pipelines (id) ON DELETE SET NULL,
    last_pipeline_id  bigint REFERENCES ci_pipelines (id) ON DELETE SET NULL,
    first_seen_at   timestamptz NOT NULL DEFAULT now(),
    last_seen_at    timestamptz NOT NULL DEFAULT now(),
    resolved_at     timestamptz,
    UNIQUE (project_id, fingerprint)
);
CREATE INDEX security_findings_open_idx ON security_findings (project_id, state, severity);

-- Which pipeline saw which finding (merge requests compare two pipelines).
CREATE TABLE security_finding_sightings (
    finding_id  bigint NOT NULL REFERENCES security_findings (id) ON DELETE CASCADE,
    pipeline_id bigint NOT NULL REFERENCES ci_pipelines (id) ON DELETE CASCADE,
    PRIMARY KEY (pipeline_id, finding_id)
);

-- One report a job uploaded, once read.
CREATE TABLE security_scans (
    build_id    bigint NOT NULL REFERENCES ci_builds (id) ON DELETE CASCADE,
    pipeline_id bigint NOT NULL REFERENCES ci_pipelines (id) ON DELETE CASCADE,
    report_type text   NOT NULL,
    scanner     text   NOT NULL DEFAULT '',
    findings    integer NOT NULL DEFAULT 0,
    error       text,
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (build_id, report_type)
);
CREATE INDEX security_scans_pipeline_idx ON security_scans (pipeline_id);

-- What happened to a finding, newest last.
CREATE TABLE security_finding_events (
    id          bigserial PRIMARY KEY,
    finding_id  bigint NOT NULL REFERENCES security_findings (id) ON DELETE CASCADE,
    -- detected, dismissed, reopened, resolved, redetected
    action      text   NOT NULL,
    user_id     bigint REFERENCES users (id) ON DELETE SET NULL,
    pipeline_id bigint REFERENCES ci_pipelines (id) ON DELETE SET NULL,
    note        text,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX security_finding_events_idx ON security_finding_events (finding_id, id);

-- Merge check: no new critical or high finding left undismissed.
ALTER TABLE ci_project_settings ADD COLUMN block_new_security_findings boolean NOT NULL DEFAULT false;

-- ==== security_on_default ====

-- Findings come from any pipeline (merge requests compare them), but
-- only default-branch pipelines make them the project's: seen there now.
ALTER TABLE security_findings ADD COLUMN on_default boolean NOT NULL DEFAULT false;
CREATE INDEX security_findings_default_idx ON security_findings (project_id, on_default);

-- ==== security_scanning ====

-- Built-in scanners (ADR-0024). A project turns them on;
-- Forgust runs them as its own pipeline (source `security`).
CREATE TABLE security_scanning_settings (
    project_id     bigint  PRIMARY KEY REFERENCES projects (id) ON DELETE CASCADE,
    enabled        boolean NOT NULL DEFAULT false,
    -- sast, secret_detection, dependency_scanning, iac, container_scanning
    scanners       text[]  NOT NULL DEFAULT '{sast,secret_detection,dependency_scanning,iac}',
    interval_hours integer NOT NULL DEFAULT 24 CHECK (interval_hours BETWEEN 1 AND 168),
    -- who turned it on: runs are theirs (they read the code as them)
    user_id        bigint  REFERENCES users (id) ON DELETE SET NULL,
    next_run_at    timestamptz,
    last_pipeline_id bigint REFERENCES ci_pipelines (id) ON DELETE SET NULL,
    -- the first run looks for secrets in the whole history
    history_scanned boolean NOT NULL DEFAULT false,
    updated_at     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX security_scanning_due_idx ON security_scanning_settings (next_run_at) WHERE enabled;

-- A group's default for its projects that never chose, and new ones.
CREATE TABLE security_scanning_group_defaults (
    namespace_id bigint  PRIMARY KEY REFERENCES namespaces (id) ON DELETE CASCADE,
    enabled      boolean NOT NULL
);

-- What administrators changed over the config file's `[security]`.
ALTER TABLE instance_settings ADD COLUMN security_scanning jsonb NOT NULL DEFAULT '{}';

-- Pipelines of the built-in scanners.
ALTER TABLE ci_pipelines DROP CONSTRAINT ci_pipelines_source_check;
ALTER TABLE ci_pipelines ADD CONSTRAINT ci_pipelines_source_check
    CHECK (source IN ('push', 'web', 'api', 'schedule', 'merge_request_event', 'trigger', 'parent_pipeline', 'pipeline', 'renovate', 'security'));

-- ==== mcp ====

-- ADR-0025: the built-in MCP server and its OAuth 2.1 sign-in.

-- When a token last called the MCP server (people see which of theirs did).
ALTER TABLE personal_access_tokens ADD COLUMN mcp_used_at timestamptz;
-- Administrators can turn the MCP server off.
ALTER TABLE instance_settings ADD COLUMN mcp_enabled boolean NOT NULL DEFAULT true;

-- Clients registered dynamically (RFC 7591): public clients, PKCE only.
CREATE TABLE oauth_clients (
    client_id     text   PRIMARY KEY,
    name          text   NOT NULL,
    redirect_uris text[] NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now()
);

-- One-time authorization codes (10 minutes).
CREATE TABLE oauth_codes (
    code_hash      bytea  PRIMARY KEY,
    client_id      text   NOT NULL REFERENCES oauth_clients (client_id) ON DELETE CASCADE,
    user_id        bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    redirect_uri   text   NOT NULL,
    code_challenge text   NOT NULL,
    scopes         text[] NOT NULL,
    expires_at     timestamptz NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now()
);

-- Refresh tokens: each use replaces it and its access token (rotation).
CREATE TABLE oauth_refresh_tokens (
    token_hash      bytea  PRIMARY KEY,
    client_id       text   NOT NULL REFERENCES oauth_clients (client_id) ON DELETE CASCADE,
    user_id         bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    scopes          text[] NOT NULL,
    access_token_id bigint REFERENCES personal_access_tokens (id) ON DELETE SET NULL,
    expires_at      timestamptz NOT NULL,
    created_at      timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX oauth_refresh_tokens_user_idx ON oauth_refresh_tokens (user_id);

-- ==== review_fixes ====

-- Review fixes (2026-11-12).

-- OAuth refresh tokens are marked used instead of deleted, so a second use
-- (a stolen copy) is recognised and ends the client's access.
ALTER TABLE oauth_refresh_tokens ADD COLUMN used_at timestamptz;

-- The last TOTP time step accepted per account: a code works once.
ALTER TABLE users ADD COLUMN totp_last_step bigint;

-- Built-in security scans run as a per-project bot (a Reporter), not as
-- whoever turned scanning on.
ALTER TABLE users DROP CONSTRAINT users_user_type_check;
ALTER TABLE users ADD CONSTRAINT users_user_type_check
    CHECK (user_type IN ('human', 'project_bot', 'group_bot', 'renovate_bot', 'security_bot'));

-- What listings (search, to-dos, work lists, contributions, "my projects")
-- may show of a viewer's projects: as `project_role`, a member without
-- two-factor authentication (not an administrator or a bot) sees nothing of
-- an organisation that requires it.
CREATE VIEW listed_project_authorizations AS
SELECT a.user_id, a.project_id, a.access_level
FROM project_authorizations a
JOIN users u ON u.id = a.user_id
JOIN projects p ON p.id = a.project_id
JOIN namespaces n ON n.id = p.namespace_id
JOIN namespaces root ON root.id = n.traversal_ids[1]
WHERE NOT root.require_two_factor OR u.totp_enabled OR u.is_admin OR u.user_type <> 'human';

-- Pipelines Forgust runs itself (built-in Renovate, security scans): listed
-- with a project's pipelines, never a ref's or a commit's status. One place
-- says which sources those are (inlined by the planner).
CREATE FUNCTION is_system_source(source text) RETURNS boolean
LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$ SELECT source IN ('renovate', 'security') $$;
