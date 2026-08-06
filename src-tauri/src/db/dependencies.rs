use super::tasks::Task;
use super::DbPool;
use crate::error::AppError;
use rusqlite::params;
use std::collections::{HashMap, HashSet};

/// Add a dependency edge: task_id depends on depends_on_id.
/// condition_type: "always" (default), "on_success", "on_failure"
/// Returns error if it would create a cycle.
pub fn add_dependency(
    db: &DbPool,
    task_id: i64,
    depends_on_id: i64,
    condition_type: Option<&str>,
) -> Result<(), AppError> {
    if task_id == depends_on_id {
        return Err(AppError::Validation("Task cannot depend on itself".into()));
    }
    if detect_cycle(db, task_id, depends_on_id) {
        return Err(AppError::Validation(
            "Adding this dependency would create a cycle".into(),
        ));
    }
    let ctype = condition_type.unwrap_or("always");
    let conn = db.lock();
    conn.execute(
        "INSERT OR IGNORE INTO task_dependencies (task_id, depends_on_id, condition_type) VALUES (?1, ?2, ?3)",
        params![task_id, depends_on_id, ctype],
    )?;
    Ok(())
}

/// Remove a dependency edge.
pub fn remove_dependency(db: &DbPool, task_id: i64, depends_on_id: i64) -> Result<(), AppError> {
    let conn = db.lock();
    conn.execute(
        "DELETE FROM task_dependencies WHERE task_id=?1 AND depends_on_id=?2",
        params![task_id, depends_on_id],
    )?;
    Ok(())
}

/// Remove all dependencies for a task (both as child and parent).
pub fn remove_all_for_task(db: &DbPool, task_id: i64) -> Result<(), AppError> {
    let conn = db.lock();
    conn.execute(
        "DELETE FROM task_dependencies WHERE task_id=?1 OR depends_on_id=?1",
        params![task_id],
    )?;
    Ok(())
}

/// Get parent task IDs (tasks that this task depends on).
pub fn get_parent_ids(db: &DbPool, task_id: i64) -> Vec<i64> {
    let conn = db.lock();
    let mut stmt =
        match conn.prepare("SELECT depends_on_id FROM task_dependencies WHERE task_id=?1") {
            Ok(s) => s,
            Err(_) => return vec![],
        };
    let result = match stmt.query_map(params![task_id], |r| r.get(0)) {
        Ok(rows) => rows.flatten().collect(),
        Err(_) => vec![],
    };
    result
}

/// Get child task IDs (tasks that depend on this task).
pub fn get_child_ids(db: &DbPool, task_id: i64) -> Vec<i64> {
    let conn = db.lock();
    let mut stmt =
        match conn.prepare("SELECT task_id FROM task_dependencies WHERE depends_on_id=?1") {
            Ok(s) => s,
            Err(_) => return vec![],
        };
    let result = match stmt.query_map(params![task_id], |r| r.get(0)) {
        Ok(rows) => rows.flatten().collect(),
        Err(_) => vec![],
    };
    result
}

/// Whether one dependency edge is satisfied, as SQL, for a parent row aliased as
/// `parent_alias`.
///
/// The single definition of the question. Every query that asks it builds its SQL
/// from here, because separate copies drift: a proposed second version counted
/// `awaiting_approval` as satisfied while this one does not, which would have let a
/// task start on work nobody had approved.
///
/// Note that satisfaction is a property of the *edge*, not of the parent alone. An
/// `on_failure` edge wants a failed parent, so a completed one leaves it unmet.
///
/// - `always` / `on_success`: parent is done or testing
/// - `on_failure`: parent has failed
/// - `on_any`: either
fn edge_is_met_sql(parent_alias: &str) -> String {
    format!(
        "CASE COALESCE(td.condition_type, 'always')
             WHEN 'on_failure' THEN
                 {a}.status = 'failed'
             WHEN 'on_any' THEN
                 {a}.status IN ('done', 'testing', 'failed')
             ELSE
                 {a}.status IN ('done', 'testing')
         END",
        a = parent_alias
    )
}

