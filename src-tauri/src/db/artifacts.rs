//! The artifact index, plus the log-replay attribution it replaces.
//!
//! The index describes markdown documents that agents wrote, one row per source
//! document. Content lives on disk under the artifact store root; this module
//! owns metadata and attribution. Identity is `(project_id, source_rel_path)`,
//! so a document edited by three tasks is one artifact whose `last_task_id`
//! moves while `origin_task_id` stays with whoever created it.
//!
//! The older half of this module reconstructs authorship by replaying
//! `task_logs` rows with `log_type='tool'`, which is how the repository-wide
//! markdown scan attributed the files it found. It is retired once the scan is
//! removed; the index makes it unnecessary, because capture records the writer
//! at the moment of the write.

use super::DbPool;
use crate::error::AppError;
use rusqlite::{params, Row};
use serde::Serialize;
use std::collections::HashMap;

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

fn row_to_artifact(row: &Row) -> rusqlite::Result<StoredArtifact> {
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
) -> Result<i64, AppError> {
    let conn = db.lock();
    conn.execute(
        "INSERT INTO artifacts
            (project_id, stored_name, source_rel_path, title, preview, kind, size,
             origin_task_id, last_task_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
         ON CONFLICT(project_id, source_rel_path) DO UPDATE SET
            title = excluded.title,
            preview = excluded.preview,
            kind = excluded.kind,
            size = excluded.size,
            last_task_id = excluded.last_task_id,
            updated_at = datetime('now','localtime')",
        params![
            project_id,
            stored_name,
            source_rel_path,
            meta.title,
            meta.preview,
            meta.kind,
            meta.size,
            task_id
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

/// Attach the on-disk filename to an indexed artifact.
///
/// Split from `insert_or_replace` because the name is derived from the row id,
/// so the row has to exist first. `insert_or_replace` deliberately leaves
/// `stored_name` alone on the conflict path, which is what lets a re-captured
/// document keep the filename existing references already point at.
pub fn set_stored_name(db: &DbPool, id: i64, stored_name: &str) -> Result<(), AppError> {
    let conn = db.lock();
    conn.execute(
        "UPDATE artifacts SET stored_name=?1 WHERE id=?2",
        params![stored_name, id],
    )?;
    Ok(())
}

/// Refresh the metadata derived from a document's content, after an in-app edit.
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

pub fn delete(db: &DbPool, id: i64) -> Result<(), AppError> {
    let conn = db.lock();
    conn.execute("DELETE FROM artifacts WHERE id=?1", params![id])?;
    Ok(())
}

/// Tool calls that produce or modify a file. Read-only tools are ignored.
const WRITE_TOOLS: [&str; 4] = ["Write", "Edit", "MultiEdit", "NotebookEdit"];

#[derive(Serialize, Clone, Debug)]
pub struct ArtifactTaskRef {
    pub task_id: i64,
    pub task_key: String,
    pub title: String,
    pub status: String,
    pub written_at: Option<String>,
}

/// Maps every markdown file written by a task in this project to the tasks that
/// wrote it, newest write first.
///
/// Keys are artifact paths relative to `working_dir`, forward-slash separated
/// and lowercased. Files logged outside `working_dir` are dropped. Returns an
/// empty map when the project has no tool logs, or when the query fails.
pub fn markdown_writes_by_project(
    db: &DbPool,
    project_id: i64,
    working_dir: &str,
) -> HashMap<String, Vec<ArtifactTaskRef>> {
    let mut by_path: HashMap<String, Vec<ArtifactTaskRef>> = HashMap::new();
    let conn = db.lock();
    let mut stmt = match conn.prepare(
        "SELECT t.id, COALESCE(t.task_key,''), t.title, COALESCE(t.status,''), l.meta, l.created_at
         FROM task_logs l JOIN tasks t ON t.id = l.task_id
         WHERE t.project_id = ?1 AND l.log_type = 'tool'
         ORDER BY l.id ASC",
    ) {
        Ok(s) => s,
        Err(e) => {
            log::error!("markdown_writes_by_project: {}", e);
            return by_path;
        }
    };

    let rows = match stmt.query_map(params![project_id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
        ))
    }) {
        Ok(rows) => rows,
        Err(e) => {
            log::error!("markdown_writes_by_project: {}", e);
            return by_path;
        }
    };

    let wd = normalize_dir(working_dir);
    for (task_id, task_key, title, status, meta, created_at) in rows.flatten() {
        let Some(file) = meta.as_deref().and_then(logged_markdown_write) else {
            continue;
        };
        let Some(key) = relative_key(&file, &wd) else {
            continue;
        };

        let refs = by_path.entry(key).or_default();
        match refs.iter_mut().find(|r| r.task_id == task_id) {
            // Rows arrive oldest-first, so a later row is the more recent write.
            Some(existing) if created_at >= existing.written_at => {
                existing.written_at = created_at;
                existing.task_key = task_key;
                existing.title = title;
                existing.status = status;
            }
            Some(_) => {}
            None => refs.push(ArtifactTaskRef {
                task_id,
                task_key,
                title,
                status,
                written_at: created_at,
            }),
        }
    }

    for refs in by_path.values_mut() {
        refs.sort_by(|a, b| {
            b.written_at
                .cmp(&a.written_at)
                .then_with(|| b.task_id.cmp(&a.task_id))
        });
    }
    by_path
}

/// Returns the logged file path when `meta` describes a markdown write.
fn logged_markdown_write(meta: &str) -> Option<String> {
    let meta: serde_json::Value = serde_json::from_str(meta).ok()?;
    let tool = meta.get("toolName")?.as_str()?;
    if !WRITE_TOOLS.contains(&tool) {
        return None;
    }
    let file = meta.get("input")?.get("file")?.as_str()?;
    let lower = file.to_lowercase();
    if lower.ends_with(".md") || lower.ends_with(".mdx") {
        Some(file.to_string())
    } else {
        None
    }
}

fn normalize_dir(dir: &str) -> String {
    dir.replace('\\', "/").trim_end_matches('/').to_lowercase()
}

/// True for POSIX absolute paths, UNC paths, and Windows drive paths.
fn is_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with('/')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

/// Turns a logged file path into a lowercased, `working_dir`-relative map key,
/// or `None` when the path lies outside `working_dir`.
fn relative_key(file: &str, working_dir: &str) -> Option<String> {
    let path = file.replace('\\', "/").trim().to_lowercase();
    let rest = if is_absolute(&path) {
        if working_dir.is_empty() {
            return None;
        }
        path.strip_prefix(working_dir)?
            .strip_prefix('/')?
            .to_string()
    } else {
        path.trim_start_matches("./").to_string()
    };

    if rest.is_empty() || rest.split('/').any(|part| part == "..") {
        return None;
    }
    Some(rest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use rusqlite::Connection;
    use std::sync::Arc;

    const WORKING_DIR: &str = "/Users/dev/board";

    fn test_db() -> DbPool {
        let conn = Connection::open_in_memory().expect("in-memory db");
        crate::db::schema::create_tables(&conn);
        Arc::new(Mutex::new(conn))
    }

    fn insert_project(db: &DbPool) -> i64 {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO projects (name,slug,working_dir) VALUES ('Board','board',?1)",
            params![WORKING_DIR],
        )
        .expect("insert project");
        conn.last_insert_rowid()
    }

    fn insert_task(db: &DbPool, project_id: i64, key: &str, title: &str) -> i64 {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO tasks (project_id,title,status,task_key) VALUES (?1,?2,'done',?3)",
            params![project_id, title, key],
        )
        .expect("insert task");
        conn.last_insert_rowid()
    }

    fn insert_tool_log(db: &DbPool, task_id: i64, tool: &str, file: &str, created_at: &str) {
        let meta = serde_json::json!({
            "toolName": tool,
            "toolId": "toolu_test",
            "input": { "file": file },
        });
        let conn = db.lock();
        conn.execute(
            "INSERT INTO task_logs (task_id,message,log_type,meta,created_at) VALUES (?1,?2,'tool',?3,?4)",
            params![task_id, tool, meta.to_string(), created_at],
        )
        .expect("insert tool log");
    }

    fn writes(db: &DbPool, project_id: i64) -> HashMap<String, Vec<ArtifactTaskRef>> {
        markdown_writes_by_project(db, project_id, WORKING_DIR)
    }

    #[test]
    fn empty_project_returns_empty_map() {
        let db = test_db();
        let project_id = insert_project(&db);
        assert!(writes(&db, project_id).is_empty());
    }

    #[test]
    fn absolute_path_maps_to_relative_key() {
        let db = test_db();
        let project_id = insert_project(&db);
        let task_id = insert_task(&db, project_id, "BOARD-1", "Write the spec");
        insert_tool_log(
            &db,
            task_id,
            "Write",
            "/Users/dev/board/docs/Spec.md",
            "2026-08-05 10:00:00",
        );

        let result = writes(&db, project_id);
        let refs = result.get("docs/spec.md").expect("key present");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].task_id, task_id);
        assert_eq!(refs[0].task_key, "BOARD-1");
        assert_eq!(refs[0].title, "Write the spec");
        assert_eq!(refs[0].status, "done");
        assert_eq!(refs[0].written_at.as_deref(), Some("2026-08-05 10:00:00"));
    }

    #[test]
    fn relative_path_maps_too() {
        let db = test_db();
        let project_id = insert_project(&db);
        let task_id = insert_task(&db, project_id, "BOARD-1", "Notes");
        insert_tool_log(
            &db,
            task_id,
            "Edit",
            "./docs/Notes.md",
            "2026-08-05 10:00:00",
        );

        let result = writes(&db, project_id);
        assert_eq!(result.get("docs/notes.md").map(Vec::len), Some(1));
    }

    #[test]
    fn read_only_tool_is_ignored() {
        let db = test_db();
        let project_id = insert_project(&db);
        let task_id = insert_task(&db, project_id, "BOARD-1", "Read the spec");
        insert_tool_log(
            &db,
            task_id,
            "Read",
            "/Users/dev/board/docs/spec.md",
            "2026-08-05 10:00:00",
        );

        assert!(writes(&db, project_id).is_empty());
    }

    #[test]
    fn non_markdown_write_is_ignored() {
        let db = test_db();
        let project_id = insert_project(&db);
        let task_id = insert_task(&db, project_id, "BOARD-1", "Ship the feature");
        insert_tool_log(
            &db,
            task_id,
            "Write",
            "/Users/dev/board/src/main.rs",
            "2026-08-05 10:00:00",
        );

        assert!(writes(&db, project_id).is_empty());
    }

    #[test]
    fn two_tasks_writing_one_file_are_ordered_newest_first() {
        let db = test_db();
        let project_id = insert_project(&db);
        let older = insert_task(&db, project_id, "BOARD-1", "Draft");
        let newer = insert_task(&db, project_id, "BOARD-2", "Revise");
        insert_tool_log(
            &db,
            older,
            "Write",
            "/Users/dev/board/docs/spec.md",
            "2026-08-05 10:00:00",
        );
        insert_tool_log(&db, newer, "Edit", "docs/spec.md", "2026-08-05 12:00:00");

        let result = writes(&db, project_id);
        let refs = result.get("docs/spec.md").expect("key present");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].task_id, newer);
        assert_eq!(refs[1].task_id, older);
    }

    /// Repeated writes from one task collapse to its most recent one.
    #[test]
    fn repeated_writes_from_one_task_dedupe_to_latest() {
        let db = test_db();
        let project_id = insert_project(&db);
        let task_id = insert_task(&db, project_id, "BOARD-1", "Iterate");
        insert_tool_log(&db, task_id, "Write", "docs/spec.md", "2026-08-05 10:00:00");
        insert_tool_log(&db, task_id, "Edit", "docs/spec.md", "2026-08-05 11:30:00");

        let result = writes(&db, project_id);
        let refs = result.get("docs/spec.md").expect("key present");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].written_at.as_deref(), Some("2026-08-05 11:30:00"));
    }

    #[test]
    fn path_outside_working_dir_is_dropped() {
        let db = test_db();
        let project_id = insert_project(&db);
        let task_id = insert_task(&db, project_id, "BOARD-1", "Stray write");
        insert_tool_log(
            &db,
            task_id,
            "Write",
            "/Users/dev/other/docs/spec.md",
            "2026-08-05 10:00:00",
        );

        assert!(writes(&db, project_id).is_empty());
    }

    #[test]
    fn other_projects_logs_are_excluded() {
        let db = test_db();
        let project_id = insert_project(&db);
        let other_project = {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO projects (name,slug,working_dir) VALUES ('Other','other',?1)",
                params![WORKING_DIR],
            )
            .expect("insert project");
            conn.last_insert_rowid()
        };
        let task_id = insert_task(&db, other_project, "OTHER-1", "Elsewhere");
        insert_tool_log(&db, task_id, "Write", "docs/spec.md", "2026-08-05 10:00:00");

        assert!(writes(&db, project_id).is_empty());
    }

    #[test]
    fn windows_separators_and_case_are_normalized() {
        let db = test_db();
        let project_id = insert_project(&db);
        let task_id = insert_task(&db, project_id, "BOARD-1", "Windows write");
        insert_tool_log(
            &db,
            task_id,
            "MultiEdit",
            r"/USERS/DEV/BOARD\docs\Spec.MD",
            "2026-08-05 10:00:00",
        );

        let result = writes(&db, project_id);
        assert_eq!(result.get("docs/spec.md").map(Vec::len), Some(1));
    }

    #[test]
    fn malformed_meta_is_skipped() {
        let db = test_db();
        let project_id = insert_project(&db);
        let task_id = insert_task(&db, project_id, "BOARD-1", "Broken log");
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO task_logs (task_id,message,log_type,meta) VALUES (?1,'Write','tool','not json')",
                params![task_id],
            )
            .expect("insert tool log");
            conn.execute(
                "INSERT INTO task_logs (task_id,message,log_type) VALUES (?1,'Write','tool')",
                params![task_id],
            )
            .expect("insert tool log without meta");
        }

        assert!(writes(&db, project_id).is_empty());
    }
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
        )
        .unwrap();
        let second = insert_or_replace(
            &db,
            p,
            "docs/plan.md",
            "1-plan.md",
            &derived("Plan v2", "changed", "plan", 300),
            second_author,
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
        )
        .unwrap();
        insert_or_replace(
            &db,
            p,
            "docs/b.md",
            "2-b.md",
            &derived("B", "", "doc", 1),
            t,
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
        )
        .unwrap();
        let two = insert_or_replace(
            &db,
            b,
            "README.md",
            "2-readme.md",
            &DerivedMeta::default(),
            tb,
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
        insert_or_replace(&db, p, "docs/x.md", "1-x.md", &DerivedMeta::default(), t).unwrap();

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
        let id =
            insert_or_replace(&db, p, "docs/d.md", "1-d.md", &DerivedMeta::default(), t).unwrap();

        delete(&db, id).unwrap();

        assert!(get(&db, id).is_none());
    }
}
