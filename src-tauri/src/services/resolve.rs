//! The task whose job is resolving the merge conflict that stopped a run.
//!
//! An ordinary board task made a *member of the stopped run*, which is the whole
//! trick: a member's worktree branches from the trunk, so the agent starts with the
//! run's merged work already in front of it and can simply merge the refused branch
//! into it. Being a task is the rest of the value — it is visible, it can raise a
//! blocker when the right resolution is a judgement call, it obeys the project's
//! approval gate, and the tasks that have not run yet can depend on it.
//!
//! Its own module rather than a helper inside the command, because creation and
//! starting are separate concerns: this half needs no `AppHandle` and is what a
//! future automatic resolution would call from the stop path.

use crate::claude::state_machine::EngineConfig;
use crate::db::task_groups::TaskGroup;
use crate::db::{activity, dependencies, projects::Project, task_groups, tasks, DbPool};

/// The card's title. Names the first branch, and says how many others there are.
pub fn resolve_task_title(trunk: &str, branches: &[String]) -> String {
    let first = branches.first().map(String::as_str).unwrap_or("(unknown)");
    let rest = branches.len().saturating_sub(1);
    if rest == 0 {
        format!("Resolve merge conflict: {} → {}", first, trunk)
    } else {
        format!(
            "Resolve merge conflict: {} + {} more → {}",
            first, rest, trunk
        )
    }
}

/// The agent's entire instruction set.
///
/// Written here rather than through prompt machinery because the runner's prompt
/// already carries the branch discipline and the raise-a-blocker imperative. What is
/// left is what only this task knows: which branches to merge, where, and the two
/// failure modes peculiar to conflict resolution — rewriting instead of merging, and
/// discarding one side to make the conflict go away. The blocker rule is restated all
/// the same, because the generic imperative fires when an agent feels stuck and a
/// conflict agent is precisely the one that never feels stuck while it guesses.
pub fn resolve_task_description(trunk: &str, branches: &[String]) -> String {
    let named = branches
        .iter()
        .map(|b| format!("`{}`", b))
        .collect::<Vec<_>>()
        .join(", ");
    let merges = branches
        .iter()
        .map(|b| format!("    git merge {}", b))
        .collect::<Vec<_>>()
        .join("\n");
    let (subject, verb) = if branches.len() == 1 {
        ("branch", "could not be merged")
    } else {
        ("branches", "could not be merged")
    };

    format!(
        "A dependency run on this board has stopped: the {subject} {named} {verb}\n\
         into the run's shared branch `{trunk}` because the merge conflicted.\n\
         Your working directory is a checkout of a branch cut from `{trunk}`, so the\n\
         run's merged work is already here. Your job is to bring the refused work in\n\
         too, resolving the conflict faithfully.\n\
         \n\
         Run:\n\
         \n\
         {merges}\n\
         \n\
         Then resolve every conflict by combining the intent of both sides. Before\n\
         deciding anything, read the conflicting hunks and the commits that produced\n\
         them (`git log --merge`, `git show`) so you understand what each side was\n\
         trying to do.\n\
         \n\
         Rules:\n\
         \n\
         - Merge — do not rewrite. No cherry-pick, no rebase, no reset, no copying\n\
         \x20 files across branches, no `git merge --abort` followed by hand-edits. The\n\
         \x20 resolution is verified by ancestry: each branch above must be a parent in\n\
         \x20 your history, and anything else will be rejected.\n\
         - Never discard one side wholesale. Do not use `git checkout --ours`,\n\
         \x20 `git checkout --theirs`, `-X ours` or `-X theirs`. If both sides changed\n\
         \x20 the same thing on purpose and only one can win, that is the user's call,\n\
         \x20 not yours: raise a blocker describing both sides and what choosing each\n\
         \x20 one would mean, and wait for the answer.\n\
         - Touch only what the conflict forces you to touch. No drive-by fixes, no\n\
         \x20 reformatting, no changes outside the conflicting hunks.\n\
         - Commit the merge on your current branch. Do not switch branches, do not\n\
         \x20 delete branches, and do not push.\n\
         \n\
         When the project has a build or test command, run it after resolving: a\n\
         resolution that does not build is not resolved."
    )
}

