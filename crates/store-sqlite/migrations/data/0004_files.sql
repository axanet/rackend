-- 0004: uploaded files (SPEC §12.3). A row exists for every stored blob;
-- `deleted_at` marks blobs to delete after commit (outbox, SPEC §4.4).
-- Released migrations MUST NEVER be edited (SPEC §7.2).

CREATE TABLE _files (
    id            TEXT PRIMARY KEY,
    collection_id TEXT NOT NULL,
    record_id     TEXT NOT NULL,
    field         TEXT NOT NULL,
    storage_key   TEXT NOT NULL UNIQUE,
    original_name TEXT NOT NULL,
    size          INTEGER NOT NULL CHECK (size >= 0),
    mime          TEXT NOT NULL,
    width         INTEGER,
    height        INTEGER,
    sha256        TEXT NOT NULL,
    created       TEXT NOT NULL,
    deleted_at    TEXT
) STRICT;
CREATE INDEX _files_record ON _files (record_id);
CREATE INDEX _files_collection ON _files (collection_id, field);
CREATE INDEX _files_deleted ON _files (deleted_at) WHERE deleted_at IS NOT NULL;
