-- 0003: passkeys (SPEC §11.3) and OIDC signing keys (SPEC §11.8.6).
-- Released migrations MUST NEVER be edited (SPEC §7.2).

CREATE TABLE _passkeys (
    id            TEXT PRIMARY KEY,
    owner_type    TEXT NOT NULL CHECK (owner_type IN ('user', 'superuser')),
    collection_id TEXT,
    user_id       TEXT NOT NULL,
    credential_id TEXT NOT NULL UNIQUE,
    credential    TEXT NOT NULL CHECK (json_valid(credential)),
    name          TEXT NOT NULL,
    created       TEXT NOT NULL,
    last_used     TEXT
) STRICT;
CREATE INDEX _passkeys_user ON _passkeys (user_id);

CREATE TABLE _keys (
    kid          TEXT PRIMARY KEY,
    alg          TEXT NOT NULL CHECK (alg IN ('RS256', 'ES256')),
    private_enc  TEXT NOT NULL,
    public_jwk   TEXT NOT NULL CHECK (json_valid(public_jwk)),
    created      TEXT NOT NULL,
    activated_at TEXT,
    retired_at   TEXT,
    delete_after TEXT
) STRICT;
