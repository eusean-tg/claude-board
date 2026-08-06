//! Commands backing "chat about this".
//!
//! Two halves that stay apart on purpose. Posting a message only writes a row —
//! no worktree, no branch, no process — so reconsidering the approach costs none
//! of the work already done. Resuming is the separate, deliberate act of sending
//! the agent back in with the conversation.
//!
//! A discussion is not a revision. `request_revision` bumps `revision_count`,
//! which feeds `max_auto_revisions` and can fail a task for asking too often.
//! Talking about the approach leaves those counters alone.

use crate::claude::state_machine::{is_valid_transition, TaskStatus};
use crate::db::discussion::{self, DiscussionMessage};
use crate::db::{self, tasks as tq, DbPool};

/// The whole thread for a task, oldest first.
#[tauri::command]
pub fn get_discussion(task_id: i64) -> Vec<DiscussionMessage> {
    discussion::for_task(&db::get_db(), task_id)
}

/// Add the user's message to the thread.
///
/// Deliberately does nothing else. The agent is not started, the worktree is not
/// touched, and the task does not change status — a half-formed thought should not
/// launch a run.
#[tauri::command]
pub fn post_discussion_message(
    task_id: i64,
    body: String,
) -> Result<Vec<DiscussionMessage>, String> {
    let db = db::get_db();
    post_discussion_message_in(&db, task_id, &body)?;
    Ok(discussion::for_task(&db, task_id))
}

pub(crate) fn post_discussion_message_in(
    db: &DbPool,
    task_id: i64,
    body: &str,
) -> Result<i64, String> {
    if tq::get_by_id(db, task_id).is_none() {
        return Err("task not found".to_string());
    }
    discussion::post(db, task_id, discussion::ROLE_USER, body).map_err(|e| e.to_string())
}

