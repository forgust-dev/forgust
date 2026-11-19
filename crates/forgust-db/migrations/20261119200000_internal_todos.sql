-- A to-do or an inbox entry made by an internal comment: shown only while its
-- owner may still read internal notes (a Reporter demoted to Guest no longer
-- sees the comment's text in their to-dos, nor that it was written).

ALTER TABLE todos ADD COLUMN internal boolean NOT NULL DEFAULT false;
ALTER TABLE notifications ADD COLUMN internal boolean NOT NULL DEFAULT false;

-- `n.*` was expanded when the view was made: replaced, it gains `internal`.
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
  AND (i.id IS NULL OR issue_visible(i, me.id, me.is_admin))
  AND (NOT n.internal OR sees_internal(n.project_id, me.id, me.is_admin));
