//! Attributes markdown files to the tasks that wrote them.
//!
//! Every tool call Claude makes is logged to `task_logs` with `log_type='tool'`
//! and a `meta` JSON blob carrying the tool name and its file argument (see the
//! `"tool_use"` arm of `crate::claude::events`). Replaying those rows tells us
//! which task last touched which markdown file, without storing anything extra.

use super::DbPool;
use rusqlite::params;
use serde::Serialize;
use std::collections::HashMap;

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
