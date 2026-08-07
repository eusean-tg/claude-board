use crate::claude::runner;
use crate::claude::state_machine::{is_valid_transition, TaskStatus};
use crate::db::{self, activity, attachments, projects as pq, tasks as tq};
use crate::services::queue;
use tauri::{AppHandle, Emitter};

#[tauri::command]
pub fn get_tasks(project_id: i64) -> Vec<tq::Task> {
    hydrate_board(&db::get_db(), project_id)
}

/// Every task on a board, with the fields the board computes rather than stores.
///
/// Split from the command so the hydration can be tested against a pool of its own;
/// it is the list-sized equivalent of `tq::hydrate`, and the two have to agree.
pub(crate) fn hydrate_board(db: &db::DbPool, project_id: i64) -> Vec<tq::Task> {
    // Both of these are one query for the whole board rather than one per card.
    let waiting = db::dependencies::unmet_parent_counts(db, project_id);
    let trunks = db::task_groups::trunks_by_task(db, project_id);
    tq::get_by_project(db, project_id)
        .into_iter()
        .map(|mut t| {
            t.is_running = runner::is_running(t.id) || runner::is_starting(t.id);
            t.waiting_on = waiting.get(&t.id).copied().unwrap_or(0);
            if let Some((trunk, run_status, resolve_task_id)) = trunks.get(&t.id) {
                t.trunk_branch = Some(trunk.clone());
                t.run_stopped = run_status == crate::db::task_groups::STATUS_STOPPED;
                t.resolve_task_id = *resolve_task_id;
            }
            t
        })
        .collect()
}

#[tauri::command]
pub fn get_task(id: i64) -> Result<tq::Task, String> {
    let db = db::get_db();
    tq::get_for_ui(&db, id)
        .map(|mut t| {
            t.is_running = runner::is_running(t.id) || runner::is_starting(t.id);
            t
        })
        .ok_or_else(|| "Task not found".into())
}

#[tauri::command]
#[allow(non_snake_case)]
#[allow(clippy::too_many_arguments)]
pub fn create_task(
    app: AppHandle,
    project_id: i64,
    title: String,
    description: Option<String>,
    priority: Option<i64>,
    task_type: Option<String>,
    acceptance_criteria: Option<String>,
    model: Option<String>,
    thinking_effort: Option<String>,
    role_id: Option<i64>,
    parentTaskId: Option<i64>,
    tags: Option<String>,
) -> Result<tq::Task, String> {
    let db = db::get_db();
    if pq::get_by_id(&db, project_id).is_none() {
        return Err("Project not found".into());
    }
    if title.trim().is_empty() {
        return Err("Title is required".into());
    }

    let id = tq::create(
        &db,
        project_id,
        title.trim(),
        description.as_deref().unwrap_or("").trim(),
        priority.unwrap_or(0),
        task_type.as_deref().unwrap_or("feature"),
        acceptance_criteria.as_deref().unwrap_or("").trim(),
        model.as_deref().unwrap_or("sonnet"),
        thinking_effort.as_deref().unwrap_or("medium"),
        role_id,
        tags.as_deref(),
    );

    // Link as sub-task if parent_task_id provided
    if let Some(parent_id) = parentTaskId {
        if tq::get_by_id(&db, parent_id).is_some() {
            tq::set_parent_task_id(&db, id, parent_id);
            tq::set_awaiting_subtasks(&db, parent_id, true);
            activity::add(
                &db,
                project_id,
                Some(id),
                "subtask_created",
                &format!("Sub-task created under #{}: {}", parent_id, title.trim()),
                None,
            );
        }
    }

    let task = tq::get_by_id(&db, id).ok_or("Failed to retrieve created task")?;
    app.emit("task:created", &task).ok();
    activity::add(
        &db,
        project_id,
        Some(task.id),
        "task_created",
        &format!("Task created: {}", title.trim()),
        None,
    );
    queue::start_next_queued(&db, &app, project_id);
    Ok(task)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn update_task(
    app: AppHandle,
    id: i64,
    title: Option<String>,
    description: Option<String>,
    priority: Option<i64>,
    task_type: Option<String>,
    acceptance_criteria: Option<String>,
    model: Option<String>,
    thinking_effort: Option<String>,
    role_id: Option<i64>,
    tags: Option<String>,
) -> Result<tq::Task, String> {
    let db = db::get_db();
    let task = tq::get_by_id(&db, id).ok_or("Task not found")?;
    tq::update(
        &db,
        id,
        title.as_deref().unwrap_or(&task.title),
        description
            .as_deref()
            .unwrap_or(task.description.as_deref().unwrap_or("")),
        priority.unwrap_or(task.priority.unwrap_or(0)),
        task_type
            .as_deref()
            .unwrap_or(task.task_type.as_deref().unwrap_or("feature")),
        acceptance_criteria
            .as_deref()
            .unwrap_or(task.acceptance_criteria.as_deref().unwrap_or("")),
        model
            .as_deref()
            .unwrap_or(task.model.as_deref().unwrap_or("sonnet")),
        thinking_effort
            .as_deref()
            .unwrap_or(task.thinking_effort.as_deref().unwrap_or("medium")),
        if role_id.is_some() {
            role_id
        } else {
            task.role_id
        },
        tags.as_deref().or(task.tags.as_deref()),
    );
    let mut updated = tq::get_for_ui(&db, id).ok_or("Failed to retrieve updated task")?;
    updated.is_running = runner::is_running(id);
    app.emit("task:updated", &updated).ok();
    Ok(updated)
}

/// Whether moving to In Progress from `from` counts as starting the task.
///
/// A start runs against whatever its dependencies produced, so it is gated. A
/// resume continues a run that already exists and must not be: answering a
/// blocker, sending an agent back in after a discussion, and requesting a
/// revision all pass through In Progress, and gating them would strand a task
/// mid-flight whenever a parent was reopened while its agent waited.
fn start_is_gated(from: TaskStatus) -> bool {
    matches!(from, TaskStatus::Backlog | TaskStatus::Failed)
}

/// Refuse to start a task whose prerequisites have not run.
///
/// Names what has to happen first, because a Start that silently does nothing
/// reads as a broken button. Unmet ancestors are counted transitively — reporting
/// only the direct parent understates the work.
fn guard_dependencies(db: &db::DbPool, task_id: i64) -> Result<(), String> {
    let waves = db::dependencies::unmet_ancestor_waves(db, task_id);
    if waves.is_empty() {
        return Ok(());
    }
    let titles: Vec<String> = waves.iter().flatten().map(|t| t.title.clone()).collect();
    // A toast, not a report: name a few and count the rest.
    const SHOWN: usize = 3;
    let listed = titles
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let tail = if titles.len() > SHOWN {
        format!(", and {} more", titles.len() - SHOWN)
    } else {
        String::new()
    };
    Err(format!(
        "Blocked by {} unfinished task(s): {}{}",
        titles.len(),
        listed,
        tail
    ))
}

/// Whether a start may proceed over a stopped run.
///
/// Only ever [`Self::IgnoreStoppedRun`] for a person who was shown what the trunk is
/// missing and asked for it anyway. The queue and the MCP bridge always pass
/// [`Self::None`]: neither can read the warning, and an agent starting a task on a
/// broken trunk is the failure this whole path exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartOverride {
    None,
    IgnoreStoppedRun,
}

/// Returned when the task id does not exist. A sentinel so the HTTP route can answer
/// 404 rather than folding "no such task" into the same 400 as a refused start.
pub(crate) const ERR_NO_TASK: &str = "Task not found";

/// Returned when there is no `AppHandle` yet. A status change cannot be honoured
/// without one — see [`change_status_inner`] — and the HTTP route answers 503, because
/// the request was fine and retrying it later will work.
pub(crate) const ERR_NO_APP: &str = "The app is still starting up — try again in a moment";

/// Refuse to start a task whose run stopped short of putting its work on the trunk.
///
/// Separate from [`guard_dependencies`] because the remedy is different. Unmet
/// prerequisites are answered by running them; this is answered by resolving the run,
/// and the parents here report `done` — so the dependency guard sees nothing wrong
/// and would wave the task through onto a trunk missing the work it depends on.
fn guard_stopped_run(db: &db::DbPool, task_id: i64) -> Result<(), String> {
    match db::task_groups::stopped_for_task(db, task_id) {
        None => Ok(()),
        Some(group) => Err(format!(
            "This task's run stopped before all of its work reached {}. Resolve the run — carry on or abandon it — before starting this task.",
            group.trunk_branch
        )),
    }
}

