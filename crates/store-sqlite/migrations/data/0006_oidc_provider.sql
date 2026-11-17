-- 0006: the OIDC provider (SPEC §11.8): clients, authorization codes and
-- consents. Signing keys (_keys) exist since 0003.
-- Released migrations MUST NEVER be edited (SPEC §7.2).

CREATE TABLE _oauth_clients (
    id                        TEXT PRIMARY KEY,
    name                      TEXT NOT NULL,
    kind                      TEXT NOT NULL CHECK (kind IN ('public', 'confidential')),
    secret_hash               TEXT,
    redirect_uris             TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(redirect_uris)),
    post_logout_redirect_uris TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(post_logout_redirect_uris)),
    scopes                    TEXT NOT NULL DEFAULT 'openid profile email offline_access',
    collection_id             TEXT NOT NULL,
    first_party               INTEGER NOT NULL DEFAULT 0,
    dev                       INTEGER NOT NULL DEFAULT 0,
    id_token_alg              TEXT NOT NULL DEFAULT 'RS256' CHECK (id_token_alg IN ('RS256', 'ES256')),
    require_pkce              INTEGER NOT NULL DEFAULT 1,
    access_ttl_s              INTEGER NOT NULL DEFAULT 900,
    refresh_ttl_s             INTEGER NOT NULL DEFAULT 2592000,
    enabled                   INTEGER NOT NULL DEFAULT 1,
    created                   TEXT NOT NULL,
    updated                   TEXT NOT NULL
) STRICT;

-- Codes are stored as SHA-256, live 60 s and are single use. `session_id`
-- is the browser session that approved the request; `issued_session_id`
-- the session created by the exchange (revoked if the code is reused).
CREATE TABLE _oauth_codes (
    hash              TEXT PRIMARY KEY,
    client_id         TEXT NOT NULL,
    session_id        TEXT NOT NULL,
    redirect_uri      TEXT NOT NULL,
    scopes            TEXT NOT NULL,
    code_challenge    TEXT NOT NULL,
    nonce             TEXT,
    auth_time         TEXT NOT NULL,
    expires           TEXT NOT NULL,
    used_at           TEXT,
    issued_session_id TEXT
) STRICT;

CREATE TABLE _oauth_consents (
    user_id       TEXT NOT NULL,
    collection_id TEXT NOT NULL,
    client_id     TEXT NOT NULL,
    scopes        TEXT NOT NULL,
    granted_at    TEXT NOT NULL,
    PRIMARY KEY (collection_id, user_id, client_id)
) STRICT;

-- Scopes granted to an OIDC client session, for refreshes and userinfo.
ALTER TABLE _sessions ADD COLUMN scopes TEXT;
