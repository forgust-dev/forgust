-- Audit rows reference people by id, never by name or address: a person's
-- erasure must leave nothing of them in `details` (the names are looked up
-- when the log is read). Earlier rows wrote usernames and addresses; drop
-- them, keeping the id where only the name was written.

-- a member's username: its id instead, when the account still exists
UPDATE audit_events a
   SET details = (a.details - 'user') || jsonb_build_object('user_id', u.id)
  FROM users u
 WHERE a.action = 'member_set' AND a.details ? 'user' AND NOT a.details ? 'user_id' AND u.username = a.details ->> 'user';

-- an LFS lock owner's username: its id instead, likewise
UPDATE audit_events a
   SET details = (a.details - 'owner') || jsonb_build_object('user_id', u.id)
  FROM users u
 WHERE a.action = 'lfs_lock_removed' AND a.details ? 'owner' AND u.username = a.details ->> 'owner';

-- what is left: names and addresses, whoever they were
UPDATE audit_events SET details = details - 'user' WHERE action = 'member_set' AND details ? 'user';
UPDATE audit_events SET details = details - 'owner' WHERE action = 'lfs_lock_removed' AND details ? 'owner';
UPDATE audit_events SET details = details - 'username'
 WHERE action IN ('impersonation_started', 'user_created', 'user_deleted') AND details ? 'username';
UPDATE audit_events SET details = details - 'email'
 WHERE action IN ('user_deleted', 'email_changed', 'email_confirmed') AND details ? 'email';
UPDATE audit_events SET details = details - 'from' WHERE action = 'email_changed' AND details ? 'from';
UPDATE audit_events SET details = details - 'to' WHERE action = 'test_email_sent' AND details ? 'to';
UPDATE audit_events SET details = details - 'by' WHERE action = 'security_scanning_settings' AND details ? 'by';
