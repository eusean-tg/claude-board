//! Groups of tasks running a dependency chain together on a shared trunk branch.
//!
//! Persisted state with a lifecycle, which is why it is separate from
//! `dependencies.rs`: that module answers questions about the graph, this one owns
//! rows that are created when a chain starts and released when it lands.
//!
//! The point of a group is the trunk. Every member's worktree branches from the
//! trunk rather than the base branch, so a dependent task's checkout already
//! contains its dependencies' merged work. Without that a dependent task branches
//! from the base and cannot see what it was told it depends on.

use super::DbPool;
use crate::error::AppError;
use rusqlite::{params, Row};
use serde::Serialize;

pub const STATUS_ACTIVE: &str = "active";
pub const STATUS_COMPLETED: &str = "completed";
pub const STATUS_FAILED: &str = "failed";

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct TaskGroup {
    pub id: i64,
    pub project_id: i64,
    pub trunk_branch: String,
    pub base_branch: String,
    pub target_task_id: i64,
    pub status: String,
    pub created_at: Option<String>,
}

fn row_to_group(row: &Row) -> rusqlite::Result<TaskGroup> {
    Ok(TaskGroup {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        trunk_branch: row.get("trunk_branch")?,
        base_branch: row.get("base_branch")?,
        target_task_id: row.get("target_task_id")?,
        status: row
            .get::<_, Option<String>>("status")?
            .unwrap_or_else(|| STATUS_ACTIVE.into()),
        created_at: row.get("created_at").ok().flatten(),
    })
}

