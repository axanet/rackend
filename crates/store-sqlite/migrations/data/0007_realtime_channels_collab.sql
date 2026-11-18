-- 0007: realtime channels and collaborative editing (SPEC §13.3, §13.4).
-- Released migrations MUST NEVER be edited (SPEC §7.2).

-- Channels: `pattern` is a name (`lobby`) or a prefix with a key (`room:*`).
-- A null rule is locked (superusers only); an empty rule allows everyone.
CREATE TABLE _channels (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL UNIQUE,
    pattern      TEXT NOT NULL UNIQUE,
    join_rule    TEXT,
    publish_rule TEXT,
    presence     INTEGER NOT NULL DEFAULT 1,
    options      TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(options)),
    created      TEXT NOT NULL,
    updated      TEXT NOT NULL
) STRICT;

-- Yjs updates not yet merged into the record's `ydoc` column. Compaction
-- writes the merged state to the column and deletes the rows it merged.
CREATE TABLE _ydoc_updates (
    id            INTEGER PRIMARY KEY,
    collection_id TEXT NOT NULL,
    record_id     TEXT NOT NULL,
    field         TEXT NOT NULL,
    "update"      BLOB NOT NULL,
    created       TEXT NOT NULL
) STRICT;
CREATE INDEX _ydoc_updates_doc ON _ydoc_updates (collection_id, record_id, field, id);
