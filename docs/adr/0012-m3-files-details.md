# 0012. M3 files details not fixed by the spec

- Status: accepted
- Date: 2026-11-17
- Spec: §8.2, §10.1, §12, §15, §20 M3

## Context

M3 adds the `file` field, uploads, serving, thumbnails and S3 storage. The spec fixes the pipeline and the guarantees; this record covers the choices it leaves open, the dependency changes and one deviation in the acceptance setup.

## Decision

### Ports

- `BlobStore` and `ImageProcessor` live in `core::ports`. Like `Store`, `BlobStore` returns boxed futures so it stays object safe (ADR 0001). Streams are chunk readers (`BlobChunks`), which keeps `futures` out of the core.
- `BlobStore::put` is split into `put_file` (uploads, streamed from the temp file) and `put_bytes` (thumbnails).
- The backend can change at runtime, so the core builds stores through a `BlobStoreFactory` and rebuilds after a `storage` settings change. If the configured backend cannot be built, every file call fails with `503 unavailable` instead of falling back to another backend silently.

### Uploads and records

- Multipart bodies: the JSON part is `@json` (also when a browser sends it as a Blob with a filename); other text parts set fields by name; file parts are named after their field.
- On create, file fields are set only by uploads. On update, a file field may keep a subset of its stored files (set it, or remove with `field-`); uploads are appended (multi) or replace the file (single). Naming a file that is not stored on the record is `400 unknown_file`, so clients cannot point records at other records' blobs.
- Batch requests cannot upload. JSON in a batch follows the same rules (no new names on create, subsets on update).
- The record id is fixed before the blobs are written (generated when the client sends none) because storage keys contain it.
- Stored filenames are `{sanitized base}_{8 random [a-z0-9]}.{ext}`; the extension comes from the sniffed type when known. Field values are these names.
- Images: dimensions are probed from the header first; above 50 MP the upload is `400 too_many_pixels` before any pixel is decoded. GIFs are stored as uploaded (re-encoding would drop animation; they carry no EXIF). `strip_metadata` defaults to true.
- A refused type is `415 unsupported_media_type` with per-field `errors`; oversize is `413` with `max_bytes`. Multipart bodies are limited to the sum of all file fields' limits plus 1 MB, enforced while streaming.
- `_files.field` stores the field id, so renames keep files.

### Deletion

- The `_files` table is the outbox: removed files get `deleted_at` inside the record transaction (updates, deletes including cascades, purged fields and collections); after commit a background task deletes the blobs and thumbnails, then the rows. Trashed fields and collections keep their files until they are purged.
- If the record transaction fails, the blobs written for it are deleted right away. The daily GC (also at start) deletes blobs without a `_files` row older than one hour, thumbnails of missing files, and any leftover marked rows.
- The thumbnail cache GC deletes the oldest thumbnails first (blob stores do not report access times).

### Serving

- `public` caching (`Cache-Control: public, max-age=86400`) only when the view rule is `""` and the field is not protected; otherwise `private, max-age=3600`.
- File responses carry `Content-Security-Policy: default-src 'none'; ...; sandbox`, except PDFs (browser PDF viewers do not run in a sandbox, and PDFs cannot run script in our origin). SVG and HTML are always attachments.
- A protected file needs `?token=` even when a bearer or cookie is present. The token is `base64url(payload).base64url(HMAC-SHA256)`, with a key derived from the secrets key, so rotating that key invalidates outstanding tokens (two minutes at most). The token stands in for its user when evaluating the view rule, which makes plain `<img src>` work.
- Ranges: one `bytes=` range is honored; several ranges get the whole file (allowed by RFC 9110). Out-of-range requests get the new problem code `range_not_satisfiable` (416) with `Content-Range: bytes */size`.
- `GET /api/files/usage` reports stored files and bytes for the admin UI.

### Settings

- `storage` is writable now: `backend` (`fs`|`s3`), `s3.{endpoint, region, bucket, prefix, access_key_id, secret_access_key, force_path_style, allow_http, redirect_downloads}`, `thumb_cache_max_mb`. Nested objects are type-checked and merged key by key.
- Secrets (`storage.s3.secret_access_key`) are stored encrypted, returned as `••••` (or `null` when unset), and sending `••••` back keeps them. Audit diffs never contain secrets or their ciphertext; they record `{"changed": true}` instead.
- S3 requests retry at most three times within 15 seconds, so a dead endpoint fails fast.

### Dependencies

- New: `object_store` (aws), `image`, `fast_image_resize`, `webp` (statically linked libwebp), `kamadak-exif` (tests), `testcontainers` (tests), plus `futures-util` and `bytes` for body streams; both were already in the tree through axum.
- `object_store`'s S3 client uses rustls with aws-lc. Their licenses add ISC, MIT-0 and CDLA-Permissive-2.0 (the root certificates) to the allowlist; all permissive.
- `cargo deny` reports unmaintained crates only for direct dependencies (`unmaintained = "workspace"`): chrono and humantime come with `object_store`, a tar crate with `testcontainers`, and we cannot replace them. Vulnerabilities still fail for every crate.
- Bundled C code (libwebp, aws-lc) needs third-party notices in release artifacts; that belongs to the release checklist together with the project license (SPEC §21).

### S3 contract tests

- The spec asks for MinIO and Garage. MinIO no longer publishes images that can be pulled anonymously (Docker Hub denies the pull, quay.io asks for a login), so the suite runs against **Garage and RustFS 1.0**, and against MinIO when `RACKEND_TEST_MINIO_IMAGE` names an image the machine can pull. The same contract runs against the filesystem backend in the default test run.
- Update (M7, owner decision): RustFS is the reference S3 server for the contract tests; Garage runs with `RACKEND_TEST_GARAGE=1`; MinIO is no longer tested (no longer maintained as open source).

## Consequences

- Clients upload with one multipart request and get the stored names back; a download URL is `/api/files/{collection}/{id}/{name}`.
- Moving between backends stays manual until a `storage migrate` command exists.
