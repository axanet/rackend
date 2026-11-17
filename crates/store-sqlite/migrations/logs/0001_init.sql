-- 0001_init: tables of logs.db (SPEC §6.6). logs.db has its own migration track.
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

CREATE TABLE _requests (
    id          INTEGER PRIMARY KEY,
    ts          TEXT NOT NULL,
    method      TEXT NOT NULL,
    route       TEXT,
    path        TEXT NOT NULL,
    status      INTEGER NOT NULL,
    duration_ms REAL NOT NULL,
    ip          TEXT,
    user_agent  TEXT,
    actor_type  TEXT,
    actor_id    TEXT,
    request_id  TEXT,
    bytes_in    INTEGER,
    bytes_out   INTEGER,
    error       TEXT
) STRICT;

CREATE INDEX _requests_ts ON _requests (ts);

CREATE TABLE _metric_points (
    ts     TEXT NOT NULL,
    series TEXT NOT NULL,
    value  REAL NOT NULL,
    PRIMARY KEY (series, ts)
) STRICT, WITHOUT ROWID;

INSERT INTO _meta (key, value) VALUES ('created_at', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