/// Everything that can refuse a status change on the way in.
///
/// Split from [`change_status_inner`] so the gates can be tested against a database of
/// their own: that function reads the process-global pool, which a test cannot replace.
/// The gates are the part that has to behave identically for the UI and the MCP bridge,
/// so they are the part worth pinning down.
///
/// There is no dependency-blocked state — a task waiting on another task is not in the
/// same situation as one whose agent asked a question — so a refusal here leaves the
/// task exactly where it was.
fn guard_start(
    db: &db::DbPool,
    task_id: i64,
    from: TaskStatus,
    to: TaskStatus,
    over: StartOverride,
) -> Result<(), String> {
    if to != TaskStatus::InProgress || !start_is_gated(from) {
        return Ok(());
    }
    // The stopped run comes first. When a task has both, the run is the root cause and
    // the one with something to click; "blocked by 1 unfinished task" would name a
    // sibling that is itself only waiting on the same run.
    if over == StartOverride::None {
        guard_stopped_run(db, task_id)?;
    }
    // Never overridable. Overriding a stopped run is a judgement about a trunk the user
    // can see and inspect; an unmet prerequisite means the work does not exist at all,
    // and no confirmation makes it exist.
    guard_dependencies(db, task_id)
}

/// Run a task by running everything it depends on first.
///
/// The way forward from the refusal `change_task_status` gives a task with unmet
/// prerequisites. Creates a group over the target's unmet ancestor closure, cuts a
/// trunk branch for it, and starts the members that are ready. Each completion
/// carries the chain forward.
///
/// A single ready task gets no group: one branch off the base is already correct,
/// and a trunk plus two merges to land one task is ceremony with two extra chances
/// to conflict.
#[tauri::command]
pub fn start_task_with_prerequisites(
    app: AppHandle,
    id: i64,
    mcp_port: u16,
) -> Result<serde_json::Value, String> {
    use crate::services::orchestration::{self, ChainStart};
    let db = db::get_db();
    match orchestration::start_chain(&db, &app, id, mcp_port, orchestration::StopPolicy::Abandon)? {
        ChainStart::Single { task_id } => Ok(serde_json::json!({
            "groupId": null,
            "queued": [task_id],
            "trunkBranch": null,
        })),
        ChainStart::Grouped {
            group_id,
            queued,
            trunk,
        } => Ok(serde_json::json!({
            "groupId": group_id,
            "queued": queued,
            "trunkBranch": trunk,
        })),
        // Only the person who clicked needs telling; the queue skips these quietly.
        ChainStart::Claimed(claimed) => Err(format!(
            "{} of these tasks already belong to another run",
            claimed.len()
        )),
    }
}

/// The stopped run a task belongs to, or a refusal naming why there is nothing to do.
///
/// Both resolution commands go through this. The buttons are only rendered for a
/// stopped run, so arriving here without one means the board is stale — worth saying
/// rather than resolving nothing and reporting success.
fn resolvable_run(db: &db::DbPool, task_id: i64) -> Result<db::task_groups::TaskGroup, String> {
    db::task_groups::stopped_for_task(db, task_id)
        .ok_or_else(|| "This task is not part of a stopped run".to_string())
}

/// Carry a stopped run on from where it stopped.
///
/// Merges whatever is still missing from the trunk, then returns the run to active and
/// starts the members that are ready. Works whether the user merged the refused branch
/// by hand or fixed the cause and wants another attempt.
///
/// The run stays stopped if a branch still cannot merge. Carrying on into the same
/// refusal would start the rest of the chain against work that is still not there.
#[tauri::command]
pub fn resume_stopped_run(app: AppHandle, task_id: i64) -> Result<serde_json::Value, String> {
    let db = db::get_db();
    let group = resolvable_run(&db, task_id)?;
    let project = pq::get_by_id(&db, group.project_id).ok_or("Project not found")?;

    let members = db::task_groups::members(&db, group.id);
    match runner::carry_run_on(&db, Some(&app), &group, &project, task_id) {
        runner::RunResumeOutcome::StillRefused { branch, refusal } => Err(format!(
            "{} still cannot be merged into {}: {}",
            branch,
            group.trunk_branch,
            refusal.reason()
        )),
        runner::RunResumeOutcome::Ready { merged } => {
            let started: Vec<i64> = members
                .into_iter()
                .filter(|id| runner::is_running(*id) || runner::is_starting(*id))
                .collect();
            Ok(serde_json::json!({"resumed": true, "merged": merged, "started": started}))
        }
    }
}

/// What clicking Resolve should do.
#[derive(Debug)]
enum ResolveAction {
    /// Nothing has been tried: create a task for these branches and start it.
    Create(Vec<String>),
    /// A resolve task exists but never ran to a conclusion. Run it again.
    Restart(i64),
    /// Say why, and change nothing.
    Refuse(String),
}

/// Decide what Resolve does for this run, given the branches it still owes.
///
/// Separate from the command so the one-attempt rule can be tested without an app or
/// a repository: the branch debt arrives as an argument, and every other input is a
/// row. One resolve task per run, ever — no arm below creates a second while the
/// first exists in any status.
fn plan_resolve(
    db: &db::DbPool,
    group: &db::task_groups::TaskGroup,
    unresolved: &[String],
) -> ResolveAction {
    let existing = group
        .resolve_task_id
        .and_then(|id| tq::get_by_id(db, id).map(|t| (id, t)));

    if let Some((id, task)) = &existing {
        let status = task.status.as_deref().unwrap_or("");
        let in_flight = matches!(
            status,
            "in_progress" | "blocked" | "testing" | "awaiting_approval"
        ) || runner::is_running(*id)
            || runner::is_starting(*id);
        if in_flight {
            // An unreviewed resolution is in flight, not spent. The panel hides the
            // button in this state too, so reaching here means a stale board.
            return ResolveAction::Refuse(format!(
                "This run is already being resolved by \"{}\"",
                task.title
            ));
        }
    }

    if unresolved.is_empty() {
        return ResolveAction::Refuse(
            "There is nothing left to merge into this run's branch — use Carry on".to_string(),
        );
    }

    match existing {
        // The attempt crashed out or was never started. Another go at the same task
        // is not a second attempt: the first produced no resolution to judge.
        Some((id, task)) if matches!(task.status.as_deref(), Some("backlog") | Some("failed")) => {
            ResolveAction::Restart(id)
        }
        // Done, and the run is still stopped: the resolution was rejected at the
        // gate, or something else was refused afterwards. A second agent pass is the
        // retry loop the one-attempt rule exists to prevent.
        Some(_) => ResolveAction::Refuse(
            "This run has already used its resolve attempt — merge by hand and carry on, or abandon the run"
                .to_string(),
        ),
        // A resolve task the user deleted leaves nothing to inspect and nothing to
        // restart, so it holds nothing against the run.
        None => ResolveAction::Create(unresolved.to_vec()),
    }
}

/// Resolve a stopped run by putting a task on the board to perform the merge.
///
/// The task joins the run, so its worktree branches from the trunk and the run's
/// merged work is already in front of the agent. The run **stays stopped** while it
/// works: that is what holds the other members back, and it is why no resolution can
/// ever spawn another — a stop only fires for an active run.
///
/// Starting it needs [`StartOverride::IgnoreStoppedRun`], since it is a member of a
/// stopped run. That is the point rather than a workaround: putting the missing work
/// on the trunk is this task's job.
#[tauri::command]
pub fn resolve_stopped_run(
    app: AppHandle,
    task_id: i64,
    mcp_port: u16,
) -> Result<serde_json::Value, String> {
    let db = db::get_db();
    let group = resolvable_run(&db, task_id)?;
    let project = pq::get_by_id(&db, group.project_id).ok_or("Project not found")?;
    let unresolved = runner::unresolved_member_branches(&db, &group, &project.working_dir);

    let (resolve_id, created) = match plan_resolve(&db, &group, &unresolved) {
        ResolveAction::Refuse(msg) => return Err(msg),
        ResolveAction::Restart(id) => (id, false),
        ResolveAction::Create(branches) => (
            crate::services::resolve::create_resolve_task(&db, &project, &group, &branches)?,
            true,
        ),
    };

    change_status_inner(
        Some(&app),
        resolve_id,
        TaskStatus::InProgress.as_str(),
        mcp_port,
        StartOverride::IgnoreStoppedRun,
    )?;

    // The new card and the panel's changed hint both arrive this way; nothing on the
    // board polls, so a run whose members are not re-emitted keeps its old counts.
    for member in db::task_groups::members(&db, group.id) {
        runner::emit_task_updated(&db, &app, member);
    }

    Ok(serde_json::json!({"resolveTaskId": resolve_id, "created": created}))
}

