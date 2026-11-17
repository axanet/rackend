-- 0005: external identities, one-time tokens, OTPs and auth providers
-- (SPEC §11.4, §11.7). Released migrations MUST NEVER be edited (SPEC §7.2).

CREATE TABLE _identities (
    id            TEXT PRIMARY KEY,
    collection_id TEXT NOT NULL,
    user_id       TEXT NOT NULL,
    provider_id   TEXT NOT NULL,
    issuer        TEXT NOT NULL,
    subject       TEXT NOT NULL,
    email_at_link TEXT,
    created       TEXT NOT NULL,
    last_login    TEXT,
    UNIQUE (issuer, subject)
) STRICT;
CREATE INDEX _identities_user ON _identities (user_id);

-- Single-use tokens, stored as SHA-256. `exchange` codes carry a finished
-- provider login to a SPA (SPEC §11.7).
CREATE TABLE _verification_tokens (
    hash          TEXT PRIMARY KEY,
    purpose       TEXT NOT NULL CHECK (purpose IN ('verify_email', 'reset_password', 'change_email', 'magic_link', 'exchange')),
    collection_id TEXT NOT NULL,
    user_id       TEXT NOT NULL,
    payload       TEXT,
    expires       TEXT NOT NULL,
    used_at       TEXT
) STRICT;
CREATE INDEX _verification_tokens_user ON _verification_tokens (user_id, purpose);

CREATE TABLE _otps (
    id            TEXT PRIMARY KEY,
    collection_id TEXT NOT NULL,
    user_id       TEXT NOT NULL,
    code_hash     TEXT NOT NULL,
    purpose       TEXT NOT NULL,
    expires       TEXT NOT NULL,
    attempts      INTEGER NOT NULL DEFAULT 0,
    used_at       TEXT
) STRICT;
CREATE INDEX _otps_user ON _otps (user_id, purpose);

CREATE TABLE _auth_providers (
    id                       TEXT PRIMARY KEY,
    name                     TEXT NOT NULL UNIQUE,
    kind                     TEXT NOT NULL CHECK (kind IN ('oidc', 'oauth2_preset')),
    preset                   TEXT,
    issuer                   TEXT,
    client_id                TEXT NOT NULL,
    client_secret_enc        TEXT,
    scopes                   TEXT,
    collection_id            TEXT NOT NULL,
    auto_link_verified_email INTEGER NOT NULL DEFAULT 0,
    claim_mapping            TEXT,
    enabled                  INTEGER NOT NULL DEFAULT 1,
    created                  TEXT NOT NULL,
    updated                  TEXT NOT NULL
) STRICT;
