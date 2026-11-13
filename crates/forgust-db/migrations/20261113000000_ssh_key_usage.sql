-- What an SSH key is for, as in GitLab's `usage_type`: signing in over SSH
-- (`auth`), verifying signed commits (`signing`), or both. Existing personal
-- keys did both; deploy keys only sign in (the CHECK holds them to it).
ALTER TABLE ssh_keys ADD COLUMN usage_type text NOT NULL DEFAULT 'auth_and_signing'
    CHECK (usage_type IN ('auth', 'signing', 'auth_and_signing'));
UPDATE ssh_keys SET usage_type = 'auth' WHERE kind = 'deploy';
ALTER TABLE ssh_keys ADD CONSTRAINT ssh_keys_deploy_keys_only_sign_in
    CHECK (kind <> 'deploy' OR usage_type = 'auth');
