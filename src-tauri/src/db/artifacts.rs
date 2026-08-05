//! The artifact index.
//!
//! Rows describe markdown documents that were deliberately saved — by an agent
//! through `save_artifact`, or by the user. Content lives on disk under the store
//! root; this module owns the metadata.
//!
//! Identity is the row id. Title, kind and tags are given by whoever saved the
//! document, never inferred from its prose; only `preview` and `size` follow from
//! the content.

use super::DbPool;
use crate::error::AppError;
use rusqlite::{params, Row};
use serde::Serialize;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct StoredArtifact {
    pub id: i64,
    pub project_id: i64,
    pub stored_name: String,
    pub title: Option<String>,
    pub kind: String,
    /// JSON array, stored the way `tasks.tags` is so the same frontend helpers read both.
    pub tags: Option<String>,
    pub preview: String,
    pub size: i64,
    /// Provenance for rows that predate explicit saves. Nullable and unused by new ones.
    pub origin: Option<String>,
    pub origin_task_id: Option<i64>,
    pub last_task_id: Option<i64>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// What a caller supplies when saving or revising a document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DerivedMeta {
    pub title: Option<String>,
    pub preview: String,
    pub kind: String,
    pub size: i64,
}

pub(crate) fn row_to_artifact(row: &Row) -> rusqlite::Result<StoredArtifact> {
    Ok(StoredArtifact {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        stored_name: row.get("stored_name")?,
        title: row.get("title").ok().flatten(),
        kind: row
            .get::<_, Option<String>>("kind")?
            .unwrap_or_else(|| "other".into()),
        tags: row.get("tags").ok().flatten(),
        preview: row.get::<_, Option<String>>("preview")?.unwrap_or_default(),
        size: row.get::<_, Option<i64>>("size")?.unwrap_or(0),
        origin: row.get("origin").ok().flatten(),
        origin_task_id: row.get("origin_task_id").ok().flatten(),
        last_task_id: row.get("last_task_id").ok().flatten(),
        created_at: row.get("created_at").ok().flatten(),
        updated_at: row.get("updated_at").ok().flatten(),
    })
}

