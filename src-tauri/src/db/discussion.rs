//! A conversation about a task's direction.
//!
//! Distinct from `task_revisions`, which records "that was wrong, do it again" and
//! feeds `max_auto_revisions`. A discussion says "let's reconsider the approach"
//! and leaves those counters alone. Neither discards work, but only a discussion
//! is available while a task is blocked mid-run.
//!
//! Storage only. Nothing here touches a worktree, a branch or a running process —
//! which is the property that makes "go back to the drawing board" cost nothing.

use super::DbPool;
use crate::error::AppError;
use rusqlite::params;
use serde::Serialize;

pub const ROLE_USER: &str = "user";
pub const ROLE_AGENT: &str = "agent";

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct DiscussionMessage {
    pub id: i64,
    pub task_id: i64,
    pub role: String,
    pub body: String,
    pub created_at: Option<String>,
}

/// Add a message to a task's thread.
///
/// An empty body is refused: it would render as a blank turn and tell a resumed
/// agent nothing.
pub fn post(db: &DbPool, task_id: i64, role: &str, body: &str) -> Result<i64, AppError> {
    let body = body.trim();
    if body.is_empty() {
        return Err(AppError::Validation("a message needs a body".to_string()));
    }
    if role != ROLE_USER && role != ROLE_AGENT {
        return Err(AppError::Validation(format!("unknown role: {}", role)));
    }
    let conn = db.lock();
    conn.execute(
        "INSERT INTO task_discussion_messages (task_id, role, body) VALUES (?1, ?2, ?3)",
        params![task_id, role, body],
    )?;
    Ok(conn.last_insert_rowid())
}

/// A task's thread, oldest first, which is reading order and prompt order.
pub fn for_task(db: &DbPool, task_id: i64) -> Vec<DiscussionMessage> {
    let conn = db.lock();
    let mut stmt = match conn.prepare(
        "SELECT id, task_id, role, body, created_at FROM task_discussion_messages
         WHERE task_id=?1 ORDER BY id",
    ) {
        Ok(s) => s,
        Err(e) => {
            log::error!("discussion for_task: {}", e);
            return vec![];
        }
    };
    let mut out = Vec::new();
    match stmt.query_map(params![task_id], |r| {
        Ok(DiscussionMessage {
            id: r.get(0)?,
            task_id: r.get(1)?,
            role: r.get(2)?,
            body: r.get(3)?,
            created_at: r.get(4).ok().flatten(),
        })
    }) {
        Ok(rows) => out.extend(rows.filter_map(|r| r.ok())),
        Err(e) => log::error!("discussion for_task: {}", e),
    }
    out
}

/// The thread as the transcript a resumed agent reads.
///
/// Empty when there is nothing to say, so a caller can tell "no discussion" from
/// "a discussion saying nothing" without a second query.
pub fn transcript(db: &DbPool, task_id: i64) -> String {
    for_task(db, task_id)
        .iter()
        .map(|m| {
            let who = if m.role == ROLE_AGENT { "You" } else { "User" };
            format!("{}: {}", who, m.body)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use rusqlite::Connection;
    use std::sync::Arc;

    fn test_db() -> DbPool {
        let conn = Connection::open_in_memory().expect("in-memory db");
        crate::db::schema::create_tables(&conn);
        crate::db::schema::run_migrations(&conn);
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        Arc::new(Mutex::new(conn))
    }

    fn seed_task(db: &DbPool, id: i64) -> i64 {
        let conn = db.lock();
        conn.execute(
            "INSERT OR IGNORE INTO projects (id,name,slug,working_dir)
             VALUES (1,'B','b','/repo')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id,project_id,title,status) VALUES (?1,1,'t','blocked')",
            params![id],
        )
        .unwrap();
        id
    }

    #[test]
    fn messages_come_back_in_the_order_they_were_written() {
        let db = test_db();
        let t = seed_task(&db, 1001);

        post(&db, t, ROLE_USER, "let's reconsider the schema").unwrap();
        post(
            &db,
            t,
            ROLE_AGENT,
            "the join table was for the many-to-many",
        )
        .unwrap();
        post(&db, t, ROLE_USER, "keep it, but drop the cascade").unwrap();

        // Oldest first: a conversation read backwards means something else.
        let messages = for_task(&db, t);
        let bodies: Vec<&str> = messages.iter().map(|m| m.body.as_str()).collect();
        assert_eq!(
            bodies,
            vec![
                "let's reconsider the schema",
                "the join table was for the many-to-many",
                "keep it, but drop the cascade"
            ]
        );
    }

    #[test]
    fn an_empty_message_is_refused() {
        let db = test_db();
        let t = seed_task(&db, 1002);

        // A blank turn renders as an empty bubble and tells a resumed agent nothing.
        assert!(post(&db, t, ROLE_USER, "   ").is_err());
        assert!(post(&db, t, ROLE_USER, "").is_err());
        assert!(for_task(&db, t).is_empty());
    }

    #[test]
    fn an_unknown_role_is_refused() {
        let db = test_db();
        let t = seed_task(&db, 1003);

        // The CHECK constraint would catch it, but not with a message worth reading.
        assert!(post(&db, t, "system", "hello").is_err());
    }

    #[test]
    fn a_message_is_stored_trimmed() {
        let db = test_db();
        let t = seed_task(&db, 1004);

        post(&db, t, ROLE_USER, "  use the cache  \n").unwrap();

        assert_eq!(for_task(&db, t)[0].body, "use the cache");
    }

    #[test]
    fn the_transcript_names_the_speakers_from_the_agents_point_of_view() {
        let db = test_db();
        let t = seed_task(&db, 1005);
        post(&db, t, ROLE_USER, "why the join table?").unwrap();
        post(&db, t, ROLE_AGENT, "for the many-to-many").unwrap();

        // The transcript goes into the agent's own prompt, so its turns read as
        // "You" rather than as a third party.
        assert_eq!(
            transcript(&db, t),
            "User: why the join table?\n\nYou: for the many-to-many"
        );
    }

    #[test]
    fn a_task_with_no_discussion_has_an_empty_transcript() {
        let db = test_db();
        let t = seed_task(&db, 1006);

        assert_eq!(transcript(&db, t), "");
        assert!(for_task(&db, t).is_empty());
    }

    #[test]
    fn deleting_the_task_takes_its_discussion() {
        let db = test_db();
        let t = seed_task(&db, 1007);
        post(&db, t, ROLE_USER, "still here?").unwrap();

        {
            let conn = db.lock();
            conn.execute("DELETE FROM tasks WHERE id=?1", params![t])
                .unwrap();
        }

        assert!(for_task(&db, t).is_empty());
    }
}
