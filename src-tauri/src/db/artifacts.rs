//! The artifact index.
//!
//! The index describes markdown documents that agents wrote, one row per source
//! document. Content lives on disk under the artifact store root; this module
//! owns metadata and attribution. Identity is `(project_id, source_rel_path)`,
//! so a document edited by three tasks is one artifact whose `last_task_id`
//! moves while `origin_task_id` stays with whoever created it.

use super::DbPool;
use crate::error::AppError;
use rusqlite::{params, Row};
use serde::Serialize;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct StoredArtifact {
    pub id: i64,
    pub project_id: i64,
    pub stored_name: String,
    pub source_rel_path: String,
    pub title: Option<String>,
    pub preview: String,
    pub kind: String,
    pub size: i64,
    pub origin_task_id: Option<i64>,
    pub last_task_id: Option<i64>,
    /// SHA-256 of the content as last synced from the repository. Only capture
    /// writes it; when the stored file stops matching, the copy is user-owned.
    pub captured_hash: Option<String>,
    /// Set when capture found a newer repository version and declined to
    /// overwrite a diverged copy.
    pub conflict_at: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// Everything derived from a document's content, so callers parse once and hand
/// the result to both the index row and the on-disk write.
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
        source_rel_path: row.get("source_rel_path")?,
        title: row.get("title").ok().flatten(),
        preview: row.get::<_, Option<String>>("preview")?.unwrap_or_default(),
        kind: row
            .get::<_, Option<String>>("kind")?
            .unwrap_or_else(|| "other".into()),
        size: row.get::<_, Option<i64>>("size")?.unwrap_or(0),
        origin_task_id: row.get("origin_task_id").ok().flatten(),
        last_task_id: row.get("last_task_id").ok().flatten(),
        captured_hash: row.get("captured_hash").ok().flatten(),
        conflict_at: row.get("conflict_at").ok().flatten(),
        created_at: row.get("created_at").ok().flatten(),
        updated_at: row.get("updated_at").ok().flatten(),
    })
}

