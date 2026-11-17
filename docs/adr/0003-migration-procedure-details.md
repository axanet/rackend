# 0003. Details of the migration procedure

- Status: accepted
- Date: 2026-11-17
- Spec: §7.3

## Context
§7.3 leaves a few details open.

## Decision
1. **Empty databases are not backed up.** When there are pending migrations but `data.db` has no tables yet (a brand-new instance), the pre-upgrade backup is skipped: there is nothing to protect, and empty backups would push real ones out of the 5-backup retention.
2. **Backup names use binary versions**: `pre-upgrade-<last_binary_version>-to-<binary version>-<YYYYMMDDThhmmssZ>.db`. Operators who want to go back look for the release they ran before. `unknown` is used when the database never recorded a binary version. Characters other than `[A-Za-z0-9.]` are replaced with `_`; colons are avoided for portability.
3. **`_meta.schema_version` is updated inside each migration's transaction**, together with its `_migrations` row, instead of once after all migrations (§7.3 step 8). Downgrade protection therefore holds even after a run that failed half way.
4. **`_meta.last_binary_version` is updated on every start** when it differs, also when no migration is pending, so the next backup name is accurate. Nothing is written before migrations run, so a failed run leaves `data.db` byte-identical.
5. **`logs.db` has its own `_meta` and `_migrations` tables** (its own track, §7.3) and no pre-upgrade backup.
6. **Checksums** are SHA-256 over `name + "\n" + sql`. Rust migration steps cannot be checksummed; `scripts/check-migrations-immutable.sh` (CI) rejects any change to migration files released under the latest tag.
7. A failed backup verification renames the backup to `*.failed-verification` so it never counts as a valid backup.

## Consequences
The procedure is covered by unit tests in `store-sqlite` (fresh DB, rerun, backup naming, downgrade, checksum mismatch, unknown migration, failing migration leaving data byte-identical, resume after partial failure, FK-off migrations, backup retention) and by e2e tests through the binary.
