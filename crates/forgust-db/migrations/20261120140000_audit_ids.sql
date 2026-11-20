-- Audit rows reference people by id, never by name or address: a person's
-- erasure must leave nothing of them in `details` (the names are looked up
-- when the log is read). Earlier rows wrote usernames and addresses: they
-- go. No id is guessed from a username: it may have been renamed since, and
-- taken by someone else.

UPDATE audit_events SET details = details - 'user' WHERE action = 'member_set' AND details ? 'user';
UPDATE audit_events SET details = details - 'owner' WHERE action = 'lfs_lock_removed' AND details ? 'owner';
UPDATE audit_events SET details = details - 'username'
 WHERE action IN ('impersonation_started', 'user_created', 'user_deleted') AND details ? 'username';
UPDATE audit_events SET details = details - 'email'
 WHERE action IN ('user_deleted', 'email_changed', 'email_confirmed') AND details ? 'email';
UPDATE audit_events SET details = details - 'from' WHERE action = 'email_changed' AND details ? 'from';
UPDATE audit_events SET details = details - 'to' WHERE action = 'test_email_sent' AND details ? 'to';
UPDATE audit_events SET details = details - 'by' WHERE action = 'security_scanning_settings' AND details ? 'by';