/// Record a newly saved document.
///
/// Always inserts. Two documents may share a title, and a caller that means to
/// revise an existing one calls [`update_meta`] with its id — inferring "same
/// title means same document" is the kind of guessing this design removed.
pub fn create(
    db: &DbPool,
    project_id: i64,
    stored_name: &str,
    meta: &DerivedMeta,
    tags: &str,
    task_id: Option<i64>,
) -> Result<i64, AppError> {
    let conn = db.lock();
    conn.execute(
        "INSERT INTO artifacts
            (project_id, stored_name, title, kind, tags, preview, size,
             origin_task_id, last_task_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
        params![
            project_id,
            stored_name,
            meta.title,
            meta.kind,
            tags,
            meta.preview,
            meta.size,
            task_id
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Revise an indexed document. Only the fields supplied change.
#[allow(clippy::too_many_arguments)]
pub fn update_meta(
    db: &DbPool,
    id: i64,
    title: Option<&str>,
    kind: Option<&str>,
    tags: Option<&str>,
    preview: Option<&str>,
    size: Option<i64>,
    task_id: Option<i64>,
) -> Result<(), AppError> {
    let conn = db.lock();
    // COALESCE so a NULL parameter leaves the column alone: an agent updating a
    // document's body must not wipe the tags it was classified with.
    conn.execute(
        "UPDATE artifacts SET
            title      = COALESCE(?1, title),
            kind       = COALESCE(?2, kind),
            tags       = COALESCE(?3, tags),
            preview    = COALESCE(?4, preview),
            size       = COALESCE(?5, size),
            last_task_id = COALESCE(?6, last_task_id),
            updated_at = datetime('now','localtime')
          WHERE id = ?7",
        params![title, kind, tags, preview, size, task_id, id],
    )?;
    Ok(())
}

/// Indexed artifacts for a project, most recently updated first.
pub fn list_for_project(db: &DbPool, project_id: i64) -> Vec<StoredArtifact> {
    let conn = db.lock();
    let mut stmt = match conn
        .prepare("SELECT * FROM artifacts WHERE project_id=?1 ORDER BY updated_at DESC, id DESC")
    {
        Ok(s) => s,
        Err(e) => {
            log::error!("list_for_project: {}", e);
            return vec![];
        }
    };
    // Collected into a local rather than returned as the tail expression: as a
    // tail expression the MappedRows temporary is dropped *after* `conn` and
    // `stmt`, which it borrows from.
    let mut out = Vec::new();
    match stmt.query_map(params![project_id], row_to_artifact) {
        Ok(rows) => out.extend(rows.filter_map(|r| r.ok())),
        Err(e) => log::error!("list_for_project: {}", e),
    }
    out
}

pub fn get(db: &DbPool, id: i64) -> Option<StoredArtifact> {
    let conn = db.lock();
    conn.query_row("SELECT * FROM artifacts WHERE id=?1", params![id], |r| {
        row_to_artifact(r)
    })
    .ok()
}

pub fn delete(db: &DbPool, id: i64) -> Result<(), AppError> {
    let conn = db.lock();
    conn.execute("DELETE FROM artifacts WHERE id=?1", params![id])?;
    Ok(())
}

#[cfg(test)]
mod index_tests {
    use super::*;
    use parking_lot::Mutex;
    use rusqlite::Connection;
    use std::sync::Arc;

    fn test_db() -> DbPool {
        let conn = Connection::open_in_memory().expect("in-memory db");
        crate::db::schema::create_tables(&conn);
        // Attribution relies on ON DELETE SET NULL, which SQLite only honours
        // with foreign keys switched on.
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        Arc::new(Mutex::new(conn))
    }

    fn seed_project(db: &DbPool, slug: &str) -> i64 {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO projects (name,slug,working_dir) VALUES (?1,?1,'/repo')",
            params![slug],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn seed_task(db: &DbPool, project_id: i64, title: &str) -> i64 {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO tasks (project_id,title,status) VALUES (?1,?2,'done')",
            params![project_id, title],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn meta(title: &str, preview: &str, kind: &str, size: i64) -> DerivedMeta {
        DerivedMeta {
            title: Some(title.to_string()),
            preview: preview.to_string(),
            kind: kind.to_string(),
            size,
        }
    }

    #[test]
    fn creating_records_what_it_was_given() {
        let db = test_db();
        let p = seed_project(&db, "board");
        let t = seed_task(&db, p, "writer");

        let id = create(
            &db,
            p,
            "auth-plan-1.md",
            &meta("Auth plan", "a preview", "plan", 120),
            r#"["context"]"#,
            Some(t),
        )
        .unwrap();

        let row = get(&db, id).unwrap();
        assert_eq!(row.title.as_deref(), Some("Auth plan"));
        assert_eq!(row.kind, "plan");
        assert_eq!(row.tags.as_deref(), Some(r#"["context"]"#));
        assert_eq!(row.size, 120);
        assert_eq!(row.origin_task_id, Some(t));
        assert_eq!(row.last_task_id, Some(t));
        assert_eq!(row.origin, None, "explicit saves have no repository path");
    }

    #[test]
    fn two_documents_with_the_same_title_are_two_rows() {
        let db = test_db();
        let p = seed_project(&db, "board");

        let one = create(
            &db,
            p,
            "notes-1.md",
            &meta("Notes", "", "doc", 1),
            "[]",
            None,
        )
        .unwrap();
        let two = create(
            &db,
            p,
            "notes-2.md",
            &meta("Notes", "", "doc", 1),
            "[]",
            None,
        )
        .unwrap();

        // Identity is the id: "same title" is not "same document".
        assert_ne!(one, two);
        assert_eq!(list_for_project(&db, p).len(), 2);
    }

    #[test]
    fn update_meta_changes_only_what_it_is_given() {
        let db = test_db();
        let p = seed_project(&db, "board");
        let reviser = seed_task(&db, p, "reviser");
        let id = create(
            &db,
            p,
            "plan-1.md",
            &meta("Plan", "old preview", "plan", 10),
            r#"["context"]"#,
            None,
        )
        .unwrap();

        // Only the body changed, so the classification must survive: an agent
        // revising a document must not wipe the tags it was filed under.
        update_meta(
            &db,
            id,
            None,
            None,
            None,
            Some("new preview"),
            Some(99),
            Some(reviser),
        )
        .unwrap();

        let row = get(&db, id).unwrap();
        assert_eq!(row.title.as_deref(), Some("Plan"));
        assert_eq!(row.kind, "plan");
        assert_eq!(row.tags.as_deref(), Some(r#"["context"]"#));
        assert_eq!(row.preview, "new preview");
        assert_eq!(row.size, 99);
        assert_eq!(row.last_task_id, Some(reviser));
    }

    #[test]
    fn update_meta_can_retitle_and_retag() {
        let db = test_db();
        let p = seed_project(&db, "board");
        let id = create(
            &db,
            p,
            "plan-1.md",
            &meta("Plan", "", "plan", 10),
            "[]",
            None,
        )
        .unwrap();

        update_meta(
            &db,
            id,
            Some("Renamed"),
            Some("spec"),
            Some(r#"["shared"]"#),
            None,
            None,
            None,
        )
        .unwrap();

        let row = get(&db, id).unwrap();
        assert_eq!(row.title.as_deref(), Some("Renamed"));
        assert_eq!(row.kind, "spec");
        assert_eq!(row.tags.as_deref(), Some(r#"["shared"]"#));
    }

    #[test]
    fn deleting_the_authoring_task_keeps_the_artifact() {
        let db = test_db();
        let p = seed_project(&db, "board");
        let task = seed_task(&db, p, "writer");
        let id = create(&db, p, "x-1.md", &meta("X", "", "doc", 1), "[]", Some(task)).unwrap();

        {
            let conn = db.lock();
            conn.execute("DELETE FROM tasks WHERE id=?1", params![task])
                .unwrap();
        }

        let row = get(&db, id).expect("artifact must survive its task");
        assert_eq!(row.origin_task_id, None, "attribution clears, row stays");
        assert_eq!(row.title.as_deref(), Some("X"));
    }

    #[test]
    fn deleting_the_project_removes_its_artifacts() {
        let db = test_db();
        let p = seed_project(&db, "board");
        create(&db, p, "x-1.md", &meta("X", "", "doc", 1), "[]", None).unwrap();

        {
            let conn = db.lock();
            conn.execute("DELETE FROM projects WHERE id=?1", params![p])
                .unwrap();
        }

        assert!(list_for_project(&db, p).is_empty());
    }

    #[test]
    fn delete_removes_the_row() {
        let db = test_db();
        let p = seed_project(&db, "board");
        let id = create(&db, p, "d-1.md", &meta("D", "", "doc", 1), "[]", None).unwrap();

        delete(&db, id).unwrap();

        assert!(get(&db, id).is_none());
    }
}