fn acceptance_criteria(branches: &[String]) -> String {
    let checks = branches
        .iter()
        .map(|b| format!("git merge-base --is-ancestor {} HEAD succeeds", b))
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "{}; the conflicted files carry both sides' intent; nothing unrelated changed.",
        checks
    )
}

/// Create the task, put it in the run, and make the waiting members depend on it.
///
/// Does not start it — starting needs an `AppHandle`, and the caller that has one
/// also has to decide whether the run's stopped state may be overridden for it.
///
/// Edges go to members that have not successfully run, meaning `backlog` and
/// `failed`. A member that is finished or in flight gets none: an edge on running or
/// finished work is a lie about ordering, and nothing disturbs an in-flight member
/// anyway. No cycle is possible — the new task has no parents of its own.
pub fn create_resolve_task(
    db: &DbPool,
    project: &Project,
    group: &TaskGroup,
    branches: &[String],
) -> Result<i64, String> {
    if branches.is_empty() {
        return Err("there is nothing left to resolve on this run".to_string());
    }

    let config = EngineConfig::from_project(project);
    let title = resolve_task_title(&group.trunk_branch, branches);
    // The run's target is what the chain exists to deliver, so the resolution that
    // unblocks it belongs in the same place in the queue.
    let priority = tasks::get_by_id(db, group.target_task_id)
        .and_then(|t| t.priority)
        .unwrap_or(0);

    let id = tasks::create(
        db,
        group.project_id,
        &title,
        &resolve_task_description(&group.trunk_branch, branches),
        priority,
        "chore",
        &acceptance_criteria(branches),
        &config.resolve_model,
        &config.resolve_effort,
        None,
        Some("[\"resolve\"]"),
    );
    if id < 0 {
        return Err("could not create the resolve task".to_string());
    }

    // Membership is what puts its worktree on the trunk. Without it the agent would
    // branch from the project's base and never see the run's work at all.
    task_groups::add_member(db, group.id, id).map_err(|e| e.to_string())?;
    task_groups::set_resolve_task(db, group.id, id).map_err(|e| e.to_string())?;

    for member in task_groups::members(db, group.id) {
        if member == id {
            continue;
        }
        let status = tasks::get_by_id(db, member)
            .and_then(|t| t.status)
            .unwrap_or_default();
        if status != "backlog" && status != "failed" {
            continue;
        }
        if let Err(e) = dependencies::add_dependency(db, member, id, None) {
            log::error!(
                "could not make task {} wait for the resolution: {}",
                member,
                e
            );
        }
    }

    activity::add(
        db,
        group.project_id,
        Some(id),
        "resolve_created",
        &format!("Resolve task created for {}: {}", group.trunk_branch, title),
        None,
    );

    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use rusqlite::params;
    use std::sync::Arc;

    fn test_db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::create_tables(&conn);
        crate::db::schema::run_migrations(&conn);
        conn.execute(
            "INSERT INTO projects (id,name,slug,working_dir) VALUES (1,'B','b','/repo')",
            [],
        )
        .unwrap();
        Arc::new(Mutex::new(conn))
    }

    fn seed_task(db: &DbPool, title: &str, status: &str) -> i64 {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO tasks (project_id,title,status,priority) VALUES (1,?1,?2,2)",
            params![title, status],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn project_with(model: Option<&str>, effort: Option<&str>) -> Project {
        serde_json::from_value(serde_json::json!({
            "id": 1, "name": "B", "slug": "b", "working_dir": "/repo",
            "resolve_model": model, "resolve_effort": effort,
        }))
        .unwrap()
    }

    /// A stopped run over four members covering the spread of statuses a real one
    /// has when it stops, and the project the run belongs to.
    fn stopped_run(db: &DbPool) -> (TaskGroup, [i64; 4]) {
        let a = seed_task(db, "a done", "done");
        let b = seed_task(db, "b waiting", "backlog");
        let c = seed_task(db, "c crashed", "failed");
        let d = seed_task(db, "d running", "in_progress");
        let gid = task_groups::create(db, 1, "trunk/feature/d", "main", d, &[a, b, c, d]).unwrap();
        task_groups::set_status(db, gid, task_groups::STATUS_STOPPED).unwrap();
        (task_groups::get(db, gid).unwrap(), [a, b, c, d])
    }

    #[test]
    fn the_description_tells_the_agent_everything_it_cannot_guess() {
        let d = resolve_task_description("trunk/feature/d", &["feature/x".into()]);

        // The agent's whole instruction set is this text; each assertion below is a
        // failure mode seen or feared: not knowing what to merge, rewriting instead
        // of merging, discarding a side, or guessing an ambiguous resolution.
        assert!(d.contains("git merge feature/x"), "{d}");
        assert!(d.contains("trunk/feature/d"), "{d}");
        assert!(d.contains("do not rewrite"), "{d}");
        assert!(d.contains("checkout --ours"), "{d}");
        assert!(d.contains("raise a blocker"), "{d}");
        assert!(d.contains("Do not switch branches"), "{d}");
    }

    #[test]
    fn every_unresolved_branch_gets_its_own_merge_instruction() {
        let d =
            resolve_task_description("trunk/feature/d", &["feature/x".into(), "feature/y".into()]);

        // A run can stop owing more than one branch. An instruction naming only the
        // first would leave the second unmerged and the resolution would be rejected
        // for work the agent was never told about.
        assert!(d.contains("git merge feature/x"), "{d}");
        assert!(d.contains("git merge feature/y"), "{d}");
        assert!(
            d.find("git merge feature/x") < d.find("git merge feature/y"),
            "merges follow members order"
        );
    }

    #[test]
    fn the_title_names_the_first_branch_and_counts_the_rest() {
        assert_eq!(
            resolve_task_title("trunk/t", &["feature/x".into()]),
            "Resolve merge conflict: feature/x → trunk/t"
        );
        assert_eq!(
            resolve_task_title("trunk/t", &["feature/x".into(), "feature/y".into()]),
            "Resolve merge conflict: feature/x + 1 more → trunk/t"
        );
    }

    #[test]
    fn the_resolve_task_runs_the_projects_chosen_model_and_joins_the_run() {
        let db = test_db();
        let (group, [a, b, c, d]) = stopped_run(&db);

        let r = create_resolve_task(
            &db,
            &project_with(None, None),
            &group,
            &["feature/x".into()],
        )
        .unwrap();

        // Opus/high is the settled default; both land on the task row because that
        // is all the runner reads when it spawns the agent.
        let task = tasks::get_by_id(&db, r).unwrap();
        assert_eq!(task.model.as_deref(), Some("opus"));
        assert_eq!(task.thinking_effort.as_deref(), Some("high"));
        // Member: this is what puts its worktree on the trunk.
        assert_eq!(task_groups::members(&db, group.id).last(), Some(&r));
        assert_eq!(
            task_groups::get(&db, group.id).unwrap().resolve_task_id,
            Some(r)
        );
        // Edges to the members that have not successfully run — and only those.
        assert_eq!(dependencies::get_parent_ids(&db, b), vec![r]);
        assert_eq!(dependencies::get_parent_ids(&db, c), vec![r]);
        assert!(dependencies::get_parent_ids(&db, a).is_empty());
        assert!(dependencies::get_parent_ids(&db, d).is_empty());
        // And the resolve task depends on nothing, so it can start immediately.
        assert!(dependencies::get_parent_ids(&db, r).is_empty());
    }

    #[test]
    fn a_project_that_dialled_down_gets_what_it_asked_for() {
        let db = test_db();
        let (group, _) = stopped_run(&db);

        let r = create_resolve_task(
            &db,
            &project_with(Some("sonnet"), Some("low")),
            &group,
            &["feature/x".into()],
        )
        .unwrap();

        // Fails if creation hardcodes the default instead of resolving the project's
        // settings, which would make the two controls decorative.
        let task = tasks::get_by_id(&db, r).unwrap();
        assert_eq!(task.model.as_deref(), Some("sonnet"));
        assert_eq!(task.thinking_effort.as_deref(), Some("low"));
    }

    #[test]
    fn a_run_with_nothing_to_merge_gets_no_task() {
        let db = test_db();
        let (group, _) = stopped_run(&db);

        // An agent spawned to merge nothing would burn a run's one resolve attempt
        // on a no-op and then report success.
        assert!(create_resolve_task(&db, &project_with(None, None), &group, &[]).is_err());
        assert_eq!(
            task_groups::get(&db, group.id).unwrap().resolve_task_id,
            None
        );
    }
}