/// The run's resolve task, if abandoning has to close it first.
///
/// Only an unfinished one. A resolution that is done, or was never started, needs
/// nothing done to it — and moving a finished task backwards would rewrite history
/// the edges are there to record.
fn resolve_task_to_close(db: &db::DbPool, group: &db::task_groups::TaskGroup) -> Option<i64> {
    let id = group.resolve_task_id?;
    let task = tq::get_by_id(db, id)?;
    matches!(
        task.status.as_deref(),
        Some("in_progress") | Some("blocked")
    )
    .then_some(id)
}

/// Give up on a stopped run, releasing its tasks so they can run in another.
///
/// The trunk is left alone. It holds whatever did merge, and deleting it here would
/// destroy the only copy of that work.
///
/// An unfinished resolve task is closed first. Left running, its agent would finish
/// into a group that no longer exists — `trunk_for_task` would answer `None`, and
/// with Auto Merge on its conflict-resolution branch would head for the project's
/// base branch instead of the trunk. Before membership is released, so there is no
/// instant where a live member belongs to no run.
#[tauri::command]
pub fn abandon_run(
    app: AppHandle,
    task_id: i64,
    mcp_port: u16,
) -> Result<serde_json::Value, String> {
    let db = db::get_db();
    let group = resolvable_run(&db, task_id)?;

    if let Some(resolve_id) = resolve_task_to_close(&db, &group) {
        // `failed`, not `backlog`: get_ready_tasks takes only backlog, so a freed
        // resolve task with no parents would become ordinary ready work and the queue
        // would re-run it against a trunk that is no longer anyone's target.
        // The transition stops its runner and cancels the question it was waiting on.
        if let Err(e) = change_status_inner(
            Some(&app),
            resolve_id,
            TaskStatus::Failed.as_str(),
            mcp_port,
            StartOverride::None,
        ) {
            log::error!("could not close resolve task {}: {}", resolve_id, e);
        }
    }

    db::task_groups::abandon_stopped(&db, &[task_id]);
    activity::add(
        &db,
        group.project_id,
        Some(task_id),
        "run_abandoned",
        &format!("Dependency run abandoned; {} kept", group.trunk_branch),
        None,
    );
    Ok(serde_json::json!({"abandoned": true, "trunk": group.trunk_branch}))
}

/// The waves that would run for a task, for the confirmation prompt.
#[tauri::command]
pub fn plan_prerequisites(id: i64) -> Vec<Vec<tq::Task>> {
    crate::services::orchestration::plan_prerequisites(&db::get_db(), id)
}

#[tauri::command]
pub fn change_task_status(
    app: AppHandle,
    id: i64,
    status: String,
    mcp_port: u16,
) -> Result<tq::Task, String> {
    change_status_inner(Some(&app), id, &status, mcp_port, StartOverride::None)
}

/// Start a task whose run stopped, on a trunk that may be missing work it depends on.
///
/// The escape hatch behind the warning in the stopped-run panel. Deliberately its own
/// command rather than a flag on [`change_task_status`]: that one is reachable from the
/// MCP bridge, and the override must not be. A person can read what the trunk is
/// missing and accept it; an agent cannot.
#[tauri::command]
pub fn start_task_despite_stopped_run(
    app: AppHandle,
    id: i64,
    mcp_port: u16,
) -> Result<tq::Task, String> {
    let db = db::get_db();
    // Read before the start and written after it. Overriding does not resolve the run,
    // so the trunk is still there to name afterwards — but the start can still be
    // refused for an unmet prerequisite, and logging first would record a start that
    // never happened.
    let trunk = db::task_groups::stopped_for_task(&db, id).map(|g| g.trunk_branch);

    let started = change_status_inner(
        Some(&app),
        id,
        TaskStatus::InProgress.as_str(),
        mcp_port,
        StartOverride::IgnoreStoppedRun,
    )?;

    if let Some(trunk) = trunk {
        tq::add_log(
            &db,
            id,
            &format!(
                "Started on {} while its run was stopped. The trunk may be missing work this task depends on.",
                trunk
            ),
            "error",
            None,
        );
        activity::add(
            &db,
            started.project_id,
            Some(id),
            "run_override",
            &format!("Started {} over a stopped run", started.title),
            None,
        );
    }
    Ok(started)
}

/// The one implementation of a status change, for both the UI and the MCP bridge.
///
/// `app` is optional only so the refusals above can be tested without a Tauri app, and
/// so the HTTP route can answer honestly before the app has finished starting. It is
/// required before the first write: every side effect of a status change needs it —
/// starting and stopping the runner above all — and writing the status without them is
/// what left an MCP-moved task sitting In Progress with no agent, invisible to the
/// queue because that only ever looks at Backlog.
pub(crate) fn change_status_inner(
    app: Option<&AppHandle>,
    id: i64,
    status: &str,
    mcp_port: u16,
    over: StartOverride,
) -> Result<tq::Task, String> {
    let db = db::get_db();
    let task = tq::get_by_id(&db, id).ok_or(ERR_NO_TASK)?;

    // ── Parse & validate via state machine ──
    let to = TaskStatus::from_str(status).ok_or("Invalid status")?;
    let from = TaskStatus::from_str(task.status.as_deref().unwrap_or("backlog"))
        .unwrap_or(TaskStatus::Backlog);

    if from != to && !is_valid_transition(from, to) {
        return Err(format!("Invalid transition: {} -> {}", from, to));
    }

    // Before the status is written, not after: a refused start leaves the task
    // exactly where it was, in Backlog, with nothing to roll back.
    guard_start(&db, id, from, to, over)?;

    // Nothing has been written yet, so a missing handle costs nothing to refuse.
    let app = app.ok_or(ERR_NO_APP)?;

    // ── Apply status in DB ──
    tq::update_status(&db, id, to.as_str());

    // ── Side effects by target status ──

    // Reset retry state when leaving failed
    if from == TaskStatus::Failed && (to == TaskStatus::Backlog || to == TaskStatus::InProgress) {
        tq::reset_retry_count(&db, id);
    }

    // The user moved a blocked task on rather than answering it. Close the
    // question with it: left open it would refuse the next raise, and the panel
    // would keep rendering a question nobody is waiting on.
    if from == TaskStatus::Blocked {
        crate::commands::blockers::cancel_open_blocker_for_task(&db, id);
    }

    if to == TaskStatus::InProgress {
        // Timer management
        if task.started_at.is_none() {
            tq::set_started(&db, id);
        } else {
            tq::set_resumed(&db, id);
        }
        // Reset retry count when manually starting
        if task.retry_count.unwrap_or(0) > 0 {
            tq::reset_retry_count(&db, id);
        }
    }

    if to == TaskStatus::Testing && from == TaskStatus::InProgress {
        tq::pause_timer(&db, id);
    }

    if to == TaskStatus::Done {
        if task.completed_at.is_none() {
            tq::finalize_timer(&db, id);
        }
        activity::add(
            &db,
            task.project_id,
            Some(id),
            "task_approved",
            &format!("Task approved: {}", task.title),
            None,
        );
        execute_done_side_effects(&db, app, id, &task);
    }

    if to == TaskStatus::Backlog {
        tq::reset_retry_count(&db, id);
    }

    // ── Runner lifecycle ──
    let updated = tq::get_by_id(&db, id).ok_or("Task not found after status update")?;

    if to == TaskStatus::InProgress && from != TaskStatus::InProgress {
        let project = pq::get_by_id(&db, task.project_id).ok_or("Project not found")?;
        if !runner::start(
            &updated,
            app.clone(),
            &project.working_dir,
            &project,
            mcp_port,
        ) {
            log::error!(
                "Failed to start runner for task {}, reverting status to {}",
                id,
                from
            );
            tq::update_status(&db, id, from.as_str());
        } else {
            activity::add(
                &db,
                task.project_id,
                Some(id),
                "task_started",
                &format!("Task started: {}", task.title),
                None,
            );
        }
    }

    // Stop runner when leaving active state
    if to != TaskStatus::InProgress && runner::is_running(id) {
        runner::stop(id, &db, app);
    }

    // Cascade queue when freeing a slot
    if from == TaskStatus::InProgress && (to == TaskStatus::Done || to == TaskStatus::Testing) {
        queue::start_next_queued(&db, app, task.project_id);
    }

    // Cascade when approving: AwaitingApproval -> Done unblocks dependents
    if from == TaskStatus::AwaitingApproval && to == TaskStatus::Done {
        queue::on_task_completed(&db, app, task.project_id, id);
    }

    let mut final_task = tq::get_for_ui(&db, id).ok_or("Task not found")?;
    final_task.is_running = runner::is_running(id) || runner::is_starting(id);
    app.emit("task:updated", &final_task).ok();

    // Propagate status change to both DB roadmap (plan/phase) and file-based
    // GSD roadmap (.planning/ROADMAP.md). Single choke-point so every mutation
    // path keeps the two in sync.
    crate::services::gsd::apply_task_status_cascade(&db, Some(app), id);

    Ok(final_task)
}

