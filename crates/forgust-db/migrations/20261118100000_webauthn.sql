-- Security keys (a second factor) and passkeys (a sign-in of their own), with
-- WebAuthn. A credential is bound to the relying party id (the host of
-- `server.external_url`) it was made for.

-- What authenticators know the account by: random, never its id or name.
ALTER TABLE users ADD COLUMN webauthn_handle uuid UNIQUE;
-- The account has security keys (kept with its credentials, in one transaction).
ALTER TABLE users ADD COLUMN security_keys_enabled boolean NOT NULL DEFAULT false;
-- A password alone does not sign in: the one definition queries use.
ALTER TABLE users ADD COLUMN two_factor_enabled boolean
    GENERATED ALWAYS AS (totp_enabled OR security_keys_enabled) STORED;

CREATE TABLE webauthn_credentials (
    id             bigserial PRIMARY KEY,
    user_id        bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind           text   NOT NULL CHECK (kind IN ('security_key', 'passkey')),
    credential_id  bytea  NOT NULL UNIQUE,
    -- webauthn-rs' SecurityKey or Passkey: public key, counter, flags
    credential     jsonb  NOT NULL,
    name           text   NOT NULL,
    rp_id          text   NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    last_used_at   timestamptz
);
CREATE INDEX webauthn_credentials_user_idx ON webauthn_credentials (user_id, kind);

-- the second step of a sign-in with a security key: what it was asked to sign
ALTER TABLE login_challenges ADD COLUMN webauthn_state jsonb;

-- Registrations (bound to the account and the session that started them) and
-- passkey sign-ins, between their two requests. Each is used once.
CREATE TABLE webauthn_ceremonies (
    token_hash  bytea  PRIMARY KEY,
    kind        text   NOT NULL CHECK (kind IN ('register_security_key', 'register_passkey', 'passkey_login')),
    user_id     bigint REFERENCES users (id) ON DELETE CASCADE,
    session_id  bigint REFERENCES sessions (id) ON DELETE CASCADE,
    state       jsonb  NOT NULL,
    expires_at  timestamptz NOT NULL
);
