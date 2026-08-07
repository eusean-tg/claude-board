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
/// The run needs a person before it can go on: a member's work could not reach the
/// trunk, so the tasks after it were not started.
///
/// Distinct from `failed`, which is a run that is over. A stopped run keeps its
/// membership, because that is what the board reads to say which cards are waiting
/// on it and what a resolution has to act on.
pub const STATUS_STOPPED: &str = "stopped";

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct TaskGroup {
    pub id: i64,
    pub project_id: i64,
    pub trunk_branch: String,
    pub base_branch: String,
    pub target_task_id: i64,
    pub status: String,
    /// The task created to resolve the conflict that stopped this run, if one was.
    pub resolve_task_id: Option<i64>,
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
        // `.ok().flatten()` like created_at: a row read before the migration ran
        // has no such column, and a group without a resolve task is the norm.
        resolve_task_id: row.get("resolve_task_id").ok().flatten(),
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

/// Add a task to a run that is already going.
///
/// A plain INSERT, exactly as inside [`create`]: `UNIQUE(task_id)` makes claiming an
/// already-claimed task an error, and swallowing it would give that task two trunks.
///
/// The member joins at the end. `members` is rowid-ordered and the callers that walk
/// it — merging member branches onto the trunk — want the newcomer's work considered
/// after the work it was added to deal with.
pub fn add_member(db: &DbPool, group_id: i64, task_id: i64) -> Result<(), AppError> {
    let conn = db.lock();
    conn.execute(
        "INSERT INTO task_group_members (group_id, task_id) VALUES (?1, ?2)",
        params![group_id, task_id],
    )?;
    Ok(())
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

/// What a card needs to know about the run its task belongs to: the trunk, the run's
/// status, and the task resolving its conflict if one was created.
pub type RunForTask = (String, String, Option<i64>);

/// Trunk branch and run status per task, for every task in a live or stopped group.
///
/// Stopped runs are included because a card whose run needs attention is exactly
/// what the board has to show; filtering to active made the marker vanish at the
/// moment it mattered most.
///
/// One query for the whole board rather than `for_task` per card.
pub fn trunks_by_task(db: &DbPool, project_id: i64) -> std::collections::HashMap<i64, RunForTask> {
    let conn = db.lock();
    let mut stmt = match conn.prepare(
        "SELECT m.task_id, g.trunk_branch, g.status, g.resolve_task_id FROM task_groups g
         JOIN task_group_members m ON m.group_id = g.id
         WHERE g.project_id = ?1 AND g.status IN ('active','stopped')",
    ) {
        Ok(s) => s,
        Err(e) => {
            log::error!("trunks_by_task: {}", e);
            return std::collections::HashMap::new();
        }
    };
    let mut out = std::collections::HashMap::new();
    match stmt.query_map(params![project_id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            (
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ),
        ))
    }) {
        Ok(rows) => out.extend(rows.flatten()),
        Err(e) => log::error!("trunks_by_task: {}", e),
    }
    out
}

/// The stopped run a task belongs to, if any.
///
/// Deliberately not a status parameter on [`for_task`]: the two callers want opposite
/// things — one asks "is this run live", the other "is this run waiting for me" — and
/// a bool at the call site reads as neither.
pub fn stopped_for_task(db: &DbPool, task_id: i64) -> Option<TaskGroup> {
    let conn = db.lock();
    conn.query_row(
        "SELECT g.* FROM task_groups g
         JOIN task_group_members m ON m.group_id = g.id
         WHERE m.task_id = ?1 AND g.status = 'stopped'",
        params![task_id],
        row_to_group,
    )
    .ok()
}

/// Trunk branch and run status for one task's live or stopped run.
///
/// The single-task counterpart of [`trunks_by_task`], for the paths that emit one
/// task rather than a board's worth.
pub fn run_for_task(db: &DbPool, task_id: i64) -> Option<RunForTask> {
    let conn = db.lock();
    conn.query_row(
        "SELECT g.trunk_branch, g.status, g.resolve_task_id FROM task_groups g
         JOIN task_group_members m ON m.group_id = g.id
         WHERE m.task_id = ?1 AND g.status IN ('active','stopped')",
        params![task_id],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<i64>>(2)?,
            ))
        },
    )
    .ok()
}

