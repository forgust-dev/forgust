-- A job's definition (its spec: scripts, variables, rules, cache...) is the
-- same in every pipeline of a CI config that did not change: it is now
-- stored once per project and content, and jobs point to it. In a
-- development database 149 jobs held 33 distinct definitions (158 KB, 36 KB
-- stored once each); the spec made 84% of a ci_builds row, and TOAST does not
-- compress a row under about 2 KB.
CREATE TABLE ci_job_definitions (
    id         bigserial PRIMARY KEY,
    project_id bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    -- sha256 of the spec as jsonb prints it (keys in jsonb's order), so equal
    -- specs match whatever order they were written in
    checksum   bytea  NOT NULL,
    spec       jsonb  NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id, checksum)
);

-- A new job keeps NULL in spec and points to its definition. Jobs made
-- before keep their spec until the CI sweep moves it (ci::move_inline_specs).
-- No row is rewritten here; NOT VALID: the rows there, which all have a spec,
-- are not scanned for the constraints.
ALTER TABLE ci_builds ADD COLUMN definition_id bigint;
ALTER TABLE ci_builds ALTER COLUMN spec DROP NOT NULL;
ALTER TABLE ci_builds ADD CONSTRAINT ci_builds_definition_id_fkey
    FOREIGN KEY (definition_id) REFERENCES ci_job_definitions (id) NOT VALID;
ALTER TABLE ci_builds ADD CONSTRAINT ci_builds_spec_or_definition
    CHECK (spec IS NOT NULL OR definition_id IS NOT NULL) NOT VALID;
-- what the foreign key looks up when a project's definitions go with it
CREATE INDEX ci_builds_definition_idx ON ci_builds (definition_id) WHERE definition_id IS NOT NULL;
-- the jobs whose spec is still to move: none once the sweep is through
CREATE INDEX ci_builds_inline_spec_idx ON ci_builds (id) WHERE spec IS NOT NULL;
