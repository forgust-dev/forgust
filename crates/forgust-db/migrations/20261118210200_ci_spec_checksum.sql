-- A job definition's key (ci_job_definitions.checksum): sha256 of the spec as
-- jsonb prints it. One definition, so that adding, finding and moving a spec
-- always agree on it. IMMUTABLE by hand: convert_to is only STABLE, but to
-- UTF8 it does not depend on any setting.
CREATE FUNCTION ci_spec_checksum(spec jsonb) RETURNS bytea
    LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE
    AS $$ SELECT sha256(convert_to(spec::text, 'UTF8')) $$;
