-- The Smart View's history: what each commit of a project's default branch changed to its
-- map (a block or a link that came or went, a block renamed or turned into another kind), as
-- the map job tells it (`assay report --history`). Each job reads the commits after the last
-- one read; one that cannot (the branch was pushed over, or it is another branch) reads the
-- whole history again, and its chapters take the place of these.
CREATE TABLE smart_view_history (
    project_id  bigint PRIMARY KEY REFERENCES projects (id) ON DELETE CASCADE,
    -- the default branch it is of
    git_ref     text NOT NULL,
    -- the last commit read: the next job reads those after it
    head        text NOT NULL,
    -- where it starts when it does not reach the first commit (more maps than a job makes)
    start_sha   text,
    start_at    timestamptz,
    updated_at  timestamptz NOT NULL DEFAULT now()
);

-- one per commit that changed the map, oldest first by id; what a merge brought, one per
-- commit of its branch that made it
CREATE TABLE smart_view_chapters (
    id            bigserial PRIMARY KEY,
    project_id    bigint NOT NULL REFERENCES smart_view_history (project_id) ON DELETE CASCADE,
    -- the commit that made the changes, and the default branch's merge that brought them when
    -- it was made on the branch merged
    sha           text NOT NULL,
    merge_sha     text,
    author_name   text NOT NULL,
    author_email  text NOT NULL,
    -- when the changes reached the default branch
    at            timestamptz NOT NULL,
    title         text NOT NULL,
    -- assay's `Changes`
    changes       jsonb NOT NULL
);
CREATE INDEX smart_view_chapters_project_idx ON smart_view_chapters (project_id, id);
