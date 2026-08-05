//! Which tasks reference which artifacts, and in what role.
//!
//! A relation rather than a path pasted into a task description, because three
//! things need to query it: the task detail view listing its documents, a blocker
//! pointing at the document under discussion, and the prompt builder deciding
//! which store paths a task should be told about. Parsing paths back out of prose
//! would be guesswork.

use super::artifacts::StoredArtifact;
use super::DbPool;
use crate::error::AppError;
use rusqlite::params;

/// The default role: the task reads this document for context.
pub const ROLE_REFERENCE: &str = "reference";

/// Relate a task to an artifact. Adding the same reference twice is a no-op.
pub fn add_ref(db: &DbPool, task_id: i64, artifact_id: i64, role: &str) -> Result<(), AppError> {
    let role = if role.trim().is_empty() {
        ROLE_REFERENCE
    } else {
        role.trim()
    };
    let conn = db.lock();
    // OR IGNORE: clicking a picker twice should not surface an error.
    conn.execute(
        "INSERT OR IGNORE INTO task_artifact_refs (task_id, artifact_id, role)
         VALUES (?1, ?2, ?3)",
        params![task_id, artifact_id, role],
    )?;
    Ok(())
}

/// Drop a task's reference to an artifact, in every role.
pub fn remove_ref(db: &DbPool, task_id: i64, artifact_id: i64) -> Result<(), AppError> {
    let conn = db.lock();
    conn.execute(
        "DELETE FROM task_artifact_refs WHERE task_id=?1 AND artifact_id=?2",
        params![task_id, artifact_id],
    )?;
    Ok(())
}

/// Artifact ids a task references, with the role, most recent first.
pub fn refs_for_task(db: &DbPool, task_id: i64) -> Vec<(i64, String)> {
    let conn = db.lock();
    let mut stmt = match conn.prepare(
        "SELECT artifact_id, role FROM task_artifact_refs
         WHERE task_id=?1 ORDER BY created_at DESC, artifact_id DESC",
    ) {
        Ok(s) => s,
        Err(e) => {
            log::error!("refs_for_task: {}", e);
            return vec![];
        }
    };
    let mut out = Vec::new();
    match stmt.query_map(params![task_id], |r| Ok((r.get(0)?, r.get(1)?))) {
        Ok(rows) => out.extend(rows.filter_map(|r| r.ok())),
        Err(e) => log::error!("refs_for_task: {}", e),
    }
    out
}

/// The artifacts a task references, joined so callers get titles and paths.
///
/// One row per artifact even when it is referenced in several roles: the callers
/// that matter — the detail view and the prompt builder — want the document once.
pub fn artifacts_for_task(db: &DbPool, task_id: i64) -> Vec<StoredArtifact> {
    let conn = db.lock();
    let mut stmt = match conn.prepare(
        "SELECT DISTINCT a.* FROM artifacts a
         JOIN task_artifact_refs r ON r.artifact_id = a.id
         WHERE r.task_id = ?1
         ORDER BY a.updated_at DESC, a.id DESC",
    ) {
        Ok(s) => s,
        Err(e) => {
            log::error!("artifacts_for_task: {}", e);
            return vec![];
        }
    };
    let mut out = Vec::new();
    match stmt.query_map(params![task_id], super::artifacts::row_to_artifact) {
        Ok(rows) => out.extend(rows.filter_map(|r| r.ok())),
        Err(e) => log::error!("artifacts_for_task: {}", e),
    }
    out
}

