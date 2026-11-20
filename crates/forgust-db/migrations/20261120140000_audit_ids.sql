-- Audit rows reference people by id, never by name or address: a person's
-- erasure must leave nothing of them in `details` (the names are looked up
-- when the log is read). Earlier rows wrote usernames and addresses: they
-- go. No id is guessed from a username: it may have been renamed since, and
-- taken by someone else.
UPDATE audit_events
   SET details = details - CASE action
           WHEN 'member_set' THEN ARRAY['user']
           WHEN 'lfs_lock_removed' THEN ARRAY['owner']
           WHEN 'impersonation_started' THEN ARRAY['username']
           WHEN 'user_created' THEN ARRAY['username']
           WHEN 'user_deleted' THEN ARRAY['username', 'email']
           WHEN 'email_changed' THEN ARRAY['from', 'email']
           WHEN 'email_confirmed' THEN ARRAY['email']
           WHEN 'test_email_sent' THEN ARRAY['to']
           WHEN 'security_scanning_settings' THEN ARRAY['by']
       END
 WHERE action IN ('member_set', 'lfs_lock_removed', 'impersonation_started', 'user_created', 'user_deleted',
                  'email_changed', 'email_confirmed', 'test_email_sent', 'security_scanning_settings');
