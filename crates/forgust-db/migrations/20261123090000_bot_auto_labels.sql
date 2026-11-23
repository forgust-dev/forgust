-- The Forgust bot's labels on merge requests (ADR-0026): what its rules
-- (`[labels]` of `.forgust/bot.toml`) asked for last time. A label is put on
-- when a rule starts to ask for it and taken off when none does any more, so
-- one that a person removed meanwhile is not put back at the next push.
CREATE TABLE merge_request_auto_labels (
    merge_request_id  bigint NOT NULL REFERENCES merge_requests (id) ON DELETE CASCADE,
    name              text   NOT NULL,
    PRIMARY KEY (merge_request_id, name)
);
