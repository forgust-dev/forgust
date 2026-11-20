-- What an import wrote for a source user with no account here (matched by
-- email): an issue, a comment, a merge request or who merged it. When an
-- account later gets that address, confirmed, it becomes theirs
-- (link_imported_authors); until then the row shows `imported_author`.
CREATE TABLE import_authored (
    import_id       bigint NOT NULL,
    source_user_id  text   NOT NULL,
    kind            text   NOT NULL CHECK (kind IN ('issue', 'issue_note', 'merge_request', 'merged_by', 'note')),
    -- the issue's, issue note's, merge request's or MR note's id
    local_id        bigint NOT NULL,
    PRIMARY KEY (kind, local_id),
    FOREIGN KEY (import_id, source_user_id) REFERENCES import_users (import_id, source_user_id) ON DELETE CASCADE
);
CREATE INDEX import_authored_user_idx ON import_authored (import_id, source_user_id);

-- Earlier imports recorded only the name shown: found again by it, within
-- the projects of the same import ("Name (@username)", or "@username").
-- Who merged was not recorded at all.
WITH people AS (
    SELECT iu.import_id, iu.source_user_id,
           CASE WHEN iu.name = '' OR iu.name = iu.username THEN '@' || iu.username
                ELSE iu.name || ' (@' || iu.username || ')' END AS shown,
           ip.project_id
      FROM import_users iu
      JOIN import_projects ip ON ip.import_id = iu.import_id AND ip.project_id IS NOT NULL
     WHERE iu.user_id IS NULL
)
INSERT INTO import_authored (import_id, source_user_id, kind, local_id)
SELECT p.import_id, p.source_user_id, x.kind, x.id
  FROM people p
  JOIN LATERAL (
        SELECT 'issue' AS kind, i.id FROM issues i
         WHERE i.project_id = p.project_id AND i.author_id IS NULL AND i.imported_author = p.shown
        UNION ALL
        SELECT 'issue_note', n.id FROM issue_notes n JOIN issues i ON i.id = n.issue_id
         WHERE i.project_id = p.project_id AND n.author_id IS NULL AND n.imported_author = p.shown
        UNION ALL
        SELECT 'merge_request', m.id FROM merge_requests m
         WHERE m.project_id = p.project_id AND m.author_id IS NULL AND m.imported_author = p.shown
        UNION ALL
        SELECT 'note', n.id FROM notes n JOIN merge_requests m ON m.id = n.merge_request_id
         WHERE m.project_id = p.project_id AND n.author_id IS NULL AND n.imported_author = p.shown
       ) x ON true
ON CONFLICT DO NOTHING;

-- Give `uid` what imports wrote for the source users with their address:
-- only a confirmed one (anyone can type an address), of an active person.
-- A source user matched to another account stays theirs.
CREATE FUNCTION link_imported_authors(uid bigint) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    addr text;
    issue_ids bigint[];
    issue_note_ids bigint[];
    mr_ids bigint[];
    merged_ids bigint[];
    note_ids bigint[];
BEGIN
    SELECT email INTO addr FROM users
     WHERE id = uid AND email_verified_at IS NOT NULL AND state = 'active' AND user_type = 'human';
    IF addr IS NULL THEN
        RETURN;
    END IF;
    WITH matched AS (
        UPDATE import_users SET user_id = uid
         WHERE lower(email) = lower(addr) AND (user_id IS NULL OR user_id = uid)
        RETURNING import_id, source_user_id
    ), done AS (
        DELETE FROM import_authored a USING matched m
         WHERE a.import_id = m.import_id AND a.source_user_id = m.source_user_id
        RETURNING a.kind, a.local_id
    )
    SELECT array_agg(local_id) FILTER (WHERE kind = 'issue'),
           array_agg(local_id) FILTER (WHERE kind = 'issue_note'),
           array_agg(local_id) FILTER (WHERE kind = 'merge_request'),
           array_agg(local_id) FILTER (WHERE kind = 'merged_by'),
           array_agg(local_id) FILTER (WHERE kind = 'note')
      INTO issue_ids, issue_note_ids, mr_ids, merged_ids, note_ids
      FROM done;
    UPDATE issues SET author_id = uid, imported_author = NULL WHERE id = ANY (issue_ids) AND author_id IS NULL;
    UPDATE issue_notes SET author_id = uid, imported_author = NULL WHERE id = ANY (issue_note_ids) AND author_id IS NULL;
    UPDATE merge_requests SET author_id = uid, imported_author = NULL WHERE id = ANY (mr_ids) AND author_id IS NULL;
    UPDATE merge_requests SET merged_by = uid WHERE id = ANY (merged_ids) AND merged_by IS NULL;
    UPDATE notes SET author_id = uid, imported_author = NULL WHERE id = ANY (note_ids) AND author_id IS NULL;
END $$;

-- Every way an address becomes confirmed (its link, a password reset, an
-- administrator, a single sign-on provider, an account created confirmed),
-- and a blocked account let back in.
CREATE FUNCTION users_link_imported() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.email_verified_at IS NOT NULL AND NEW.state = 'active'
       AND (TG_OP = 'INSERT' OR OLD.email_verified_at IS NULL OR lower(OLD.email) <> lower(NEW.email) OR OLD.state <> NEW.state) THEN
        PERFORM link_imported_authors(NEW.id);
    END IF;
    RETURN NULL;
END $$;
CREATE TRIGGER users_link_imported AFTER INSERT OR UPDATE OF email, email_verified_at, state ON users
    FOR EACH ROW EXECUTE FUNCTION users_link_imported();

-- Who already has a confirmed address a source user had: theirs now.
SELECT link_imported_authors(u.id)
  FROM users u
 WHERE u.email_verified_at IS NOT NULL AND u.state = 'active' AND u.user_type = 'human'
   AND EXISTS (SELECT 1 FROM import_users iu WHERE lower(iu.email) = lower(u.email) AND iu.user_id IS NULL);
