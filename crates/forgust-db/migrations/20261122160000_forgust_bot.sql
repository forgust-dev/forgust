-- The Forgust bot (ADR-0026): one account of the instance, `forgust-bot`, that
-- carries out the commands people write in comments (`/label ~bug`), in the
-- projects that turned it on.

ALTER TABLE users DROP CONSTRAINT users_user_type_check;
ALTER TABLE users ADD CONSTRAINT users_user_type_check
    CHECK (user_type IN ('human', 'project_bot', 'group_bot', 'renovate_bot', 'security_bot', 'ai_review_bot', 'forgust_bot'));

-- a project with no row never chose (a group above may choose for it)
CREATE TABLE bot_settings (
    project_id  bigint  PRIMARY KEY REFERENCES projects (id) ON DELETE CASCADE,
    enabled     boolean NOT NULL DEFAULT false,
    -- who turned it on or off
    user_id     bigint  REFERENCES users (id) ON DELETE SET NULL,
    updated_at  timestamptz NOT NULL DEFAULT now()
);

-- a group turns the bot on for its projects (those that never chose, and new
-- ones)
CREATE TABLE bot_group_defaults (
    namespace_id  bigint PRIMARY KEY REFERENCES namespaces (id) ON DELETE CASCADE,
    enabled       boolean NOT NULL DEFAULT false,
    updated_at    timestamptz NOT NULL DEFAULT now()
);

-- `/hold`: a merge request that must not merge until `/unhold`
CREATE TABLE merge_request_holds (
    merge_request_id  bigint PRIMARY KEY REFERENCES merge_requests (id) ON DELETE CASCADE,
    -- who asked for it
    user_id           bigint REFERENCES users (id) ON DELETE SET NULL,
    created_at        timestamptz NOT NULL DEFAULT now()
);