/// Side effects when a task transitions to Done (manual approval).
fn execute_done_side_effects(db: &crate::db::DbPool, app: &AppHandle, id: i64, task: &tq::Task) {
    if let Some(project) = pq::get_by_id(db, task.project_id) {
        let fresh_task = tq::get_by_id(db, id).unwrap_or(task.clone());
        // Use worktree dir for PR creation (where commits live), fall back to project dir
        let pr_dir = runner::get_task_worktree(id).unwrap_or_else(|| project.working_dir.clone());
        runner::auto_create_pr_public(&fresh_task, &pr_dir, &project, db, app);
        let after_pr = tq::get_by_id(db, id).unwrap_or(fresh_task.clone());
        // Cleanup uses project root (manages worktrees and branches)
        let cleanup = runner::cleanup_task_branch(&after_pr, &project.working_dir, &project, db);
        runner::report_task_branch_outcome(cleanup, id, db, app);
        // Approving the group's target task is what lands its trunk on the base.
        if let Some(landed) =
            runner::finish_group_if_complete(db, id, &project.working_dir, &project)
        {
            runner::report_branch_cleanup(landed, id, db, app);
        }

        // Auto-close linked GitHub issue
        if project.github_sync_enabled.unwrap_or(0) == 1 {
            if let Some(issue_num) = fresh_task.github_issue_number {
                let repo = project.github_repo.as_deref().unwrap_or("");
                if !repo.is_empty() {
                    let pr_url = after_pr.pr_url.as_deref().unwrap_or("");
                    let task_key = fresh_task.task_key.as_deref().unwrap_or("");
                    let comment_body = if !pr_url.is_empty() {
                        format!(
                            "Completed via Claude Board task `{}`. PR: {}",
                            task_key, pr_url
                        )
                    } else {
                        format!("Completed via Claude Board task `{}`.", task_key)
                    };
                    let repo_owned = repo.to_string();
                    std::thread::spawn(move || {
                        if let Ok(token) = crate::commands::github::get_gh_token_pub() {
                            let _ = crate::services::github_sync::close_and_comment(
                                &token,
                                &repo_owned,
                                issue_num,
                                &comment_body,
                            );
                        }
                    });
                }
            }
        }
    }
}

#[tauri::command]
pub fn delete_task(app: AppHandle, id: i64) -> Result<(), String> {
    let db = db::get_db();
    let task = tq::get_by_id(&db, id).ok_or("Task not found")?;
    if runner::is_running(id) || runner::is_starting(id) {
        runner::stop(id, &db, &app);
    }
    // Notify children that their parent is being removed
    let children = db::dependencies::get_child_ids(&db, id);
    db::dependencies::remove_all_for_task(&db, id).map_err(|e| e.to_string())?;
    tq::delete(&db, id);
    app.emit("task:deleted", &serde_json::json!({"id": task.id}))
        .ok();
    // Emit updates for children so they refresh dependency state
    for child_id in children {
        if let Some(child) = tq::get_by_id(&db, child_id) {
            app.emit("task:updated", &child).ok();
        }
    }
    Ok(())
}

#[tauri::command]
pub fn get_task_logs(id: i64, limit: Option<i64>) -> Vec<tq::TaskLog> {
    let db = db::get_db();
    let mut logs = tq::get_recent_logs(&db, id, limit.unwrap_or(500));
    logs.reverse();
    logs
}

#[tauri::command]
pub fn stop_task(app: AppHandle, id: i64) {
    let db = db::get_db();
    runner::stop(id, &db, &app);
}

#[tauri::command]
pub fn restart_task(app: AppHandle, id: i64, mcp_port: u16) -> Result<tq::Task, String> {
    let db = db::get_db();
    let task = tq::get_by_id(&db, id).ok_or("Task not found")?;
    // Restart clears the logs and runs the task again from the beginning, so it is
    // a start and gets the same gate as one.
    guard_dependencies(&db, id)?;
    runner::stop(id, &db, &app);
    tq::clear_logs(&db, id);
    tq::update_status(&db, id, "in_progress");
    let updated = tq::get_by_id(&db, id).ok_or("Task not found after restart")?;
    let project = pq::get_by_id(&db, task.project_id).ok_or("Project not found")?;
    runner::start(
        &updated,
        app.clone(),
        &project.working_dir,
        &project,
        mcp_port,
    );
    if let Ok(mut val) = serde_json::to_value(&updated) {
        if let Some(obj) = val.as_object_mut() {
            obj.insert("is_running".into(), serde_json::Value::Bool(true));
        }
        app.emit("task:updated", &val).ok();
    }
    Ok(updated)
}

#[tauri::command]
pub fn request_changes(
    app: AppHandle,
    id: i64,
    feedback: String,
    mcp_port: u16,
) -> Result<tq::Task, String> {
    let db = db::get_db();
    let task = tq::get_by_id(&db, id).ok_or("Task not found")?;
    let current = TaskStatus::from_str(task.status.as_deref().unwrap_or("backlog"))
        .unwrap_or(TaskStatus::Backlog);
    if current != TaskStatus::Testing && current != TaskStatus::Done {
        return Err(format!(
            "Cannot request changes on task in '{}' status",
            current
        ));
    }
    if feedback.trim().is_empty() {
        return Err("Feedback is required".into());
    }
    // Stop any running process (auto-test) before restarting with revision
    if runner::is_running(id) {
        runner::stop(id, &db, &app);
    }

    tq::increment_revision_count(&db, id);
    let rev_num = tq::get_by_id(&db, id)
        .map(|t| t.revision_count.unwrap_or(1))
        .unwrap_or(1);
    tq::add_revision(&db, id, rev_num, feedback.trim());
    tq::update_status(&db, id, "in_progress");
    let updated = tq::get_by_id(&db, id).ok_or("Task not found")?;
    let project = pq::get_by_id(&db, task.project_id).ok_or("Project not found")?;
    runner::start(
        &updated,
        app.clone(),
        &project.working_dir,
        &project,
        mcp_port,
    );
    activity::add(
        &db,
        task.project_id,
        Some(id),
        "revision_requested",
        &format!("Revision #{}: {}", rev_num, task.title),
        Some(&serde_json::json!({"feedback": feedback.trim()}).to_string()),
    );
    crate::services::notification::notify_revision_requested(
        &app,
        &crate::services::notification::TaskNotification::new(
            &task.title,
            task.task_key.as_deref(),
        ),
    );
    crate::services::webhook::fire(
        task.project_id,
        "revision_requested",
        &format!("Revision #{}: {}", rev_num, task.title),
        serde_json::json!({"taskId": id, "taskKey": task.task_key, "title": task.title, "revision": rev_num, "feedback": feedback.trim()}),
    );
    let mut final_task = tq::get_for_ui(&db, id).ok_or("Task not found")?;
    final_task.is_running = runner::is_running(id) || runner::is_starting(id);
    app.emit("task:updated", &final_task).ok();
    Ok(final_task)
}

#[tauri::command]
pub fn get_revisions(id: i64) -> Vec<tq::TaskRevision> {
    tq::get_revisions(&db::get_db(), id)
}

