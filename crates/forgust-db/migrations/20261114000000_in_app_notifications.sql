-- In-app notifications: an inbox per person, and a switch per kind and channel.

-- What a notification is about (`forgust_db::notifications::Kind`).
CREATE DOMAIN notification_kind AS text
    CHECK (VALUE IN ('mention', 'assignment', 'activity', 'review', 'pipeline_failed', 'pipeline_fixed', 'security'));

-- What reached someone in the app (the bell). The level (watch, participating,
-- mention, disabled) decides who hears about something; the channels below decide how.
CREATE TABLE notifications (
    id          bigserial PRIMARY KEY,
    user_id     bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    project_id  bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    kind        notification_kind NOT NULL,
    -- the thing and its number in the project; `security`: the project's findings (no number)
    target_type text   NOT NULL CHECK (target_type IN ('merge_request', 'issue', 'pipeline', 'security')),
    target_iid  bigint CHECK ((target_iid IS NULL) = (target_type = 'security')),
    actor_id    bigint REFERENCES users (id) ON DELETE SET NULL,
    -- what it is about ("Fix the login page", "Pipeline #12 on main")
    title       text   NOT NULL,
    -- what happened ("approved", "failed")
    summary     text   NOT NULL,
    read_at     timestamptz,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX notifications_user_idx ON notifications (user_id, created_at DESC, id DESC);
CREATE INDEX notifications_unread_idx ON notifications (user_id) WHERE read_at IS NULL;
-- what was read a while ago, or never read for a long time, is removed (housekeeping)
CREATE INDEX notifications_read_idx ON notifications (read_at) WHERE read_at IS NOT NULL;
CREATE INDEX notifications_created_idx ON notifications (created_at);

-- What an inbox may show: what its owner can still see. A member sees a
-- project's issues, and its merge requests, pipelines and findings (code) from
-- Reporter; anyone sees a public project's, and an administrator everything.
CREATE VIEW listed_notifications AS
SELECT n.*
FROM notifications n
JOIN projects p ON p.id = n.project_id
JOIN users me ON me.id = n.user_id
LEFT JOIN listed_project_authorizations a ON a.project_id = n.project_id AND a.user_id = n.user_id
WHERE p.visibility = 20
   OR me.is_admin
   OR a.access_level >= 20
   OR (a.access_level IS NOT NULL AND n.target_type = 'issue');

-- A person's choice for a kind on a channel; no row: the default
-- (`forgust_db::notifications::default_on`). `web` is the bell, `screen` the
-- message that shows as it arrives while the app is open.
CREATE TABLE notification_channels (
    user_id    bigint  NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind       notification_kind NOT NULL,
    channel    text    NOT NULL CHECK (channel IN ('web', 'email', 'screen')),
    enabled    boolean NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, kind, channel)
);

-- How a pipeline ended the last time its people were told (`success` or `failed`):
-- a job retried or played makes it end again, which is said once per outcome.
ALTER TABLE ci_pipelines ADD COLUMN notified_status text CHECK (notified_status IN ('success', 'failed'));