/// Parents this task is still waiting on, respecting each edge's condition.
pub fn unmet_parent_ids(db: &DbPool, task_id: i64) -> Vec<i64> {
    let sql = format!(
        "SELECT td.depends_on_id FROM task_dependencies td
         JOIN tasks parent ON parent.id = td.depends_on_id
         WHERE td.task_id = ?1 AND NOT ({})",
        edge_is_met_sql("parent")
    );
    let conn = db.lock();
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(e) => {
            log::error!("unmet_parent_ids: {}", e);
            return vec![];
        }
    };
    let mut out: Vec<i64> = Vec::new();
    match stmt.query_map(params![task_id], |r| r.get::<_, i64>(0)) {
        Ok(rows) => out.extend(rows.flatten()),
        Err(e) => log::error!("unmet_parent_ids: {}", e),
    }
    out
}

/// Check if ALL parent dependencies of a task are met, respecting condition_type.
pub fn are_all_parents_met(db: &DbPool, task_id: i64) -> bool {
    unmet_parent_ids(db, task_id).is_empty()
}

/// Transitive ancestors of `task_id` that still have to run, ordered leaves first.
///
/// Empty means the task can start now — the same answer [`are_all_parents_met`]
/// gives, because both read the one edge predicate. The target itself is never
/// included, so a caller can enqueue these and then the target.
///
/// Waves are laid out by longest path rather than shortest, so a task appears only
/// after every ancestor it transitively waits on. Two ancestors in the same wave
/// are independent and can run together.
pub fn unmet_ancestor_waves(db: &DbPool, task_id: i64) -> Vec<Vec<Task>> {
    // Closure of unmet ancestors, with each task's unmet parents memoised: the
    // depth pass below revisits them repeatedly and each lookup is a query.
    let mut parents_of: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut pending: Vec<i64> = unmet_parent_ids(db, task_id);
    let mut closure: HashSet<i64> = HashSet::new();
    while let Some(id) = pending.pop() {
        if !closure.insert(id) {
            continue;
        }
        let parents = unmet_parent_ids(db, id);
        pending.extend(parents.iter().copied());
        parents_of.insert(id, parents);
    }
    if closure.is_empty() {
        return vec![];
    }

    // Longest path from a leaf, counting only edges inside the closure: an edge to
    // an already-satisfied task delays nothing. Relaxed until stable, which takes
    // at most one pass per node because each pass settles at least one more level.
    let mut depth: HashMap<i64, usize> = HashMap::new();
    for _ in 0..closure.len() {
        let mut changed = false;
        for &id in &closure {
            let d = parents_of
                .get(&id)
                .map(|ps| {
                    ps.iter()
                        .filter(|p| closure.contains(p))
                        .map(|p| depth.get(p).copied().unwrap_or(0) + 1)
                        .max()
                        .unwrap_or(0)
                })
                .unwrap_or(0);
            if depth.get(&id).copied().unwrap_or(0) != d {
                depth.insert(id, d);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let deepest = depth.values().copied().max().unwrap_or(0);
    let mut waves: Vec<Vec<Task>> = vec![Vec::new(); deepest + 1];
    for &id in &closure {
        if let Some(task) = super::tasks::get_by_id(db, id) {
            waves[depth.get(&id).copied().unwrap_or(0)].push(task);
        }
    }
    // A task that vanished between the walk and the read leaves a hole, not a gap
    // in the ordering.
    waves.retain(|w| !w.is_empty());
    waves
}

/// Get all backlog tasks in a project that have all dependencies met (ready to run).
/// Supports conditional dependencies: always/on_success require parent done/testing,
/// on_failure requires parent to have exhausted retries.
pub fn get_ready_tasks(db: &DbPool, project_id: i64) -> Vec<Task> {
    let conn = db.lock();
    let sql = format!(
        "SELECT t.* FROM tasks t
         LEFT JOIN projects p ON p.id = t.project_id
         WHERE t.project_id = ?1 AND t.status = 'backlog'
         AND COALESCE(t.retry_count, 0) <= CASE WHEN COALESCE(p.max_retries, 0) > 0 THEN p.max_retries ELSE 2 END
         AND (t.retry_after IS NULL OR t.retry_after <= datetime('now','localtime'))
         AND NOT EXISTS (
             SELECT 1 FROM task_dependencies td
             JOIN tasks parent ON parent.id = td.depends_on_id
             WHERE td.task_id = t.id
             AND NOT ({edge})
         )
         ORDER BY
             (SELECT COUNT(*) FROM task_dependencies cd WHERE cd.depends_on_id = t.id) DESC,
             t.priority DESC, t.queue_position ASC, t.id ASC",
        edge = edge_is_met_sql("parent")
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let result = match stmt.query_map(params![project_id], super::tasks::row_to_task) {
        Ok(rows) => rows.flatten().collect(),
        Err(_) => vec![],
    };
    result
}

/// Detect if adding depends_on_id as a parent of task_id would create a cycle.
/// Uses DFS: walks ancestors of depends_on_id to check if task_id is reachable.
fn detect_cycle(db: &DbPool, task_id: i64, depends_on_id: i64) -> bool {
    let conn = db.lock();
    let mut visited = HashSet::new();
    let mut stack = vec![depends_on_id];

    while let Some(current) = stack.pop() {
        if current == task_id {
            return true;
        }
        if !visited.insert(current) {
            continue;
        }
        let mut stmt =
            match conn.prepare("SELECT depends_on_id FROM task_dependencies WHERE task_id=?1") {
                Ok(s) => s,
                Err(_) => continue,
            };
        let parents: Vec<i64> = match stmt.query_map(params![current], |r| r.get(0)) {
            Ok(rows) => rows.flatten().collect(),
            Err(_) => vec![],
        };
        drop(stmt);
        stack.extend(parents);
    }
    false
}

/// Get execution waves for a project: groups of tasks that can run in parallel.
/// Wave 0 = no dependencies, Wave 1 = depends only on wave 0, etc.
pub fn get_execution_waves(db: &DbPool, project_id: i64) -> Vec<Vec<Task>> {
    let all_tasks = super::tasks::get_by_project(db, project_id);
    if all_tasks.is_empty() {
        return vec![];
    }

    let mut assigned: HashSet<i64> = HashSet::new();
    let mut waves: Vec<Vec<Task>> = Vec::new();

    // Also treat done/testing tasks as already resolved
    for t in &all_tasks {
        if matches!(t.status.as_deref(), Some("done") | Some("testing")) {
            assigned.insert(t.id);
        }
    }

    let pending: Vec<&Task> = all_tasks
        .iter()
        .filter(|t| matches!(t.status.as_deref(), Some("backlog") | Some("in_progress")))
        .collect();

    loop {
        let wave: Vec<Task> = pending
            .iter()
            .filter(|t| !assigned.contains(&t.id))
            .filter(|t| {
                let parents = get_parent_ids(db, t.id);
                parents.is_empty() || parents.iter().all(|p| assigned.contains(p))
            })
            .map(|t| (*t).clone())
            .collect();

        if wave.is_empty() {
            break;
        }

        for t in &wave {
            assigned.insert(t.id);
        }
        waves.push(wave);
    }

    waves
}

/// Get dependency graph summary for a project (used by frontend orchestration view).
pub fn get_graph_data(db: &DbPool, project_id: i64) -> serde_json::Value {
    let all_tasks = super::tasks::get_by_project(db, project_id);

    let edges: Vec<serde_json::Value> = {
        let conn = db.lock();
        let mut stmt = match conn.prepare(
            "SELECT td.task_id, td.depends_on_id, COALESCE(td.condition_type, 'always') FROM task_dependencies td
             JOIN tasks t ON t.id = td.task_id WHERE t.project_id = ?1"
        ) {
            Ok(s) => s,
            Err(_) => return serde_json::json!({ "tasks": all_tasks, "edges": [], "waves": [] }),
        };
        let rows: Vec<(i64, i64, String)> = match stmt.query_map(params![project_id], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get::<_, String>(2).unwrap_or_else(|_| "always".into()),
            ))
        }) {
            Ok(r) => r.flatten().collect(),
            Err(_) => vec![],
        };
        drop(stmt);
        rows.into_iter().map(|(child, parent, ctype)| {
            serde_json::json!({ "from": parent, "to": child, "conditionType": ctype })
        }).collect()
    };

    let waves = get_execution_waves(db, project_id);

    serde_json::json!({
        "tasks": all_tasks,
        "edges": edges,
        "waves": waves.iter().enumerate().map(|(i, w)| {
            serde_json::json!({
                "index": i,
                "taskIds": w.iter().map(|t| t.id).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
    })
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

    fn set_status(db: &DbPool, id: i64, status: &str) {
        db.lock()
            .execute(
                "UPDATE tasks SET status=?2 WHERE id=?1",
                params![id, status],
            )
            .unwrap();
    }

    fn wave_ids(waves: &[Vec<Task>]) -> Vec<Vec<i64>> {
        waves
            .iter()
            .map(|w| {
                let mut ids: Vec<i64> = w.iter().map(|t| t.id).collect();
                ids.sort();
                ids
            })
            .collect()
    }

    #[test]
    fn unmet_ancestor_waves_orders_leaves_first() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let b = seed_task(&db, "b");
        let c = seed_task(&db, "c");
        add_dependency(&db, b, a, None).unwrap();
        add_dependency(&db, c, b, None).unwrap();

        let waves = unmet_ancestor_waves(&db, c);

        // b cannot run before a, so it cannot share a wave with it.
        assert_eq!(wave_ids(&waves), vec![vec![a], vec![b]]);
    }

    #[test]
    fn unmet_ancestor_waves_skips_satisfied_parents() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let b = seed_task(&db, "b");
        add_dependency(&db, b, a, None).unwrap();
        set_status(&db, a, "done");

        assert!(unmet_ancestor_waves(&db, b).is_empty());
    }

    #[test]
    fn unmet_ancestor_waves_groups_independent_parents_into_one_wave() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let b = seed_task(&db, "b");
        let c = seed_task(&db, "c");
        add_dependency(&db, c, a, None).unwrap();
        add_dependency(&db, c, b, None).unwrap();

        let waves = unmet_ancestor_waves(&db, c);

        assert_eq!(wave_ids(&waves), vec![vec![a, b]]);
    }

    #[test]
    fn a_diamond_puts_the_join_after_both_of_its_branches() {
        let db = test_db();
        //   a → {b, c} → d → target
        let a = seed_task(&db, "a");
        let b = seed_task(&db, "b");
        let c = seed_task(&db, "c");
        let d = seed_task(&db, "d");
        let target = seed_task(&db, "target");
        add_dependency(&db, b, a, None).unwrap();
        add_dependency(&db, c, a, None).unwrap();
        add_dependency(&db, d, b, None).unwrap();
        add_dependency(&db, d, c, None).unwrap();
        add_dependency(&db, target, d, None).unwrap();

        let waves = unmet_ancestor_waves(&db, target);

        // Depth is the longest path, not the shortest: d waits for both branches.
        assert_eq!(wave_ids(&waves), vec![vec![a], vec![b, c], vec![d]]);
    }

    #[test]
    fn a_shortcut_edge_does_not_pull_a_task_into_an_earlier_wave() {
        let db = test_db();
        // d depends on a *and* on b, and b depends on a. The a→d edge is a shortcut
        // past b.
        let a = seed_task(&db, "a");
        let b = seed_task(&db, "b");
        let d = seed_task(&db, "d");
        let target = seed_task(&db, "target");
        add_dependency(&db, b, a, None).unwrap();
        add_dependency(&db, d, a, None).unwrap();
        add_dependency(&db, d, b, None).unwrap();
        add_dependency(&db, target, d, None).unwrap();

        let waves = unmet_ancestor_waves(&db, target);

        // Depth has to be the longest path. Taking the shortest would put d in the
        // same wave as b, and d cannot start until b has finished.
        assert_eq!(wave_ids(&waves), vec![vec![a], vec![b], vec![d]]);
    }

    #[test]
    fn a_task_with_no_dependencies_has_no_waves() {
        let db = test_db();
        let a = seed_task(&db, "a");

        assert!(unmet_ancestor_waves(&db, a).is_empty());
    }

    #[test]
    fn an_awaiting_approval_parent_is_not_satisfied() {
        let db = test_db();
        let parent = seed_task(&db, "parent");
        let child = seed_task(&db, "child");
        add_dependency(&db, child, parent, None).unwrap();
        set_status(&db, parent, "awaiting_approval");

        // Treating this as satisfied would start the child on work nobody approved.
        assert_eq!(
            wave_ids(&unmet_ancestor_waves(&db, child)),
            vec![vec![parent]]
        );
        assert!(!are_all_parents_met(&db, child));
    }

    #[test]
    fn a_blocked_parent_is_not_satisfied() {
        let db = test_db();
        let parent = seed_task(&db, "parent");
        let child = seed_task(&db, "child");
        add_dependency(&db, child, parent, None).unwrap();
        set_status(&db, parent, "blocked");

        // A parent waiting on an answer has not produced anything to build on.
        assert_eq!(
            wave_ids(&unmet_ancestor_waves(&db, child)),
            vec![vec![parent]]
        );
    }

    #[test]
    fn a_parent_in_testing_counts_as_satisfied() {
        let db = test_db();
        let parent = seed_task(&db, "parent");
        let child = seed_task(&db, "child");
        add_dependency(&db, child, parent, None).unwrap();
        set_status(&db, parent, "testing");

        // Existing behaviour, pinned: testing means the work exists on its branch.
        assert!(unmet_ancestor_waves(&db, child).is_empty());
        assert!(are_all_parents_met(&db, child));
    }

    #[test]
    fn satisfaction_follows_the_edge_condition_not_just_the_parent_status() {
        let db = test_db();
        let parent = seed_task(&db, "parent");
        let child = seed_task(&db, "child");
        add_dependency(&db, child, parent, Some("on_failure")).unwrap();
        set_status(&db, parent, "done");

        // An on_failure edge wants a failed parent. A done one does not satisfy it,
        // so asking only "is this task finished?" gets the answer wrong.
        assert_eq!(
            wave_ids(&unmet_ancestor_waves(&db, child)),
            vec![vec![parent]]
        );
        assert!(!are_all_parents_met(&db, child));

        set_status(&db, parent, "failed");
        assert!(unmet_ancestor_waves(&db, child).is_empty());
        assert!(are_all_parents_met(&db, child));
    }

    #[test]
    fn the_waves_and_are_all_parents_met_never_disagree() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let b = seed_task(&db, "b");
        add_dependency(&db, b, a, None).unwrap();

        // The invariant the gate depends on: empty waves must mean startable.
        for status in [
            "backlog",
            "in_progress",
            "blocked",
            "awaiting_approval",
            "failed",
        ] {
            set_status(&db, a, status);
            assert_eq!(
                unmet_ancestor_waves(&db, b).is_empty(),
                are_all_parents_met(&db, b),
                "disagreed while the parent was {status}"
            );
        }
        for status in ["done", "testing"] {
            set_status(&db, a, status);
            assert!(unmet_ancestor_waves(&db, b).is_empty());
            assert!(are_all_parents_met(&db, b));
        }
    }

    #[test]
    fn an_unmet_middle_ancestor_still_pulls_in_its_own_unmet_parents() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let b = seed_task(&db, "b");
        let c = seed_task(&db, "c");
        add_dependency(&db, b, a, None).unwrap();
        add_dependency(&db, c, b, None).unwrap();
        set_status(&db, b, "failed");

        // b failed, so it is unmet and has to run again — and a has to precede it.
        assert_eq!(
            wave_ids(&unmet_ancestor_waves(&db, c)),
            vec![vec![a], vec![b]]
        );
    }
}