#[tauri::command]
#[allow(non_snake_case)]
pub fn get_task_events(taskId: i64, limit: Option<i64>) -> Vec<serde_json::Value> {
    let db = db::get_db();
    let conn = db.lock();
    let lim = limit.unwrap_or(500);
    let mut stmt = match conn.prepare(
        "SELECT id, event_type, event_data, timestamp_ms FROM task_events
         WHERE task_id=?1 ORDER BY timestamp_ms ASC LIMIT ?2",
    ) {
        Ok(s) => s,
        Err(e) => {
            log::error!("get_task_events prepare: {}", e);
            return vec![];
        }
    };
    let result: Vec<serde_json::Value> = match stmt.query_map(rusqlite::params![taskId, lim], |r| {
        let data_str: String = r.get(2)?;
        let data: serde_json::Value =
            serde_json::from_str(&data_str).unwrap_or(serde_json::json!({}));
        Ok(serde_json::json!({
            "id": r.get::<_, i64>(0)?,
            "eventType": r.get::<_, String>(1)?,
            "data": data,
            "timestampMs": r.get::<_, i64>(3)?,
        }))
    }) {
        Ok(rows) => rows.flatten().collect(),
        Err(e) => {
            log::error!("get_task_events query: {}", e);
            vec![]
        }
    };
    result
}

#[tauri::command]
pub fn get_task_detail(id: i64) -> Result<serde_json::Value, String> {
    let db = db::get_db();
    let task = tq::get_by_id(&db, id).ok_or("Task not found")?;
    let revisions = tq::get_revisions(&db, id);
    let task_attachments = attachments::get_by_task(&db, id);
    let commits: serde_json::Value = task
        .commits
        .as_deref()
        .and_then(|c| serde_json::from_str(c).ok())
        .unwrap_or(serde_json::json!([]));

    let mut val = serde_json::to_value(&task).map_err(|e| e.to_string())?;
    if let Some(obj) = val.as_object_mut() {
        obj.insert("commits".into(), commits);
        obj.insert(
            "revisions".into(),
            serde_json::to_value(revisions).unwrap_or_default(),
        );
        obj.insert(
            "attachments".into(),
            serde_json::to_value(task_attachments).unwrap_or_default(),
        );
        obj.insert(
            "is_running".into(),
            serde_json::Value::Bool(runner::is_running(id)),
        );
    }
    Ok(val)
}

#[tauri::command]
pub fn reorder_queue(project_id: i64, task_ids: Vec<i64>) -> Vec<tq::Task> {
    let db = db::get_db();
    for (i, id) in task_ids.iter().enumerate() {
        tq::update_queue_position(&db, *id, i as i64);
    }
    tq::get_by_project(&db, project_id)
        .into_iter()
        .map(|mut t| {
            t.is_running = runner::is_running(t.id);
            t
        })
        .collect()
}

#[tauri::command]
pub fn reorder_tasks(task_ids: Vec<i64>) {
    let db = db::get_db();
    for (i, id) in task_ids.iter().enumerate() {
        tq::update_sort_order(&db, *id, i as i64);
    }
}

#[tauri::command]
pub fn set_task_dependency(
    app: AppHandle,
    id: i64,
    depends_on: Option<i64>,
) -> Result<tq::Task, String> {
    let db = db::get_db();
    let _task = tq::get_by_id(&db, id).ok_or("Task not found")?;
    if let Some(dep_id) = depends_on {
        if dep_id == id {
            return Err("Task cannot depend on itself".into());
        }
        if tq::get_by_id(&db, dep_id).is_none() {
            return Err("Dependency task not found".into());
        }
    }
    tq::update_depends_on(&db, id, depends_on);
    let updated = tq::get_for_ui(&db, id).ok_or("Task not found")?;
    app.emit("task:updated", &updated).ok();
    Ok(updated)
}

#[tauri::command]
#[allow(non_snake_case)]
pub fn add_task_dependency(
    app: AppHandle,
    taskId: i64,
    dependsOnId: i64,
    conditionType: Option<String>,
) -> Result<serde_json::Value, String> {
    let db = db::get_db();
    tq::get_by_id(&db, taskId).ok_or("Task not found")?;
    tq::get_by_id(&db, dependsOnId).ok_or("Parent task not found")?;
    db::dependencies::add_dependency(&db, taskId, dependsOnId, conditionType.as_deref())
        .map_err(|e| e.to_string())?;
    let updated = tq::get_for_ui(&db, taskId).ok_or("Task not found")?;
    app.emit("task:updated", &updated).ok();
    Ok(serde_json::json!({
        "task": updated,
        "parents": db::dependencies::get_parent_ids(&db, taskId),
        "children": db::dependencies::get_child_ids(&db, taskId),
    }))
}

#[tauri::command]
#[allow(non_snake_case)]
pub fn remove_task_dependency(
    app: AppHandle,
    taskId: i64,
    dependsOnId: i64,
) -> Result<serde_json::Value, String> {
    let db = db::get_db();
    db::dependencies::remove_dependency(&db, taskId, dependsOnId).map_err(|e| e.to_string())?;
    let updated = tq::get_for_ui(&db, taskId).ok_or("Task not found")?;
    app.emit("task:updated", &updated).ok();
    Ok(serde_json::json!({
        "task": updated,
        "parents": db::dependencies::get_parent_ids(&db, taskId),
        "children": db::dependencies::get_child_ids(&db, taskId),
    }))
}

#[tauri::command]
#[allow(non_snake_case)]
pub fn get_task_dependencies(taskId: i64) -> serde_json::Value {
    let db = db::get_db();
    let parents = db::dependencies::get_parent_ids(&db, taskId);
    let children = db::dependencies::get_child_ids(&db, taskId);
    serde_json::json!({ "parents": parents, "children": children })
}

#[tauri::command]
pub fn get_execution_waves(project_id: i64) -> Vec<Vec<tq::Task>> {
    let db = db::get_db();
    db::dependencies::get_execution_waves(&db, project_id)
}

#[tauri::command]
pub fn get_dependency_graph(project_id: i64) -> serde_json::Value {
    let db = db::get_db();
    db::dependencies::get_graph_data(&db, project_id)
}

