-- More addresses per account: commits, imported people and signed commits
-- match any confirmed one, like the account's own. A confirmed address
-- belongs to one account, as its own or as an extra one; an unconfirmed one
-- reserves nothing (anyone could list someone else's address and lock them
-- out): several accounts may list it until one of them confirms it.
CREATE TABLE user_emails (
    id            bigserial   PRIMARY KEY,
    user_id       bigint      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    email         text        NOT NULL,
    confirmed_at  timestamptz,
    -- link: opened from its own inbox; admin: an administrator vouched for
    -- it; auto: the instance trusted it (no email, sign-up closed)
    confirmed_by  text        CHECK (confirmed_by IN ('link', 'admin', 'auto')),
    created_at    timestamptz NOT NULL DEFAULT now(),
    CHECK ((confirmed_at IS NULL) = (confirmed_by IS NULL))
);
CREATE UNIQUE INDEX user_emails_email_idx ON user_emails (lower(email)) WHERE confirmed_at IS NOT NULL;
CREATE UNIQUE INDEX user_emails_user_email_idx ON user_emails (user_id, lower(email));
-- the listings of an address, confirmed or not (dropped when someone takes it)
CREATE INDEX user_emails_lower_email_idx ON user_emails (lower(email));

-- Links for an extra address (`email`: which one) go with its listing.
ALTER TABLE user_tokens DROP CONSTRAINT user_tokens_purpose_check;
ALTER TABLE user_tokens ADD CONSTRAINT user_tokens_purpose_check CHECK (purpose IN ('password_reset', 'email', 'extra_email'));
CREATE FUNCTION user_emails_drop_links() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM user_tokens WHERE user_id = OLD.user_id AND purpose = 'extra_email' AND lower(email) = lower(OLD.email);
    RETURN NULL;
END $$;
CREATE TRIGGER user_emails_drop_links AFTER DELETE ON user_emails
    FOR EACH ROW EXECUTE FUNCTION user_emails_drop_links();

-- An account's own address is nobody's extra one (theirs included: it is
-- their address already), confirmed: refused; unconfirmed: dropped. The lock
-- on the address makes two saves of it at once wait for each other.
CREATE FUNCTION users_email_reserved() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND lower(NEW.email) = lower(OLD.email) THEN
        RETURN NEW;
    END IF;
    PERFORM pg_advisory_xact_lock(hashtextextended('email:' || lower(NEW.email), 0));
    IF EXISTS (SELECT 1 FROM user_emails WHERE lower(email) = lower(NEW.email) AND confirmed_at IS NOT NULL) THEN
        RAISE EXCEPTION 'email is already in use' USING ERRCODE = 'unique_violation';
    END IF;
    DELETE FROM user_emails WHERE lower(email) = lower(NEW.email);
    RETURN NEW;
END $$;
CREATE TRIGGER users_email_reserved BEFORE INSERT OR UPDATE OF email ON users
    FOR EACH ROW EXECUTE FUNCTION users_email_reserved();

-- An extra address is nobody's own one, and confirmed by one account at
-- most (user_emails_email_idx): confirming it drops the other accounts'
-- unconfirmed listings of it.
CREATE FUNCTION user_emails_reserved() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended('email:' || lower(NEW.email), 0));
    IF EXISTS (SELECT 1 FROM users WHERE lower(email) = lower(NEW.email)) THEN
        RAISE EXCEPTION 'email is already in use' USING ERRCODE = 'unique_violation';
    END IF;
    IF NEW.confirmed_at IS NOT NULL THEN
        DELETE FROM user_emails WHERE lower(email) = lower(NEW.email) AND user_id <> NEW.user_id AND confirmed_at IS NULL;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER user_emails_reserved BEFORE INSERT OR UPDATE OF email, confirmed_at ON user_emails
    FOR EACH ROW EXECUTE FUNCTION user_emails_reserved();

-- Without email, the addresses people add are confirmed as they add them
-- (only while sign-up is closed: see services/emails.rs).
ALTER TABLE instance_settings ADD COLUMN auto_confirm_emails boolean NOT NULL DEFAULT false;

-- Every confirmed address of an account, lowercased.
CREATE VIEW confirmed_emails AS
SELECT id AS user_id, lower(email) AS email FROM users WHERE email_verified_at IS NOT NULL
UNION ALL
SELECT user_id, lower(email) FROM user_emails WHERE confirmed_at IS NOT NULL;

-- As before (20261120170000_import_authors.sql), for each confirmed address
-- of `uid`, extra ones included.
CREATE OR REPLACE FUNCTION link_imported_authors(uid bigint) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    addrs text[];
    issue_ids bigint[];
    issue_note_ids bigint[];
    mr_ids bigint[];
    merged_ids bigint[];
    note_ids bigint[];
BEGIN
    SELECT array_agg(c.email) INTO addrs
      FROM confirmed_emails c JOIN users u ON u.id = c.user_id
     WHERE u.id = uid AND u.state = 'active' AND u.user_type = 'human';
    IF addrs IS NULL THEN
        RETURN;
    END IF;
    WITH matched AS (
        UPDATE import_users SET user_id = uid
         WHERE lower(email) = ANY (addrs) AND (user_id IS NULL OR user_id = uid)
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

-- An extra address confirmed (or added confirmed): what imports wrote for it.
CREATE FUNCTION user_emails_link_imported() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.confirmed_at IS NOT NULL AND (TG_OP = 'INSERT' OR OLD.confirmed_at IS NULL) THEN
        PERFORM link_imported_authors(NEW.user_id);
    END IF;
    RETURN NULL;
END $$;
CREATE TRIGGER user_emails_link_imported AFTER INSERT OR UPDATE OF confirmed_at ON user_emails
    FOR EACH ROW EXECUTE FUNCTION user_emails_link_imported();
