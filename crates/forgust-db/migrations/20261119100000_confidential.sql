-- Confidential issues and internal notes: what only a project's members see.
-- A confidential issue: its Reporters and up, its author and its assignees.
-- An internal note (on an issue or a merge request): its Reporters and up.
-- Administrators see both.

ALTER TABLE issues ADD COLUMN confidential boolean NOT NULL DEFAULT false;
ALTER TABLE issue_notes ADD COLUMN internal boolean NOT NULL DEFAULT false;
ALTER TABLE notes ADD COLUMN internal boolean NOT NULL DEFAULT false;

-- Reporter and up on the project (as `project_role`: not a member blocked by
-- two-factor authentication), or an administrator. `uid` NULL: anonymous.
CREATE FUNCTION sees_internal(project bigint, uid bigint, admin boolean) RETURNS boolean
LANGUAGE sql STABLE PARALLEL SAFE AS $$
    SELECT admin OR EXISTS (SELECT 1 FROM listed_project_authorizations a
                            WHERE a.project_id = project AND a.user_id = uid AND a.access_level >= 20)
$$;

-- Who may see a confidential issue: its author, its Reporters and up, its assignees.
CREATE FUNCTION issue_member(issue bigint, project bigint, author bigint, uid bigint, admin boolean) RETURNS boolean
LANGUAGE sql STABLE PARALLEL SAFE AS $$
    SELECT coalesce(author = uid, false)
        OR sees_internal(project, uid, admin)
        OR EXISTS (SELECT 1 FROM issue_assignees x WHERE x.issue_id = issue AND x.user_id = uid)
$$;

-- The one rule for an issue, for every query that lists, counts or names
-- issues: not confidential, or the viewer may see it. Seeing the project is
-- checked apart. No subquery here, so the planner inlines it: a public issue
-- costs one column test, and only confidential ones call `issue_member`.
CREATE FUNCTION issue_visible(i issues, uid bigint, admin boolean) RETURNS boolean
LANGUAGE sql STABLE PARALLEL SAFE AS $$
    SELECT NOT i.confidential OR issue_member(i.id, i.project_id, i.author_id, uid, admin)
$$;

-- The inbox no longer shows a confidential issue to someone who lost sight of it.
CREATE OR REPLACE VIEW listed_notifications AS
SELECT n.*
FROM notifications n
JOIN projects p ON p.id = n.project_id
JOIN users me ON me.id = n.user_id
LEFT JOIN listed_project_authorizations a ON a.project_id = n.project_id AND a.user_id = n.user_id
LEFT JOIN issues i ON n.target_type = 'issue' AND i.project_id = n.project_id AND i.iid = n.target_iid
WHERE (p.visibility = 20
       OR me.is_admin
       OR a.access_level >= 20
       OR (a.access_level IS NOT NULL AND n.target_type = 'issue'))
  AND (i.id IS NULL OR issue_visible(i, me.id, me.is_admin));