#[tauri::command]
pub fn get_pipeline_status(project_id: i64) -> serde_json::Value {
    let db = db::get_db();
    let tasks = tq::get_by_project(&db, project_id);
    let running: Vec<_> = tasks
        .iter()
        .filter(|t| {
            t.status.as_deref() == Some(TaskStatus::InProgress.as_str()) || runner::is_running(t.id)
        })
        .collect();
    let queued: Vec<_> = tasks
        .iter()
        .filter(|t| t.status.as_deref() == Some(TaskStatus::Backlog.as_str()))
        .collect();
    let completed: Vec<_> = tasks
        .iter()
        .filter(|t| matches!(t.status.as_deref(), Some("done") | Some("testing")))
        .collect();
    let total_cost: f64 = tasks.iter().map(|t| t.total_cost.unwrap_or(0.0)).sum();
    let total_tokens: i64 = tasks
        .iter()
        .map(|t| t.input_tokens.unwrap_or(0) + t.output_tokens.unwrap_or(0))
        .sum();
    let avg_duration: i64 = {
        let durations: Vec<i64> = completed
            .iter()
            .filter_map(|t| t.work_duration_ms)
            .filter(|d| *d > 0)
            .collect();
        if durations.is_empty() {
            0
        } else {
            durations.iter().sum::<i64>() / durations.len() as i64
        }
    };

    let waves = db::dependencies::get_execution_waves(&db, project_id);
    let failed: Vec<_> = tasks
        .iter()
        .filter(|t| t.status.as_deref() == Some(TaskStatus::Failed.as_str()))
        .collect();
    let awaiting_approval: Vec<_> = tasks
        .iter()
        .filter(|t| t.status.as_deref() == Some("awaiting_approval"))
        .collect();

    // Circuit breaker status
    let project = pq::get_by_id(&db, project_id);
    let circuit_breaker_active = project
        .as_ref()
        .map(|p| p.circuit_breaker_active.unwrap_or(0) == 1)
        .unwrap_or(false);
    let circuit_breaker_threshold = project
        .as_ref()
        .and_then(|p| p.circuit_breaker_threshold)
        .unwrap_or(0);
    let consecutive_failures = project
        .as_ref()
        .and_then(|p| p.consecutive_failures)
        .unwrap_or(0);

    // Bottlenecks: tasks that block the most other tasks
    let bottlenecks: Vec<serde_json::Value> = {
        let conn = db.lock();
        let mut stmt = match conn.prepare(
            "SELECT t.id, t.title, t.status, COUNT(cd.task_id) as blocker_count
             FROM tasks t
             JOIN task_dependencies cd ON cd.depends_on_id = t.id
             WHERE t.project_id = ?1 AND t.deleted_at IS NULL AND t.status NOT IN ('done')
             GROUP BY t.id
             ORDER BY blocker_count DESC
             LIMIT 5",
        ) {
            Ok(s) => s,
            Err(_) => return serde_json::json!({}),
        };
        let result: Vec<serde_json::Value> = stmt
            .query_map(rusqlite::params![project_id], |r| {
                Ok(serde_json::json!({
                    "taskId": r.get::<_, i64>(0)?,
                    "title": r.get::<_, String>(1)?,
                    "status": r.get::<_, String>(2)?,
                    "blockerCount": r.get::<_, i64>(3)?,
                }))
            })
            .ok()
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
        result
    };

    // Burn rate: tokens per minute based on running tasks
    let burn_rate: f64 = running
        .iter()
        .filter_map(|t| {
            let started = t.started_at.as_ref()?;
            let elapsed_sec = chrono::NaiveDateTime::parse_from_str(started, "%Y-%m-%d %H:%M:%S")
                .ok()
                .map(|d| (chrono::Local::now().naive_local() - d).num_seconds())?;
            if elapsed_sec <= 0 {
                return None;
            }
            let tokens = (t.input_tokens.unwrap_or(0) + t.output_tokens.unwrap_or(0)) as f64;
            Some(tokens / (elapsed_sec as f64 / 60.0))
        })
        .sum();

    serde_json::json!({
        "running": running.len(),
        "queued": queued.len(),
        "completed": completed.len(),
        "failed": failed.len(),
        "awaitingApproval": awaiting_approval.len(),
        "total": tasks.len(),
        "totalCost": total_cost,
        "totalTokens": total_tokens,
        "avgDurationMs": avg_duration,
        "waves": waves.len(),
        "circuitBreakerActive": circuit_breaker_active,
        "circuitBreakerThreshold": circuit_breaker_threshold,
        "consecutiveFailures": consecutive_failures,
        "bottlenecks": bottlenecks,
        "burnRate": burn_rate,
        "tasks": {
            "running": running,
            "queued": queued,
        }
    })
}

#[tauri::command]
#[allow(non_snake_case)]
pub fn get_task_diff(taskId: i64) -> Result<serde_json::Value, String> {
    let db = db::get_db();
    let task = tq::get_by_id(&db, taskId).ok_or("Task not found")?;
    let project = pq::get_by_id(&db, task.project_id).ok_or("Project not found")?;
    let working_dir = &project.working_dir;

    let exec = |args: &[&str]| -> Option<String> {
        let mut cmd = crate::child_env::command("git");
        cmd.args(args)
            .current_dir(working_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }
        cmd.output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };

    // Parse task commits to find the range
    let commits: Vec<serde_json::Value> = task
        .commits
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    let diff = if commits.len() >= 2 {
        // Multiple commits: diff from parent of first to last
        let first = commits
            .last()
            .and_then(|c| c.get("short").and_then(|v| v.as_str()))
            .unwrap_or("HEAD~1");
        let last = commits
            .first()
            .and_then(|c| c.get("short").and_then(|v| v.as_str()))
            .unwrap_or("HEAD");
        exec(&["diff", "--no-color", &format!("{}~1..{}", first, last)]).unwrap_or_default()
    } else if commits.len() == 1 {
        let hash = commits[0]
            .get("short")
            .and_then(|v| v.as_str())
            .unwrap_or("HEAD");
        exec(&["diff", "--no-color", &format!("{}~1..{}", hash, hash)]).unwrap_or_default()
    } else {
        // Fallback: last commit
        exec(&["diff", "--no-color", "HEAD~1..HEAD"]).unwrap_or_default()
    };

    // Truncate if too large (max ~200KB), safe for UTF-8
    let diff = if diff.len() > 200_000 {
        let mut end = 200_000;
        while end > 0 && !diff.is_char_boundary(end) {
            end -= 1;
        }
        format!(
            "{}\n\n--- Diff truncated ({} bytes total) ---",
            &diff[..end],
            diff.len()
        )
    } else {
        diff
    };

    Ok(serde_json::json!({ "diff": diff }))
}

// ─── Observability & Collaboration Commands ───

#[tauri::command]
pub fn get_active_file_map() -> serde_json::Value {
    let map = crate::claude::events::get_file_access_map();
    serde_json::to_value(map).unwrap_or(serde_json::json!({}))
}

