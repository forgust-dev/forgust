-- A provider sign-in started from the account's settings: its callback links
-- the provider identity to that account instead of signing anyone in.
ALTER TABLE oidc_states ADD COLUMN link_user_id bigint REFERENCES users (id) ON DELETE CASCADE;

-- What a provider sign-in would change on the account (administrator rights,
-- name, email): applied only once the second factor is given.
ALTER TABLE login_challenges ADD COLUMN provider_sync jsonb;

-- the providers of an account: its settings, the admin user list
CREATE INDEX identities_user_idx ON identities (user_id, provider);