/// Send the agent back in with the conversation.
///
/// Reuses the worktree, so the work done before the discussion survives, and
/// leaves `revision_count` alone.
#[tauri::command]
pub fn resume_with_discussion(
    app: tauri::AppHandle,
    task_id: i64,
    mcp_port: u16,
) -> Result<(), String> {
    let db = db::get_db();
    let task = tq::get_by_id(&db, task_id).ok_or("task not found")?;
    let project =
        crate::db::projects::get_by_id(&db, task.project_id).ok_or("project not found")?;

    let thread = discussion::transcript(&db, task_id);
    if thread.is_empty() {
        return Err("there is nothing in the discussion yet".to_string());
    }

    let from = TaskStatus::from_str(task.status.as_deref().unwrap_or("backlog"))
        .unwrap_or(TaskStatus::Backlog);
    if from != TaskStatus::InProgress && !is_valid_transition(from, TaskStatus::InProgress) {
        return Err(format!("a task that is {} cannot be started", from));
    }

    // Any question still open is closed first. The agent is being redirected, so
    // the question it asked is moot, and leaving it open would refuse its next one.
    super::blockers::cancel_open_blocker_for_task(&db, task_id);

    tq::update_status(&db, task_id, TaskStatus::InProgress.as_str());
    tq::set_resumed(&db, task_id);
    let updated = tq::get_by_id(&db, task_id).ok_or("task not found after the status change")?;

    if crate::claude::runner::resume_with_discussion(
        &updated,
        app.clone(),
        &project.working_dir,
        &project,
        mcp_port,
        &thread,
    ) {
        use tauri::Emitter;
        app.emit("task:updated", &updated).ok();
        Ok(())
    } else {
        // Back to where it was. An in_progress task with nothing running is reset
        // to backlog on the next launch.
        tq::update_status(&db, task_id, from.as_str());
        if from == TaskStatus::Blocked {
            tq::pause_timer(&db, task_id);
        }
        Err("the runner refused to start".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use rusqlite::{params, Connection};
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::sync::Arc;

    fn test_db() -> DbPool {
        let conn = Connection::open_in_memory().expect("in-memory db");
        crate::db::schema::create_tables(&conn);
        crate::db::schema::run_migrations(&conn);
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        Arc::new(Mutex::new(conn))
    }

    fn seed_task(db: &DbPool, id: i64, status: &str) -> i64 {
        let conn = db.lock();
        conn.execute(
            "INSERT OR IGNORE INTO projects (id,name,slug,working_dir)
             VALUES (1,'B','b','/repo')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id,project_id,title,status) VALUES (?1,1,'t',?2)",
            params![id, status],
        )
        .unwrap();
        id
    }

    fn git(args: &[&str], dir: &Path) -> bool {
        let mut cmd = crate::child_env::command("git");
        cmd.args(args)
            .current_dir(dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd.output().map(|o| o.status.success()).unwrap_or(false)
    }

    fn rev_parse(dir: &Path, rev: &str) -> String {
        let mut cmd = crate::child_env::command("git");
        let out = cmd
            .args(["rev-parse", rev])
            .current_dir(dir)
            .output()
            .expect("rev-parse");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn repo(suffix: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("cb-discussion-{}-{}", std::process::id(), suffix));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        assert!(git(&["init", "--quiet"], &root));
        git(&["config", "user.email", "test@example.com"], &root);
        git(&["config", "user.name", "Test"], &root);
        std::fs::write(root.join("seed.txt"), "seed\n").unwrap();
        git(&["add", "."], &root);
        assert!(git(&["commit", "--quiet", "-m", "seed"], &root));
        git(&["branch", "-M", "main"], &root);
        root
    }

    #[test]
    fn discussing_does_not_touch_the_worktree_or_the_branch() {
        let root = repo("no-side-effects");
        let wt = root.join(".worktrees").join("task-7");
        assert!(git(
            &[
                "worktree",
                "add",
                "-b",
                "feature/task-7",
                &wt.to_string_lossy(),
                "main"
            ],
            &root,
        ));
        std::fs::write(wt.join("wip.txt"), "half-done\n").unwrap();
        let head_before = rev_parse(&root, "HEAD");
        let db = test_db();
        let t = seed_task(&db, 7, "blocked");

        post_discussion_message_in(&db, t, "let's reconsider the schema").unwrap();

        // "Go back to the drawing board" must cost none of the work.
        assert!(wt.join("wip.txt").exists());
        assert_eq!(rev_parse(&root, "HEAD"), head_before);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn discussing_leaves_the_task_alone() {
        let db = test_db();
        let t = seed_task(&db, 1101, "blocked");

        post_discussion_message_in(&db, t, "what if we cached it instead?").unwrap();

        // A half-formed thought must not launch a run or move the task.
        let task = tq::get_by_id(&db, t).unwrap();
        assert_eq!(task.status.as_deref(), Some("blocked"));
        assert_eq!(task.revision_count.unwrap_or(0), 0);
    }

    #[test]
    fn a_discussion_is_not_a_revision() {
        let db = test_db();
        let t = seed_task(&db, 1102, "blocked");

        post_discussion_message_in(&db, t, "one").unwrap();
        post_discussion_message_in(&db, t, "two").unwrap();

        // revision_count feeds max_auto_revisions, which can fail a task for
        // needing too many. Talking is not a rejection.
        assert_eq!(
            tq::get_by_id(&db, t).unwrap().revision_count.unwrap_or(0),
            0
        );
        assert!(tq::get_revisions(&db, t).is_empty());
        assert_eq!(discussion::for_task(&db, t).len(), 2);
    }

    #[test]
    fn an_empty_message_is_refused() {
        let db = test_db();
        let t = seed_task(&db, 1103, "blocked");

        assert!(post_discussion_message_in(&db, t, "   ").is_err());
        assert!(discussion::for_task(&db, t).is_empty());
    }

    #[test]
    fn a_message_for_a_missing_task_is_refused() {
        let db = test_db();

        // The task may have been deleted while the panel was open.
        assert!(post_discussion_message_in(&db, 9_999, "hello").is_err());
    }

    #[test]
    fn the_thread_reads_back_in_order_through_the_command_layer() {
        let db = test_db();
        let t = seed_task(&db, 1104, "blocked");

        post_discussion_message_in(&db, t, "first").unwrap();
        post_discussion_message_in(&db, t, "second").unwrap();

        let bodies: Vec<String> = discussion::for_task(&db, t)
            .into_iter()
            .map(|m| m.body)
            .collect();
        assert_eq!(bodies, vec!["first", "second"]);
    }
}
