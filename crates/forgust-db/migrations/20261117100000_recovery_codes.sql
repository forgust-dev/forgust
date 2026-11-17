-- One-time codes that stand in for the second factor (a lost authenticator).
-- Only their SHA-256 is kept: they are long and random.
CREATE TABLE recovery_codes (
    id          bigserial PRIMARY KEY,
    user_id     bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    code_hash   bytea  NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (user_id, code_hash)
);
