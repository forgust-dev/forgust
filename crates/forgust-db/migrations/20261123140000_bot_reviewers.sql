-- The Forgust bot asks for reviews (ADR-0026, `[reviewers]` of
-- `.forgust/bot.toml`).

-- once per merge request: reviewers taken off afterwards are not replaced
CREATE TABLE merge_request_bot_reviews (
    merge_request_id  bigint PRIMARY KEY REFERENCES merge_requests (id) ON DELETE CASCADE,
    created_at        timestamptz NOT NULL DEFAULT now()
);

-- when the bot last chose each person in a project: the next turn goes to
-- whoever was asked longest ago
CREATE TABLE bot_review_turns (
    project_id  bigint NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    user_id     bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    picked_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, user_id)
);
