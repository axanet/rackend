//! Renders and runs schema-change steps (SPEC §8.5).

use rackend_core::domain::DdlStep;
use rackend_core::{Error, Result};
use rusqlite::Connection;

use crate::records::map_err;
use crate::rules_sql::q;

/// SQL statements for one step (multi→single checks run separately).
fn render(step: &DdlStep) -> Vec<String> {
    match step {
        DdlStep::CreateTable {
            table,
            fields,
            auth,
        } => {
            let mut cols = vec![
                "\"id\" TEXT PRIMARY KEY NOT NULL".to_owned(),
                "\"created\" TEXT NOT NULL".to_owned(),
                "\"updated\" TEXT NOT NULL".to_owned(),
            ];
            cols.extend(
                fields
                    .iter()
                    .map(|f| format!("{} {}", q(&f.name), f.sql_type())),
            );
            if *auth {
                cols.push("\"password_hash\" TEXT".to_owned());
            }
            vec![format!("CREATE TABLE {} ({})", q(table), cols.join(", "))]
        }
        DdlStep::AddColumn { table, field } => {
            vec![format!(
                "ALTER TABLE {} ADD COLUMN {} {}",
                q(table),
                q(&field.name),
                field.sql_type()
            )]
        }
        DdlStep::RenameColumn { table, from, to } => {
            vec![format!(
                "ALTER TABLE {} RENAME COLUMN {} TO {}",
                q(table),
                q(from),
                q(to)
            )]
        }
        DdlStep::SingleToMulti { table, column } => vec![format!(
            "UPDATE {t} SET {c} = CASE WHEN {c} IS NULL OR {c} = '' THEN NULL ELSE json_array({c}) END",
            t = q(table),
            c = q(column)
        )],
        DdlStep::MultiToSingle { table, column } => vec![format!(
            "UPDATE {t} SET {c} = CASE WHEN json_valid({c}) THEN json_extract({c}, '$[0]') ELSE {c} END",
            t = q(table),
            c = q(column)
        )],
        DdlStep::RenameTable { from, to } => {
            vec![format!("ALTER TABLE {} RENAME TO {}", q(from), q(to))]
        }
        DdlStep::DropColumn { table, column } => vec![format!(
            "ALTER TABLE {} DROP COLUMN {}",
            q(table),
            q(column)
        )],
        DdlStep::DropTable { table } => vec![format!("DROP TABLE IF EXISTS {}", q(table))],
        DdlStep::CreateIndex {
            table,
            name,
            columns,
            unique,
        } => vec![format!(
            "CREATE {}INDEX {} ON {} ({})",
            if *unique { "UNIQUE " } else { "" },
            q(name),
            q(table),
            columns.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ")
        )],
        DdlStep::DropIndex { name } => vec![format!("DROP INDEX IF EXISTS {}", q(name))],
    }
}

/// The SQL of `steps`, joined like [`apply`] reports it.
pub fn render_all(steps: &[DdlStep]) -> String {
    steps
        .iter()
        .flat_map(render)
        .collect::<Vec<_>>()
        .join(";\n")
}

/// Runs `steps` on `conn` (inside the caller's transaction) and returns the SQL.
pub fn apply(conn: &Connection, steps: &[DdlStep]) -> Result<String> {
    let mut log = Vec::new();
    for step in steps {
        if let DdlStep::MultiToSingle { table, column } = step {
            let n: i64 = conn
                .query_row(
                    &format!(
                        "SELECT count(*) FROM {} WHERE json_valid({c}) AND json_array_length({c}) > 1",
                        q(table),
                        c = q(column)
                    ),
                    [],
                    |r| r.get(0),
                )
                .map_err(map_err)?;
            if n > 0 {
                return Err(Error::Conflict(format!(
                    "{n} record(s) have more than one value in '{column}'; reduce them to one before switching to a single value"
                )));
            }
        }
        if let DdlStep::CreateIndex {
            unique: true, name, ..
        } = step
        {
            // A unique index over duplicate data fails; report it as a conflict.
            for sql in render(step) {
                conn.execute_batch(&sql).map_err(|e| match map_err(e) {
                    Error::Conflict(_) => Error::Conflict(format!("cannot create unique index '{name}': existing records have duplicate values")),
                    other => other,
                })?;
                log.push(sql);
            }
            continue;
        }
        for sql in render(step) {
            conn.execute_batch(&sql).map_err(map_err)?;
            log.push(sql);
        }
    }
    Ok(log.join(";\n"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use rackend_core::domain::Field;
    use rackend_core::error::ValidationErrors;
    use serde_json::json;

    fn field(v: serde_json::Value) -> Field {
        Field::from_json(&v, "f", &mut ValidationErrors::default()).unwrap()
    }

    #[test]
    fn create_alter_and_convert() {
        let c = Connection::open_in_memory().unwrap();
        let tags = field(json!({"name": "tags", "type": "select", "values": ["a", "b"]}));
        apply(
            &c,
            &[DdlStep::CreateTable {
                auth: false,
                table: "posts".into(),
                fields: vec![tags],
            }],
        )
        .unwrap();
        c.execute(
            "INSERT INTO posts VALUES ('1', 'c', 'u', 'a'), ('2', 'c', 'u', NULL)",
            [],
        )
        .unwrap();
        apply(
            &c,
            &[DdlStep::SingleToMulti {
                table: "posts".into(),
                column: "tags".into(),
            }],
        )
        .unwrap();
        let v: String = c
            .query_row("SELECT tags FROM posts WHERE id = '1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, "[\"a\"]");
        c.execute("UPDATE posts SET tags = '[\"a\",\"b\"]' WHERE id = '2'", [])
            .unwrap();
        let err = apply(
            &c,
            &[DdlStep::MultiToSingle {
                table: "posts".into(),
                column: "tags".into(),
            }],
        )
        .unwrap_err();
        assert!(matches!(err, Error::Conflict(_)));
        c.execute("UPDATE posts SET tags = '[\"b\"]' WHERE id = '2'", [])
            .unwrap();
        apply(
            &c,
            &[DdlStep::MultiToSingle {
                table: "posts".into(),
                column: "tags".into(),
            }],
        )
        .unwrap();
        let v: String = c
            .query_row("SELECT tags FROM posts WHERE id = '2'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, "b");
        let sql = apply(
            &c,
            &[DdlStep::CreateIndex {
                table: "posts".into(),
                name: "idx_posts_t".into(),
                columns: vec!["tags".into()],
                unique: true,
            }],
        )
        .unwrap();
        assert!(sql.starts_with("CREATE UNIQUE INDEX"));
    }
}
