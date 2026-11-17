-- Code search: what the code index holds of each project. The index is a
-- directory on disk that can be rebuilt; `generation` says which index a
-- project was put into, so a row of another generation means "not indexed".
-- No foreign key: the row of a deleted project stays until the sweep has
-- removed that project's files from the index.
CREATE TABLE code_index_state (
    project_id      bigint PRIMARY KEY,
    generation      text   NOT NULL,
    -- head of the default branch when indexed (NULL: empty repository)
    indexed_commit  text,
    default_branch  text,
    indexed_at      timestamptz NOT NULL
);