/// The trunk a task's work belongs on, whether or not its run is still live.
///
/// Branch cleanup needs this rather than [`for_task`]: a member still running when
/// the run stopped has to merge onto the trunk when it finishes like every other
/// member. Resolving no trunk would send it at the project's base branch instead.
pub fn trunk_for_task(db: &DbPool, task_id: i64) -> Option<String> {
    let conn = db.lock();
    conn.query_row(
        "SELECT g.trunk_branch FROM task_groups g
         JOIN task_group_members m ON m.group_id = g.id
         WHERE m.task_id = ?1 AND g.status IN ('active','stopped')",
        params![task_id],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

/// Close a stopped run and release its tasks, so they can be claimed again.
///
/// The escape hatch for a run nobody is going to resolve: without it a stopped
/// run's members are claimed for good and can never be started in another chain.
pub fn abandon_stopped(db: &DbPool, task_ids: &[i64]) -> Vec<i64> {
    let stopped: Vec<i64> = {
        let conn = db.lock();
        let mut ids = Vec::new();
        for id in task_ids {
            if let Ok(gid) = conn.query_row(
                "SELECT g.id FROM task_groups g
                 JOIN task_group_members m ON m.group_id = g.id
                 WHERE m.task_id = ?1 AND g.status = 'stopped'",
                params![id],
                |r| r.get::<_, i64>(0),
            ) {
                if !ids.contains(&gid) {
                    ids.push(gid);
                }
            }
        }
        ids
    };
    for gid in &stopped {
        if let Err(e) = finish(db, *gid, STATUS_FAILED) {
            log::error!("could not abandon stopped group {}: {}", gid, e);
        }
    }
    stopped
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

/// Record the task created to resolve this run's conflict.
///
/// Written once, never cleared: the run's members are released when it finishes, so
/// this column is all that is left to say a conflict was resolved and by what.
pub fn set_resolve_task(db: &DbPool, group_id: i64, task_id: i64) -> Result<(), AppError> {
    let conn = db.lock();
    conn.execute(
        "UPDATE task_groups SET resolve_task_id=?2, updated_at=datetime('now','localtime')
         WHERE id=?1",
        params![group_id, task_id],
    )?;
    Ok(())
}

pub fn set_status(db: &DbPool, group_id: i64, status: &str) -> Result<(), AppError> {
    if !matches!(
        status,
        STATUS_ACTIVE | STATUS_COMPLETED | STATUS_FAILED | STATUS_STOPPED
    ) {
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
    fn a_new_member_joins_at_the_end_of_the_run() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let r = seed_task(&db, "resolve");
        let id = create(&db, 1, "trunk/s", "main", a, &[a]).unwrap();

        add_member(&db, id, r).unwrap();

        // Order matters: members() is rowid-ordered and remerge_stopped_members
        // walks it front to back, so the resolve task must come after the work it
        // resolves rather than before it.
        assert_eq!(members(&db, id), vec![a, r]);
    }

    #[test]
    fn a_task_already_claimed_by_a_run_cannot_join_another() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let b = seed_task(&db, "b");
        create(&db, 1, "trunk/one", "main", a, &[a]).unwrap();
        let two = create(&db, 1, "trunk/two", "main", b, &[b]).unwrap();

        // Silently accepting this would give `a` two trunks and no correct merge
        // target — the same reason create() uses a plain INSERT.
        assert!(add_member(&db, two, a).is_err());
        assert_eq!(members(&db, two), vec![b]);
    }

    #[test]
    fn the_resolve_task_is_recorded_and_survives_the_runs_end() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let r = seed_task(&db, "resolve");
        let id = create(&db, 1, "trunk/s", "main", a, &[a]).unwrap();
        assert_eq!(get(&db, id).unwrap().resolve_task_id, None);

        set_resolve_task(&db, id, r).unwrap();
        assert_eq!(get(&db, id).unwrap().resolve_task_id, Some(r));

        // finish() deletes membership but keeps the group row — and this column is
        // the row's record of how the conflict was handled. Losing it on finish
        // would erase the audit trail the feature promises.
        finish(&db, id, STATUS_COMPLETED).unwrap();
        assert_eq!(get(&db, id).unwrap().resolve_task_id, Some(r));
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
    fn trunks_by_task_covers_every_member_of_a_live_group_only() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let b = seed_task(&db, "b");
        let done_member = seed_task(&db, "old");
        let live = create(&db, 1, "trunk/live", "main", b, &[a, b]).unwrap();
        let old = create(&db, 1, "trunk/old", "main", done_member, &[done_member]).unwrap();
        finish(&db, old, STATUS_COMPLETED).unwrap();

        let map = trunks_by_task(&db, 1);

        assert_eq!(
            map.get(&a),
            Some(&("trunk/live".to_string(), STATUS_ACTIVE.to_string(), None))
        );
        assert_eq!(
            map.get(&b),
            Some(&("trunk/live".to_string(), STATUS_ACTIVE.to_string(), None))
        );
        // A landed group's trunk is deleted, so showing it on a card would name a
        // branch that is gone.
        assert!(!map.contains_key(&done_member));
        assert_eq!(get(&db, live).unwrap().status, STATUS_ACTIVE);
    }

    #[test]
    fn stopped_for_task_finds_only_a_stopped_run() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let id = create(&db, 1, "trunk/s", "main", a, &[a]).unwrap();

        // An active run is not resolvable: there is nothing waiting on a person.
        assert!(stopped_for_task(&db, a).is_none());

        set_status(&db, id, STATUS_STOPPED).unwrap();
        assert_eq!(stopped_for_task(&db, a).map(|g| g.id), Some(id));

        // A closed run has released its members, so there is nothing to resume.
        finish(&db, id, STATUS_FAILED).unwrap();
        assert!(stopped_for_task(&db, a).is_none());
    }

    #[test]
    fn run_for_task_agrees_with_the_board_query() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let b = seed_task(&db, "b");
        let id = create(&db, 1, "trunk/agree", "main", b, &[a, b]).unwrap();

        // The single-task lookup feeds every task:updated payload and the batch one
        // feeds the board. If they disagree, a card's marker flickers between them.
        for status in [STATUS_ACTIVE, STATUS_STOPPED] {
            set_status(&db, id, status).unwrap();
            let batch = trunks_by_task(&db, 1);
            for task in [a, b] {
                assert_eq!(
                    run_for_task(&db, task),
                    batch.get(&task).cloned(),
                    "disagreement on task {task} at status {status}"
                );
            }
        }

        finish(&db, id, STATUS_COMPLETED).unwrap();
        assert!(run_for_task(&db, a).is_none());
        assert!(trunks_by_task(&db, 1).is_empty());
    }

    #[test]
    fn a_stopped_run_still_reaches_the_board() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let b = seed_task(&db, "b");
        let id = create(&db, 1, "trunk/stopped", "main", b, &[a, b]).unwrap();
        set_status(&db, id, STATUS_STOPPED).unwrap();

        let map = trunks_by_task(&db, 1);

        // The moment a run needs attention is the moment its cards have to say so.
        // Filtering to active made the marker disappear exactly then.
        assert_eq!(
            map.get(&a),
            Some(&(
                "trunk/stopped".to_string(),
                STATUS_STOPPED.to_string(),
                None
            ))
        );
        // And the trunk still resolves, so a member finishing late lands on it
        // rather than on the project's base branch.
        assert_eq!(trunk_for_task(&db, a).as_deref(), Some("trunk/stopped"));
        // A stopped run is not live: nothing may start its remaining members or
        // try to land its trunk.
        assert!(for_task(&db, a).is_none());
    }

    #[test]
    fn abandoning_a_stopped_run_releases_its_tasks() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let b = seed_task(&db, "b");
        let id = create(&db, 1, "trunk/give-up", "main", b, &[a, b]).unwrap();
        set_status(&db, id, STATUS_STOPPED).unwrap();

        let closed = abandon_stopped(&db, &[a, b]);

        // Without this a stopped run nobody resolves claims its tasks for good.
        assert_eq!(closed, vec![id]);
        assert_eq!(get(&db, id).unwrap().status, STATUS_FAILED);
        assert!(members(&db, id).is_empty());
        assert!(trunk_for_task(&db, a).is_none());
        // Claimable again: a new run over the same tasks must be allowed.
        assert!(create(&db, 1, "trunk/again", "main", b, &[a, b]).is_ok());
    }

    #[test]
    fn abandoning_leaves_a_live_run_alone() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let id = create(&db, 1, "trunk/live", "main", a, &[a]).unwrap();

        // Only a stopped run is up for abandonment. Closing a running one would
        // orphan the agents working inside it.
        assert!(abandon_stopped(&db, &[a]).is_empty());
        assert_eq!(get(&db, id).unwrap().status, STATUS_ACTIVE);
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
