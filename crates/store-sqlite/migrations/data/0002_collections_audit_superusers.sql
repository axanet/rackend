-- 0002: collections, schema history, trash, audit trail (SPEC §6.5, §8.5, §14.1)
-- and the superuser slice of auth (ADR 0008).
-- Released migrations MUST NEVER be edited (SPEC §7.2).

CREATE TABLE _collections (
    id      TEXT PRIMARY KEY,
    name    TEXT NOT NULL UNIQUE,
    type    TEXT NOT NULL CHECK (type IN ('base', 'auth')),
    fields  TEXT NOT NULL CHECK (json_valid(fields)),
    indexes TEXT NOT NULL CHECK (json_valid(indexes)),
    rules   TEXT NOT NULL CHECK (json_valid(rules)),
    options TEXT NOT NULL CHECK (json_valid(options)),
    created TEXT NOT NULL,
    updated TEXT NOT NULL
) STRICT;

CREATE TABLE _schema_history (
    id            TEXT PRIMARY KEY,
    collection_id TEXT,
    action        TEXT NOT NULL,
    before        TEXT CHECK (before IS NULL OR json_valid(before)),
    after         TEXT CHECK (after IS NULL OR json_valid(after)),
    ddl           TEXT NOT NULL,
    actor         TEXT NOT NULL,
    ts            TEXT NOT NULL
) STRICT;
CREATE INDEX _schema_history_collection ON _schema_history (collection_id, ts);

CREATE TABLE _trash (
    id            TEXT PRIMARY KEY,
    kind          TEXT NOT NULL CHECK (kind IN ('field', 'collection')),
    collection_id TEXT NOT NULL,
    original_name TEXT NOT NULL,
    storage_name  TEXT NOT NULL,
    definition    TEXT NOT NULL CHECK (json_valid(definition)),
    deleted_at    TEXT NOT NULL,
    purge_after   TEXT NOT NULL,
    actor         TEXT NOT NULL
) STRICT;
CREATE INDEX _trash_purge_after ON _trash (purge_after);

CREATE TABLE _audit (
    id            TEXT PRIMARY KEY,
    ts            TEXT NOT NULL,
    actor_type    TEXT NOT NULL CHECK (actor_type IN ('superuser', 'user', 'client', 'system', 'anonymous')),
    actor_id      TEXT,
    actor_label   TEXT,
    action        TEXT NOT NULL,
    collection_id TEXT,
    record_id     TEXT,
    diff          TEXT CHECK (diff IS NULL OR json_valid(diff)),
    ip            TEXT,
    user_agent    TEXT,
    request_id    TEXT,
    meta          TEXT CHECK (meta IS NULL OR json_valid(meta))
) STRICT;
CREATE INDEX _audit_ts ON _audit (ts);
CREATE INDEX _audit_record ON _audit (collection_id, record_id, ts);
CREATE INDEX _audit_action ON _audit (action, ts);

CREATE TABLE _superusers (
    id            TEXT PRIMARY KEY,
    email         TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    created       TEXT NOT NULL,
    updated       TEXT NOT NULL
) STRICT;

CREATE TABLE _sessions (
    id                TEXT PRIMARY KEY,
    family_id         TEXT NOT NULL,
    subject_type      TEXT NOT NULL CHECK (subject_type IN ('user', 'superuser')),
    collection_id     TEXT,
    user_id           TEXT NOT NULL,
    client_id         TEXT,
    auth_method       TEXT NOT NULL,
    access_hash       TEXT NOT NULL,
    access_expires    TEXT NOT NULL,
    refresh_hash      TEXT,
    prev_refresh_hash TEXT,
    refresh_expires   TEXT,
    created           TEXT NOT NULL,
    last_seen         TEXT NOT NULL,
    revoked_at        TEXT,
    revoked_reason    TEXT,
    ip                TEXT,
    user_agent        TEXT
) STRICT;
CREATE INDEX _sessions_user ON _sessions (user_id);
CREATE INDEX _sessions_refresh ON _sessions (refresh_hash);
CREATE INDEX _sessions_prev_refresh ON _sessions (prev_refresh_hash);

CREATE TABLE _setup_tokens (
    hash    TEXT PRIMARY KEY,
    expires TEXT NOT NULL,
    used_at TEXT
) STRICT;