/// Record a captured document, or refresh the row that already describes it.
///
/// `origin_task_id` and `created_at` survive an update: they say who first wrote
/// the document, which does not change when someone else edits it.
pub fn insert_or_replace(
    db: &DbPool,
    project_id: i64,
    source_rel_path: &str,
    stored_name: &str,
    meta: &DerivedMeta,
    task_id: i64,
    captured_hash: &str,
) -> Result<i64, AppError> {
    let conn = db.lock();
    conn.execute(
        "INSERT INTO artifacts
            (project_id, stored_name, source_rel_path, title, preview, kind, size,
             origin_task_id, last_task_id, captured_hash, conflict_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9, NULL)
         ON CONFLICT(project_id, source_rel_path) DO UPDATE SET
            title = excluded.title,
            preview = excluded.preview,
            kind = excluded.kind,
            size = excluded.size,
            last_task_id = excluded.last_task_id,
            captured_hash = excluded.captured_hash,
            -- A successful sync from the repo resolves any previous divergence.
            conflict_at = NULL,
            updated_at = datetime('now','localtime')",
        params![
            project_id,
            stored_name,
            source_rel_path,
            meta.title,
            meta.preview,
            meta.kind,
            meta.size,
            task_id,
            captured_hash
        ],
    )?;
    // `last_insert_rowid` reports 0 when the statement took the DO UPDATE path,
    // so read the id back rather than trusting it.
    let id = conn.query_row(
        "SELECT id FROM artifacts WHERE project_id=?1 AND source_rel_path=?2",
        params![project_id, source_rel_path],
        |r| r.get(0),
    )?;
    Ok(id)
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

/// The artifact describing a given source document, if one is indexed.
///
/// Capture looks this up before naming a file: a document that already has an
/// artifact keeps its existing filename, so a path already handed to an agent
/// stays valid across re-captures. `insert_or_replace` leaves `stored_name`
/// alone on the conflict path for the same reason.
pub fn find_by_source(
    db: &DbPool,
    project_id: i64,
    source_rel_path: &str,
) -> Option<StoredArtifact> {
    let conn = db.lock();
    conn.query_row(
        "SELECT * FROM artifacts WHERE project_id=?1 AND source_rel_path=?2",
        params![project_id, source_rel_path],
        row_to_artifact,
    )
    .ok()
}

/// Refresh the metadata derived from a document's content.
///
/// Deliberately leaves `captured_hash` alone. That is what makes an in-app edit —
/// or an agent writing through the store path — diverge from the last repository
/// sync, so a later capture keeps its hands off rather than silently discarding
/// the edit.
pub fn update_content_meta(db: &DbPool, id: i64, meta: &DerivedMeta) -> Result<(), AppError> {
    let conn = db.lock();
    conn.execute(
        "UPDATE artifacts
            SET title=?1, preview=?2, kind=?3, size=?4,
                updated_at=datetime('now','localtime')
          WHERE id=?5",
        params![meta.title, meta.preview, meta.kind, meta.size, id],
    )?;
    Ok(())
}

/// Flag that the repository has a newer version that capture declined to write.
pub fn set_conflict(db: &DbPool, id: i64) -> Result<(), AppError> {
    let conn = db.lock();
    conn.execute(
        "UPDATE artifacts SET conflict_at=datetime('now','localtime') WHERE id=?1",
        params![id],
    )?;
    Ok(())
}

/// Acknowledge a divergence. The stored copy is unchanged; only the flag clears.
pub fn clear_conflict(db: &DbPool, id: i64) -> Result<(), AppError> {
    let conn = db.lock();
    conn.execute(
        "UPDATE artifacts SET conflict_at=NULL WHERE id=?1",
        params![id],
    )?;
    Ok(())
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

    fn derived(title: &str, preview: &str, kind: &str, size: i64) -> DerivedMeta {
        DerivedMeta {
            title: Some(title.to_string()),
            preview: preview.to_string(),
            kind: kind.to_string(),
            size,
        }
    }

    #[test]
    fn re_capturing_the_same_document_updates_rather_than_duplicates() {
        let db = test_db();
        let p = seed_project(&db, "board");
        let first_author = seed_task(&db, p, "writer");
        let second_author = seed_task(&db, p, "editor");

        let first = insert_or_replace(
            &db,
            p,
            "docs/plan.md",
            "1-plan.md",
            &derived("Plan", "a preview", "plan", 120),
            first_author,
            "seed-hash",
        )
        .unwrap();
        let second = insert_or_replace(
            &db,
            p,
            "docs/plan.md",
            "1-plan.md",
            &derived("Plan v2", "changed", "plan", 300),
            second_author,
            "seed-hash",
        )
        .unwrap();

        assert_eq!(first, second, "same document must stay one artifact");
        let row = get(&db, first).unwrap();
        assert_eq!(row.title.as_deref(), Some("Plan v2"));
        assert_eq!(row.size, 300);
        assert_eq!(row.origin_task_id, Some(first_author), "first author kept");
        assert_eq!(row.last_task_id, Some(second_author), "latest recorded");
        assert_eq!(list_for_project(&db, p).len(), 1);
    }

    #[test]
    fn two_different_documents_are_two_artifacts() {
        let db = test_db();
        let p = seed_project(&db, "board");
        let t = seed_task(&db, p, "writer");

        insert_or_replace(
            &db,
            p,
            "docs/a.md",
            "1-a.md",
            &derived("A", "", "doc", 1),
            t,
            "seed-hash",
        )
        .unwrap();
        insert_or_replace(
            &db,
            p,
            "docs/b.md",
            "2-b.md",
            &derived("B", "", "doc", 1),
            t,
            "seed-hash",
        )
        .unwrap();

        assert_eq!(list_for_project(&db, p).len(), 2);
    }

    #[test]
    fn the_same_path_in_two_projects_stays_separate() {
        let db = test_db();
        let a = seed_project(&db, "board");
        let b = seed_project(&db, "other");
        let ta = seed_task(&db, a, "a");
        let tb = seed_task(&db, b, "b");

        // Identity is scoped to the project; every repo has a README.
        let one = insert_or_replace(
            &db,
            a,
            "README.md",
            "1-readme.md",
            &DerivedMeta::default(),
            ta,
            "seed-hash",
        )
        .unwrap();
        let two = insert_or_replace(
            &db,
            b,
            "README.md",
            "2-readme.md",
            &DerivedMeta::default(),
            tb,
            "seed-hash",
        )
        .unwrap();

        assert_ne!(one, two);
        assert_eq!(list_for_project(&db, a).len(), 1);
        assert_eq!(list_for_project(&db, b).len(), 1);
    }

    #[test]
    fn deleting_the_authoring_task_keeps_the_artifact() {
        let db = test_db();
        let p = seed_project(&db, "board");
        let task = seed_task(&db, p, "writer");
        let id = insert_or_replace(
            &db,
            p,
            "docs/x.md",
            "1-x.md",
            &derived("X", "", "doc", 1),
            task,
            "seed-hash",
        )
        .unwrap();

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
        let t = seed_task(&db, p, "writer");
        insert_or_replace(
            &db,
            p,
            "docs/x.md",
            "1-x.md",
            &DerivedMeta::default(),
            t,
            "h",
        )
        .unwrap();

        {
            let conn = db.lock();
            conn.execute("DELETE FROM projects WHERE id=?1", params![p])
                .unwrap();
        }

        assert!(list_for_project(&db, p).is_empty());
    }

    #[test]
    fn update_content_meta_refreshes_the_derived_fields() {
        let db = test_db();
        let p = seed_project(&db, "board");
        let t = seed_task(&db, p, "writer");
        let id = insert_or_replace(
            &db,
            p,
            "docs/p.md",
            "1-p.md",
            &derived("Old", "old", "doc", 10),
            t,
            "seed-hash",
        )
        .unwrap();

        update_content_meta(&db, id, &derived("New", "new body", "plan", 99)).unwrap();

        let row = get(&db, id).unwrap();
        assert_eq!(row.title.as_deref(), Some("New"));
        assert_eq!(row.preview, "new body");
        assert_eq!(row.kind, "plan");
        assert_eq!(row.size, 99);
    }

    #[test]
    fn delete_removes_the_row() {
        let db = test_db();
        let p = seed_project(&db, "board");
        let t = seed_task(&db, p, "writer");
        let id = insert_or_replace(
            &db,
            p,
            "docs/d.md",
            "1-d.md",
            &DerivedMeta::default(),
            t,
            "h",
        )
        .unwrap();

        delete(&db, id).unwrap();

        assert!(get(&db, id).is_none());
    }
}
