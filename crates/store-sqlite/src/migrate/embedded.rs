//! The migrations embedded in this binary. Append only: a released migration
//! MUST NEVER be edited (SPEC §7.2); CI checks this against the last release tag.

use super::Migration;

/// Migrations of `data.db`.
pub static DATA_MIGRATIONS: &[Migration] = &[
    Migration {
        id: 1,
        name: "init",
        sql: include_str!("../../migrations/data/0001_init.sql"),
        rust: None,
        foreign_keys_off: false,
    },
    Migration {
        id: 2,
        name: "collections_audit_superusers",
        sql: include_str!("../../migrations/data/0002_collections_audit_superusers.sql"),
        rust: None,
        foreign_keys_off: false,
    },
];

/// Migrations of `logs.db` (its own track, SPEC §7.3).
pub static LOGS_MIGRATIONS: &[Migration] = &[Migration {
    id: 1,
    name: "init",
    sql: include_str!("../../migrations/logs/0001_init.sql"),
    rust: None,
    foreign_keys_off: false,
}];

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Every SQL file is embedded under its number and name, ids are 1..=n.
    fn check_track(dir: &str, migrations: &[Migration]) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("migrations")
            .join(dir);
        let mut files: Vec<String> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".sql"))
            .collect();
        files.sort();
        assert_eq!(
            files.len(),
            migrations.len(),
            "every file in {dir:?} must be embedded"
        );
        for (i, (file, m)) in files.iter().zip(migrations).enumerate() {
            let expected_id = u32::try_from(i + 1).unwrap_or(u32::MAX);
            assert_eq!(m.id, expected_id, "ids must be sequential from 1");
            assert_eq!(file, &format!("{:04}_{}.sql", m.id, m.name));
            let on_disk = std::fs::read_to_string(dir.join(file)).unwrap_or_default();
            assert_eq!(on_disk, m.sql, "{file} must be the embedded SQL");
        }
    }

    #[test]
    fn data_track_matches_files() {
        check_track("data", DATA_MIGRATIONS);
    }

    #[test]
    fn logs_track_matches_files() {
        check_track("logs", LOGS_MIGRATIONS);
    }
}