/// Task ids referencing an artifact, for showing where a document is used.
pub fn tasks_for_artifact(db: &DbPool, artifact_id: i64) -> Vec<i64> {
    let conn = db.lock();
    let mut stmt = match conn.prepare(
        "SELECT DISTINCT task_id FROM task_artifact_refs WHERE artifact_id=?1 ORDER BY task_id",
    ) {
        Ok(s) => s,
        Err(e) => {
            log::error!("tasks_for_artifact: {}", e);
            return vec![];
        }
    };
    let mut out = Vec::new();
    match stmt.query_map(params![artifact_id], |r| r.get::<_, i64>(0)) {
        Ok(rows) => out.extend(rows.filter_map(|r| r.ok())),
        Err(e) => log::error!("tasks_for_artifact: {}", e),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::artifacts::{self, DerivedMeta};
    use parking_lot::Mutex;
    use rusqlite::Connection;
    use std::sync::Arc;

    fn test_db() -> DbPool {
        let conn = Connection::open_in_memory().expect("in-memory db");
        crate::db::schema::create_tables(&conn);
        // The CASCADE behaviour these tests assert only happens with foreign keys on.
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        Arc::new(Mutex::new(conn))
    }

    fn seed_project(db: &DbPool) -> i64 {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO projects (name,slug,working_dir) VALUES ('B','b','/repo')",
            [],
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

    fn seed_artifact(db: &DbPool, project_id: i64, task_id: i64, name: &str) -> i64 {
        artifacts::create(
            db,
            project_id,
            &format!("{}-1.md", name.replace('/', "-")),
            &DerivedMeta::default(),
            "[]",
            Some(task_id),
        )
        .unwrap()
    }

    #[test]
    fn a_task_can_reference_one_artifact_in_several_roles() {
        let db = test_db();
        let p = seed_project(&db);
        let t = seed_task(&db, p, "t");
        let a = seed_artifact(&db, p, t, "docs/plan.md");

        add_ref(&db, t, a, "reference").unwrap();
        add_ref(&db, t, a, "progress").unwrap();

        // Two roles, both legitimate.
        assert_eq!(refs_for_task(&db, t).len(), 2);
        // But the document itself is listed once, which is what the prompt and
        // the detail view want.
        assert_eq!(artifacts_for_task(&db, t).len(), 1);
    }

    #[test]
    fn adding_the_same_reference_twice_is_not_an_error() {
        let db = test_db();
        let p = seed_project(&db);
        let t = seed_task(&db, p, "t");
        let a = seed_artifact(&db, p, t, "docs/plan.md");
        add_ref(&db, t, a, "reference").unwrap();

        assert!(add_ref(&db, t, a, "reference").is_ok());
        assert_eq!(artifacts_for_task(&db, t).len(), 1);
    }

    #[test]
    fn an_empty_role_falls_back_to_reference() {
        let db = test_db();
        let p = seed_project(&db);
        let t = seed_task(&db, p, "t");
        let a = seed_artifact(&db, p, t, "docs/plan.md");

        add_ref(&db, t, a, "  ").unwrap();

        assert_eq!(refs_for_task(&db, t), vec![(a, ROLE_REFERENCE.to_string())]);
    }

    #[test]
    fn deleting_an_artifact_drops_its_references() {
        let db = test_db();
        let p = seed_project(&db);
        let t = seed_task(&db, p, "t");
        let a = seed_artifact(&db, p, t, "docs/gone.md");
        add_ref(&db, t, a, "reference").unwrap();

        artifacts::delete(&db, a).unwrap();

        assert!(artifacts_for_task(&db, t).is_empty());
        assert!(refs_for_task(&db, t).is_empty());
    }

    #[test]
    fn deleting_a_task_drops_its_references_but_keeps_the_artifact() {
        let db = test_db();
        let p = seed_project(&db);
        let t = seed_task(&db, p, "t");
        let a = seed_artifact(&db, p, t, "docs/plan.md");
        add_ref(&db, t, a, "reference").unwrap();

        {
            let conn = db.lock();
            conn.execute("DELETE FROM tasks WHERE id=?1", params![t])
                .unwrap();
        }

        assert!(tasks_for_artifact(&db, a).is_empty());
        // The reference goes; the document does not.
        assert!(artifacts::get(&db, a).is_some());
    }

    #[test]
    fn removing_a_reference_clears_every_role() {
        let db = test_db();
        let p = seed_project(&db);
        let t = seed_task(&db, p, "t");
        let a = seed_artifact(&db, p, t, "docs/plan.md");
        add_ref(&db, t, a, "reference").unwrap();
        add_ref(&db, t, a, "progress").unwrap();

        remove_ref(&db, t, a).unwrap();

        assert!(refs_for_task(&db, t).is_empty());
    }

    #[test]
    fn tasks_for_artifact_lists_every_referencing_task_once() {
        let db = test_db();
        let p = seed_project(&db);
        let one = seed_task(&db, p, "one");
        let two = seed_task(&db, p, "two");
        let a = seed_artifact(&db, p, one, "docs/plan.md");
        add_ref(&db, one, a, "reference").unwrap();
        add_ref(&db, one, a, "progress").unwrap();
        add_ref(&db, two, a, "reference").unwrap();

        assert_eq!(tasks_for_artifact(&db, a), vec![one, two]);
    }
}