/// Start a group over `member_ids`, with `target_task_id` as the task the user
/// asked for.
///
/// The group and its membership are written in one transaction, so a member
/// already claimed by a live group leaves no half-built group behind.
pub fn create(
    db: &DbPool,
    project_id: i64,
    trunk_branch: &str,
    base_branch: &str,
    target_task_id: i64,
    member_ids: &[i64],
) -> Result<i64, AppError> {
    if member_ids.is_empty() {
        return Err(AppError::Validation(
            "a group needs at least one member".to_string(),
        ));
    }
    if !member_ids.contains(&target_task_id) {
        // The target is what the group exists to deliver, and Task 7 reads it to
        // decide when the trunk may land.
        return Err(AppError::Validation(
            "the target task must be a member of its own group".to_string(),
        ));
    }

    super::with_transaction(db, |conn| {
        conn.execute(
            "INSERT INTO task_groups (project_id, trunk_branch, base_branch, target_task_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![project_id, trunk_branch, base_branch, target_task_id],
        )
        .map_err(|e| e.to_string())?;
        let group_id = conn.last_insert_rowid();
        for id in member_ids {
            // A plain INSERT, not INSERT OR IGNORE: a task already in a live group
            // is a caller bug, and swallowing it would give that task two trunks.
            conn.execute(
                "INSERT INTO task_group_members (group_id, task_id) VALUES (?1, ?2)",
                params![group_id, id],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(group_id)
    })
    .map_err(AppError::Database)
}

/// A group by id, whatever its status.
pub fn get(db: &DbPool, group_id: i64) -> Option<TaskGroup> {
    let conn = db.lock();
    conn.query_row(
        "SELECT * FROM task_groups WHERE id=?1",
        params![group_id],
        row_to_group,
    )
    .ok()
}

/// The live group a task belongs to, if any.
///
/// Restricted to `active` on purpose. Callers ask this to decide which branch to
/// build on and where to merge, and a finished group's trunk has already been
/// deleted — handing it back would branch a task from a ref that no longer exists.
pub fn for_task(db: &DbPool, task_id: i64) -> Option<TaskGroup> {
    let conn = db.lock();
    conn.query_row(
        "SELECT g.* FROM task_groups g
         JOIN task_group_members m ON m.group_id = g.id
         WHERE m.task_id = ?1 AND g.status = 'active'",
        params![task_id],
        row_to_group,
    )
    .ok()
}

/// Task ids in a group, in the order they were added.
pub fn members(db: &DbPool, group_id: i64) -> Vec<i64> {
    let conn = db.lock();
    let mut stmt = match conn
        .prepare("SELECT task_id FROM task_group_members WHERE group_id=?1 ORDER BY rowid")
    {
        Ok(s) => s,
        Err(e) => {
            log::error!("group members: {}", e);
            return vec![];
        }
    };
    let mut out: Vec<i64> = Vec::new();
    match stmt.query_map(params![group_id], |r| r.get::<_, i64>(0)) {
        Ok(rows) => out.extend(rows.flatten()),
        Err(e) => log::error!("group members: {}", e),
    }
    out
}

pub fn set_status(db: &DbPool, group_id: i64, status: &str) -> Result<(), AppError> {
    if !matches!(status, STATUS_ACTIVE | STATUS_COMPLETED | STATUS_FAILED) {
        return Err(AppError::Validation(format!("unknown status: {}", status)));
    }
    let conn = db.lock();
    conn.execute(
        "UPDATE task_groups SET status=?2, updated_at=datetime('now','localtime') WHERE id=?1",
        params![group_id, status],
    )?;
    Ok(())
}

/// Close a group and let its tasks join a later one.
///
/// Membership is operational state rather than history: it says "this task builds
/// on that trunk", which stops being true once the trunk is gone. Keeping the rows
/// would make `UNIQUE(task_id)` permanent and refuse to ever re-run a task in a new
/// chain. The group row itself survives, with its target and outcome.
pub fn finish(db: &DbPool, group_id: i64, status: &str) -> Result<(), AppError> {
    set_status(db, group_id, status)?;
    let conn = db.lock();
    conn.execute(
        "DELETE FROM task_group_members WHERE group_id=?1",
        params![group_id],
    )?;
    Ok(())
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
        conn.execute(
            "INSERT INTO projects (id,name,slug,working_dir) VALUES (1,'B','b','/repo')",
            [],
        )
        .unwrap();
        Arc::new(Mutex::new(conn))
    }

    fn seed_task(db: &DbPool, title: &str) -> i64 {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO tasks (project_id,title,status) VALUES (1,?1,'backlog')",
            params![title],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn a_task_belongs_to_at_most_one_live_group() {
        let db = test_db();
        let t = seed_task(&db, "t");
        create(&db, 1, "trunk/a", "main", t, &[t]).unwrap();

        // Two trunks for one task has no correct merge target.
        assert!(create(&db, 1, "trunk/b", "main", t, &[t]).is_err());
    }

    #[test]
    fn a_refused_group_leaves_nothing_behind() {
        let db = test_db();
        let claimed = seed_task(&db, "claimed");
        let free = seed_task(&db, "free");
        create(&db, 1, "trunk/a", "main", claimed, &[claimed]).unwrap();

        // `free` is inserted before the conflict on `claimed` is reached.
        assert!(create(&db, 1, "trunk/b", "main", free, &[free, claimed]).is_err());

        assert!(
            for_task(&db, free).is_none(),
            "the rolled-back member is free"
        );
        let conn = db.lock();
        let groups: i64 = conn
            .query_row("SELECT COUNT(*) FROM task_groups", [], |r| r.get(0))
            .unwrap();
        assert_eq!(groups, 1, "no half-built group");
    }

    #[test]
    fn for_task_finds_the_group_from_any_member() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let b = seed_task(&db, "b");
        let id = create(&db, 1, "trunk/x", "main", b, &[a, b]).unwrap();

        assert_eq!(for_task(&db, a).map(|g| g.id), Some(id));
        assert_eq!(for_task(&db, b).unwrap().trunk_branch, "trunk/x");
    }

    #[test]
    fn a_finished_group_stops_answering_for_its_tasks() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let id = create(&db, 1, "trunk/gone", "main", a, &[a]).unwrap();

        finish(&db, id, STATUS_COMPLETED).unwrap();

        // Its trunk was deleted when it landed. Handing it back would branch the
        // next run from a ref that no longer exists.
        assert!(for_task(&db, a).is_none());
        // The group itself survives as a record of what ran.
        assert_eq!(get(&db, id).unwrap().status, STATUS_COMPLETED);
    }

    #[test]
    fn a_group_marked_finished_without_releasing_still_stops_answering() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let id = create(&db, 1, "trunk/half", "main", a, &[a]).unwrap();

        // set_status is public and does not release membership. The status filter in
        // for_task is what stops a caller that took this route from being handed a
        // trunk that has already been deleted.
        set_status(&db, id, STATUS_COMPLETED).unwrap();

        assert!(for_task(&db, a).is_none());
        assert_eq!(
            members(&db, id),
            vec![a],
            "membership is untouched by set_status"
        );
    }

    #[test]
    fn a_task_can_join_a_later_group_once_the_first_has_finished() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let first = create(&db, 1, "trunk/one", "main", a, &[a]).unwrap();
        finish(&db, first, STATUS_COMPLETED).unwrap();

        // Keeping membership forever would make UNIQUE(task_id) permanent and
        // refuse to ever run this task in another chain.
        let second = create(&db, 1, "trunk/two", "main", a, &[a]).unwrap();
        assert_eq!(for_task(&db, a).map(|g| g.id), Some(second));
    }

    #[test]
    fn a_failed_group_also_releases_its_tasks() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let id = create(&db, 1, "trunk/f", "main", a, &[a]).unwrap();

        finish(&db, id, STATUS_FAILED).unwrap();

        // A group whose trunk could not land keeps the trunk for the user, but the
        // tasks must not stay claimed by it forever.
        assert!(for_task(&db, a).is_none());
        assert_eq!(get(&db, id).unwrap().status, STATUS_FAILED);
    }

    #[test]
    fn members_come_back_in_the_order_they_were_added() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let b = seed_task(&db, "b");
        let c = seed_task(&db, "c");
        let id = create(&db, 1, "trunk/o", "main", c, &[a, b, c]).unwrap();

        // Leaves-first order is what the caller passed and what a display wants.
        assert_eq!(members(&db, id), vec![a, b, c]);
    }

    #[test]
    fn the_target_must_be_one_of_the_members() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let outsider = seed_task(&db, "outsider");

        // Task 7 reads target_task_id to decide when the trunk may land; a target
        // outside the group would never be reached.
        assert!(create(&db, 1, "trunk/z", "main", outsider, &[a]).is_err());
    }

    #[test]
    fn an_empty_group_is_refused() {
        let db = test_db();
        let a = seed_task(&db, "a");

        assert!(create(&db, 1, "trunk/e", "main", a, &[]).is_err());
    }

    #[test]
    fn an_unknown_status_is_refused() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let id = create(&db, 1, "trunk/s", "main", a, &[a]).unwrap();

        assert!(set_status(&db, id, "whatever").is_err());
        assert_eq!(get(&db, id).unwrap().status, STATUS_ACTIVE);
    }

    #[test]
    fn deleting_a_member_task_drops_its_membership() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let b = seed_task(&db, "b");
        let id = create(&db, 1, "trunk/d", "main", b, &[a, b]).unwrap();

        {
            let conn = db.lock();
            conn.execute("DELETE FROM tasks WHERE id=?1", params![a])
                .unwrap();
        }

        assert_eq!(members(&db, id), vec![b]);
        assert!(for_task(&db, a).is_none());
    }

    #[test]
    fn deleting_the_target_task_takes_the_group_with_it() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let b = seed_task(&db, "b");
        let id = create(&db, 1, "trunk/t", "main", b, &[a, b]).unwrap();

        {
            let conn = db.lock();
            conn.execute("DELETE FROM tasks WHERE id=?1", params![b])
                .unwrap();
        }

        // Without its target the group has no landing condition, so it is gone and
        // its remaining members are free.
        assert!(get(&db, id).is_none());
        assert!(for_task(&db, a).is_none());
    }
}
