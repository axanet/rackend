# rackend-blob-fs

Implements the core `BlobStore` port on the local filesystem under `<data dir>/storage`, with atomic writes via `tmp/` + fsync + rename (SPEC §12.2). Implemented in M3.
