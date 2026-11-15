-- Administrators hear of new Forgust releases (the latest GitHub release),
-- unless they turn the check off.
ALTER TABLE instance_settings
    ADD COLUMN update_check_enabled boolean NOT NULL DEFAULT true,
    ADD COLUMN latest_version       text,
    ADD COLUMN latest_release_url   text;
