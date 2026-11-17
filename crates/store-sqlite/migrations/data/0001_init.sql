-- 0001_init: bookkeeping tables of data.db (SPEC §6.5).
-- Released migrations MUST NEVER be edited (SPEC §7.2).

CREATE TABLE _meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;

CREATE TABLE _migrations (
    id             INTEGER PRIMARY KEY,
    name           TEXT NOT NULL,
    checksum       TEXT NOT NULL,
    applied_at     TEXT NOT NULL,
    binary_version TEXT NOT NULL
) STRICT;

CREATE TABLE _settings (
    key       TEXT PRIMARY KEY,
    value     TEXT NOT NULL CHECK (json_valid(value)),
    is_secret INTEGER NOT NULL DEFAULT 0 CHECK (is_secret IN (0, 1)),
    updated   TEXT NOT NULL
) STRICT;

INSERT INTO _meta (key, value) VALUES ('created_at', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
