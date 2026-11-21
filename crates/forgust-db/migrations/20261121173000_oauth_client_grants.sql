-- When someone last let an OAuth client in (a code or a refresh token traded
-- for tokens). Anyone may register a client: one nobody ever let in is
-- forgotten after a day, one that was stays.
ALTER TABLE oauth_clients ADD COLUMN granted_at timestamptz;
UPDATE oauth_clients c SET granted_at = now()
 WHERE EXISTS (SELECT 1 FROM oauth_refresh_tokens r WHERE r.client_id = c.client_id);