#[tauri::command]
pub fn get_agent_activity(project_id: i64) -> serde_json::Value {
    let db = db::get_db();
    let tasks = tq::get_by_project(&db, project_id);
    let file_map = crate::claude::events::get_file_access_map();

    let agents: Vec<serde_json::Value> = tasks.iter()
        .filter(|t| t.status.as_deref() == Some(TaskStatus::InProgress.as_str()) || runner::is_running(t.id))
        .map(|t| {
            // Get recent tool calls from logs
            let conn = db.lock();
            let recent_tools: Vec<serde_json::Value> = conn.prepare(
                "SELECT message, meta, created_at FROM task_logs WHERE task_id=?1 AND log_type='tool' ORDER BY id DESC LIMIT 20"
            ).ok().map(|mut stmt| {
                stmt.query_map(rusqlite::params![t.id], |r| {
                    let msg: String = r.get(0)?;
                    let meta: Option<String> = r.get(1)?;
                    let created: Option<String> = r.get(2)?;
                    let meta_val = meta.and_then(|m| serde_json::from_str::<serde_json::Value>(&m).ok());
                    Ok(serde_json::json!({
                        "message": msg,
                        "meta": meta_val,
                        "created_at": created,
                    }))
                }).ok().map(|rows| rows.flatten().collect()).unwrap_or_default()
            }).unwrap_or_default();
            let tool_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM task_logs WHERE task_id=?1 AND log_type='tool'",
                rusqlite::params![t.id], |r| r.get(0),
            ).unwrap_or(0);
            drop(conn);

            // Files this agent is accessing
            let agent_files: Vec<String> = file_map.iter()
                .filter(|(_, task_ids)| task_ids.contains(&t.id))
                .map(|(path, _)| path.clone())
                .collect();

            let elapsed: i64 = t.started_at.as_ref().and_then(|s| {
                chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").ok()
                    .map(|d| (chrono::Local::now().naive_local() - d).num_seconds())
            }).unwrap_or(0);

            serde_json::json!({
                "taskId": t.id,
                "taskKey": t.task_key,
                "title": t.title,
                "model": t.model_used.as_ref().or(t.model.as_ref()),
                "startedAt": t.started_at,
                "elapsedSec": elapsed,
                "inputTokens": t.input_tokens.unwrap_or(0),
                "outputTokens": t.output_tokens.unwrap_or(0),
                "totalCost": t.total_cost.unwrap_or(0.0),
                "toolCallCount": tool_count,
                "recentTools": recent_tools,
                "activeFiles": agent_files,
                "isRunning": runner::is_running(t.id),
                "awaitingSubtasks": t.awaiting_subtasks.unwrap_or(0) == 1,
            })
        })
        .collect();

    // Detect conflicts
    let conflicts: Vec<serde_json::Value> = file_map
        .iter()
        .filter(|(_, task_ids)| task_ids.len() > 1)
        .map(|(path, task_ids)| {
            serde_json::json!({
                "filePath": path,
                "taskIds": task_ids,
            })
        })
        .collect();

    serde_json::json!({
        "agents": agents,
        "fileMap": file_map,
        "conflicts": conflicts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use rusqlite::{params, Connection};
    use std::sync::Arc;

    fn test_db() -> db::DbPool {
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

    #[test]
    fn a_single_task_read_carries_the_markers_the_board_computes() {
        let db = test_db();
        let parent = seed_task(&db, "parent");
        let child = seed_task(&db, "child");
        db::dependencies::add_dependency(&db, child, parent, None).unwrap();
        let gid = db::task_groups::create(&db, 1, "trunk/m", "main", child, &[child]).unwrap();
        db::task_groups::set_status(&db, gid, db::task_groups::STATUS_STOPPED).unwrap();

        // get_by_id leaves these at their defaults, and every task:updated payload
        // came from it — so each event overwrote what the list query had worked out
        // and the waiting and run-stopped markers vanished from the card.
        let plain = tq::get_by_id(&db, child).unwrap();
        assert_eq!((plain.waiting_on, plain.run_stopped), (0, false));

        let hydrated = tq::get_for_ui(&db, child).unwrap();

        assert_eq!(hydrated.waiting_on, 1);
        assert_eq!(hydrated.trunk_branch.as_deref(), Some("trunk/m"));
        assert!(hydrated.run_stopped);
        assert_eq!(hydrated.resolve_task_id, None);

        // And once a resolution exists, every member's card carries its id: that is
        // how the panel says who is already resolving instead of offering a second.
        let r = seed_task(&db, "resolve");
        db::task_groups::add_member(&db, gid, r).unwrap();
        db::task_groups::set_resolve_task(&db, gid, r).unwrap();
        assert_eq!(
            tq::get_for_ui(&db, child).unwrap().resolve_task_id,
            Some(r),
            "the single-task read"
        );
        assert_eq!(
            hydrate_board(&db, 1)
                .into_iter()
                .find(|t| t.id == child)
                .and_then(|t| t.resolve_task_id),
            Some(r),
            "and the board query, which is a different query"
        );
    }

    #[test]
    fn a_hydrated_task_stops_claiming_a_run_that_ended() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let gid = db::task_groups::create(&db, 1, "trunk/gone", "main", a, &[a]).unwrap();
        db::task_groups::finish(&db, gid, db::task_groups::STATUS_COMPLETED).unwrap();

        // The other direction matters as much: a marker that outlives its run names a
        // branch that has been deleted.
        let hydrated = tq::get_for_ui(&db, a).unwrap();

        assert_eq!(hydrated.trunk_branch, None);
        assert!(!hydrated.run_stopped);
    }

    #[test]
    fn resolving_a_task_that_is_not_in_a_stopped_run_is_refused() {
        let db = test_db();
        let a = seed_task(&db, "a");

        // Nothing to resolve, and no run to name. Reporting success here would tell
        // the user a stopped run had been carried on when none existed.
        assert!(resolvable_run(&db, a).is_err());

        let gid = db::task_groups::create(&db, 1, "trunk/x", "main", a, &[a]).unwrap();
        // An active run is not resolvable either: its members are still working.
        assert!(resolvable_run(&db, a).is_err());

        db::task_groups::set_status(&db, gid, db::task_groups::STATUS_STOPPED).unwrap();
        assert_eq!(resolvable_run(&db, a).map(|g| g.id).ok(), Some(gid));
    }

    fn seed_task(db: &db::DbPool, title: &str) -> i64 {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO tasks (project_id,title,status) VALUES (1,?1,'backlog')",
            params![title],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn set_status(db: &db::DbPool, id: i64, status: &str) {
        db.lock()
            .execute(
                "UPDATE tasks SET status=?2 WHERE id=?1",
                params![id, status],
            )
            .unwrap();
    }

    /// A stopped run over one member, and the debt it stopped owing.
    fn stopped_run(db: &db::DbPool) -> db::task_groups::TaskGroup {
        let a = seed_task(db, "a");
        let gid = db::task_groups::create(db, 1, "trunk/r", "main", a, &[a]).unwrap();
        db::task_groups::set_status(db, gid, db::task_groups::STATUS_STOPPED).unwrap();
        db::task_groups::get(db, gid).unwrap()
    }

    /// Attach a resolve task in `status` to `group`, as create_resolve_task would,
    /// refreshing the caller's copy of the group so it carries the new id.
    fn with_resolve_task(
        db: &db::DbPool,
        group: &mut db::task_groups::TaskGroup,
        status: &str,
    ) -> i64 {
        let r = seed_task(db, "Resolve merge conflict: feature/x → trunk/r");
        set_status(db, r, status);
        db::task_groups::add_member(db, group.id, r).unwrap();
        db::task_groups::set_resolve_task(db, group.id, r).unwrap();
        *group = db::task_groups::get(db, group.id).unwrap();
        r
    }

    const DEBT: [&str; 1] = ["feature/x"];

    fn debt() -> Vec<String> {
        DEBT.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn abandoning_names_the_resolve_task_that_must_be_closed_with_it() {
        // Left running, an abandoned run's resolve agent would finish into a group
        // that no longer exists: trunk_for_task answers None, and with Auto Merge on
        // its conflict-resolution branch would head for the base branch itself.
        for status in ["in_progress", "blocked"] {
            let db = test_db();
            let mut group = stopped_run(&db);
            let r = with_resolve_task(&db, &mut group, status);
            assert_eq!(resolve_task_to_close(&db, &group), Some(r), "at {status}");
        }

        // Finished, and never started: nothing to close either way. Moving a done
        // resolution backwards would rewrite the record its edges point at.
        for status in ["done", "backlog", "failed"] {
            let db = test_db();
            let mut group = stopped_run(&db);
            with_resolve_task(&db, &mut group, status);
            assert_eq!(resolve_task_to_close(&db, &group), None, "at {status}");
        }

        let db = test_db();
        let group = stopped_run(&db);
        assert_eq!(resolve_task_to_close(&db, &group), None, "no resolve task");
    }

    #[test]
    fn resolving_creates_one_task_when_none_exists() {
        let db = test_db();
        let group = stopped_run(&db);

        assert!(matches!(
            plan_resolve(&db, &group, &debt()),
            ResolveAction::Create(ref b) if b == &debt()
        ));
    }

    #[test]
    fn a_run_being_resolved_refuses_a_second_resolve_task() {
        // The no-recursion rule as a guard rather than a hope: while a resolution is
        // in flight — running, blocked on a question, or waiting for review — the
        // answer to "resolve this" is "it already is".
        for status in ["in_progress", "blocked", "testing", "awaiting_approval"] {
            let db = test_db();
            let mut group = stopped_run(&db);
            with_resolve_task(&db, &mut group, status);

            match plan_resolve(&db, &group, &debt()) {
                ResolveAction::Refuse(msg) => assert!(
                    msg.contains("Resolve merge conflict"),
                    "the refusal names the task at {status}: {msg}"
                ),
                other => panic!("expected a refusal at {status}, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_crashed_resolve_task_is_restarted_rather_than_duplicated() {
        for status in ["failed", "backlog"] {
            let db = test_db();
            let mut group = stopped_run(&db);
            let r = with_resolve_task(&db, &mut group, status);

            // Same task again is not a second attempt in the one-attempt sense: the
            // first never produced a resolution to judge.
            assert!(
                matches!(plan_resolve(&db, &group, &debt()), ResolveAction::Restart(id) if id == r),
                "at {status}"
            );
        }
    }

    #[test]
    fn a_spent_resolve_attempt_is_refused_for_good() {
        // The resolve task is done and the run is still stopped — the gate
        // quarantined its work, or a second branch was refused later. Another agent
        // pass is exactly the retry loop the one-attempt rule forbids; the user
        // resolves by hand, starts anyway, or abandons.
        let db = test_db();
        let mut group = stopped_run(&db);
        with_resolve_task(&db, &mut group, "done");

        assert!(matches!(
            plan_resolve(&db, &group, &debt()),
            ResolveAction::Refuse(_)
        ));
    }

    #[test]
    fn a_run_with_nothing_unresolved_is_sent_to_carry_on() {
        // A resolve task with an empty job would run an agent to do nothing and then
        // report success. The user merged by hand; Carry on is the button they want.
        let db = test_db();
        let group = stopped_run(&db);

        match plan_resolve(&db, &group, &[]) {
            ResolveAction::Refuse(msg) => assert!(msg.contains("Carry on"), "got: {msg}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_resolve_task_the_user_deleted_is_not_held_against_the_run() {
        // Deleting the card takes its membership and its edges with it, so refusing
        // with "this run already used its resolve attempt" would name a task that is
        // not on the board. Nothing is left to inspect, so nothing is spent.
        let db = test_db();
        let mut group = stopped_run(&db);
        let r = with_resolve_task(&db, &mut group, "done");
        db.lock()
            .execute("DELETE FROM tasks WHERE id=?1", params![r])
            .unwrap();

        assert!(matches!(
            plan_resolve(&db, &group, &debt()),
            ResolveAction::Create(_)
        ));
    }

    #[test]
    fn starting_a_task_with_an_unfinished_dependency_is_refused() {
        let db = test_db();
        let parent = seed_task(&db, "build the parser");
        let child = seed_task(&db, "child");
        db::dependencies::add_dependency(&db, child, parent, None).unwrap();

        let err = guard_dependencies(&db, child).expect_err("should be refused");

        // The message has to name what to do about it, not just say no.
        assert!(err.contains("build the parser"), "got: {err}");
    }

    #[test]
    fn starting_a_ready_task_is_allowed() {
        let db = test_db();
        let child = seed_task(&db, "child");

        assert!(guard_dependencies(&db, child).is_ok());
    }

    #[test]
    fn a_finished_dependency_stops_blocking() {
        let db = test_db();
        let parent = seed_task(&db, "parent");
        let child = seed_task(&db, "child");
        db::dependencies::add_dependency(&db, child, parent, None).unwrap();
        set_status(&db, parent, "done");

        assert!(guard_dependencies(&db, child).is_ok());
    }

    #[test]
    fn the_refusal_counts_transitive_ancestors_not_only_parents() {
        let db = test_db();
        let a = seed_task(&db, "a");
        let b = seed_task(&db, "b");
        let c = seed_task(&db, "c");
        db::dependencies::add_dependency(&db, b, a, None).unwrap();
        db::dependencies::add_dependency(&db, c, b, None).unwrap();

        let err = guard_dependencies(&db, c).expect_err("should be refused");

        // Two tasks have to run before c, not one. Reporting only the direct parent
        // understates the work and makes the refusal look arbitrary.
        assert!(err.contains('2'), "got: {err}");
    }

    #[test]
    fn a_long_list_of_blockers_is_summarised_rather_than_dumped() {
        let db = test_db();
        let child = seed_task(&db, "child");
        for i in 0..6 {
            let p = seed_task(&db, &format!("parent {i}"));
            db::dependencies::add_dependency(&db, child, p, None).unwrap();
        }

        let err = guard_dependencies(&db, child).expect_err("should be refused");

        assert!(err.contains("more"), "a six-task list needs a tail: {err}");
        assert!(err.len() < 200, "message is a toast, not a report: {err}");
    }

    #[test]
    fn a_standing_start_is_gated_and_a_resume_is_not() {
        // Gated: nothing is in flight and no completed work is being revisited.
        assert!(start_is_gated(TaskStatus::Backlog));
        assert!(start_is_gated(TaskStatus::Failed));

        // Not gated: these continue or revisit a run that already produced work.
        // Gating them would strand a task mid-flight when a parent is reopened
        // while its agent waits for an answer.
        assert!(!start_is_gated(TaskStatus::Blocked));
        assert!(!start_is_gated(TaskStatus::Testing));
        assert!(!start_is_gated(TaskStatus::AwaitingApproval));
        assert!(!start_is_gated(TaskStatus::Done));
    }

    #[test]
    fn an_unmet_dependency_does_not_stop_an_agent_resuming_from_blocked() {
        let db = test_db();
        let parent = seed_task(&db, "parent");
        let child = seed_task(&db, "child");
        db::dependencies::add_dependency(&db, child, parent, None).unwrap();
        set_status(&db, child, "blocked");

        // The guard would refuse this task, so the only thing keeping the answer
        // path working is that Blocked is not a gated origin.
        assert!(guard_dependencies(&db, child).is_err());
        assert!(!start_is_gated(TaskStatus::Blocked));
    }

    /// A stopped run over `members`, targeting the last of them.
    fn stop_a_run(db: &db::DbPool, members: &[i64]) -> i64 {
        let target = *members.last().unwrap();
        let gid = db::task_groups::create(db, 1, "trunk/dep-d", "main", target, members).unwrap();
        db::task_groups::set_status(db, gid, db::task_groups::STATUS_STOPPED).unwrap();
        gid
    }

    fn start(
        db: &db::DbPool,
        id: i64,
        from: TaskStatus,
        over: StartOverride,
    ) -> Result<(), String> {
        guard_start(db, id, from, TaskStatus::InProgress, over)
    }

    #[test]
    fn a_stopped_runs_member_is_refused_a_start_and_told_which_trunk() {
        let db = test_db();
        let a = seed_task(&db, "dep a");
        let d = seed_task(&db, "dep d");
        db::dependencies::add_dependency(&db, d, a, None).unwrap();
        // The prerequisite reports done — its merge into the trunk is what failed — so
        // the dependency guard alone sees nothing wrong with starting d.
        set_status(&db, a, "done");
        stop_a_run(&db, &[a, d]);

        assert!(guard_dependencies(&db, d).is_ok());
        let err = start(&db, d, TaskStatus::Backlog, StartOverride::None).unwrap_err();

        // Naming the trunk is what makes it actionable: that is the branch to merge.
        assert!(err.contains("trunk/dep-d"), "{err}");
    }

    #[test]
    fn the_override_starts_a_stopped_runs_member() {
        let db = test_db();
        let a = seed_task(&db, "dep a");
        let d = seed_task(&db, "dep d");
        db::dependencies::add_dependency(&db, d, a, None).unwrap();
        set_status(&db, a, "done");
        stop_a_run(&db, &[a, d]);

        assert!(start(&db, d, TaskStatus::Backlog, StartOverride::IgnoreStoppedRun).is_ok());
    }

    #[test]
    fn the_override_does_not_reach_the_dependency_gate() {
        let db = test_db();
        let a = seed_task(&db, "dep a");
        let d = seed_task(&db, "dep d");
        db::dependencies::add_dependency(&db, d, a, None).unwrap();
        // a never ran, so d's work does not exist rather than merely sitting on a branch.
        stop_a_run(&db, &[a, d]);

        let err = start(&db, d, TaskStatus::Backlog, StartOverride::IgnoreStoppedRun).unwrap_err();

        // No confirmation can make an unfinished prerequisite exist, so this refusal
        // has to survive the override that clears the other one.
        assert!(err.contains("dep a"), "{err}");
    }

    #[test]
    fn a_stopped_run_does_not_block_resuming_a_member_that_is_already_under_way() {
        let db = test_db();
        let a = seed_task(&db, "dep a");
        let b = seed_task(&db, "dep b");
        stop_a_run(&db, &[a, b]);

        // A sibling still running when the run stopped keeps running, and answering its
        // blocker or requesting a revision passes back through In Progress. Gating that
        // would strand work mid-flight over a run the task is already committed to.
        for from in [
            TaskStatus::Blocked,
            TaskStatus::Testing,
            TaskStatus::AwaitingApproval,
            TaskStatus::Done,
        ] {
            assert!(
                start(&db, b, from, StartOverride::None).is_ok(),
                "refused a resume from {from}"
            );
        }
    }

    #[test]
    fn a_stopped_run_does_not_block_moving_a_member_anywhere_else() {
        let db = test_db();
        let a = seed_task(&db, "dep a");
        let b = seed_task(&db, "dep b");
        stop_a_run(&db, &[a, b]);

        // Only starting is gated. Parking the task, failing it, or approving what did
        // run are all still available while the run waits to be resolved.
        for to in [
            TaskStatus::Backlog,
            TaskStatus::Failed,
            TaskStatus::Done,
            TaskStatus::Testing,
        ] {
            assert!(
                guard_start(&db, b, TaskStatus::Backlog, to, StartOverride::None).is_ok(),
                "refused a move to {to}"
            );
        }
    }

    #[test]
    fn a_resolved_run_stops_refusing_its_members() {
        let db = test_db();
        let a = seed_task(&db, "dep a");
        let d = seed_task(&db, "dep d");
        let gid = stop_a_run(&db, &[a, d]);
        assert!(start(&db, d, TaskStatus::Backlog, StartOverride::None).is_err());

        // Carrying the run on returns it to active; abandoning it finishes the group.
        // Both have to clear the refusal or the tasks are claimed for good.
        db::task_groups::set_status(&db, gid, db::task_groups::STATUS_ACTIVE).unwrap();
        assert!(start(&db, d, TaskStatus::Backlog, StartOverride::None).is_ok());

        db::task_groups::finish(&db, gid, db::task_groups::STATUS_FAILED).unwrap();
        assert!(start(&db, d, TaskStatus::Backlog, StartOverride::None).is_ok());
    }
}
