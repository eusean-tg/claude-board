use crate::db::{self, activity, attachments, projects, settings, stats, tasks};
/// Lightweight HTTP API for MCP server communication.
/// The MCP server (Node.js sidecar) talks to this API to manage tasks.
use axum::{
    extract::{Path, Query},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, patch, post},
    Json, Router,
};
use serde::Deserialize;
use tower_http::cors::CorsLayer;

/// Helper: serialize to JSON Value, fallback to empty object on error.
fn to_json<T: serde::Serialize>(val: &T) -> serde_json::Value {
    serde_json::to_value(val).unwrap_or_default()
}

pub async fn start_server(port: u16) {
    let listener = match tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port)).await {
        Ok(l) => l,
        Err(e) => {
            log::error!("Failed to bind MCP HTTP API on port {}: {}", port, e);
            return;
        }
    };
    log::info!("MCP HTTP API listening on port {}", port);
    axum::serve(listener, router()).await.ok();
}

/// The routing table, separate from binding it, so a test can serve it on an
/// ephemeral port and exercise a real request rather than calling handlers directly.
pub(crate) fn router() -> Router {
    Router::new()
        // Projects
        .route("/api/projects", get(list_projects))
        .route("/api/projects/summary", get(projects_summary))
        .route("/api/projects/{id}", get(get_project))
        // Tasks
        .route(
            "/api/projects/{project_id}/tasks",
            get(list_tasks).post(create_task),
        )
        .route(
            "/api/tasks/{id}",
            get(get_task).put(update_task).delete(delete_task_handler),
        )
        .route("/api/tasks/{id}/status", patch(change_status))
        .route("/api/tasks/{id}/detail", get(task_detail))
        .route("/api/tasks/{id}/logs", get(task_logs))
        .route("/api/tasks/{id}/revisions", get(task_revisions))
        // Blockers
        .route("/api/tasks/{id}/blockers", post(raise_blocker))
        .route("/api/tasks/{id}/blockers/open", get(open_blocker))
        // Stats
        .route(
            "/api/projects/{pid}/artifacts",
            get(list_artifacts).post(save_artifact),
        )
        .route("/api/artifacts/{id}", patch(update_artifact))
        .route("/api/projects/{pid}/stats", get(project_stats))
        .route("/api/stats/claude-usage", get(claude_usage))
        .route("/api/projects/{pid}/activity", get(project_activity))
        // Auth
        .route("/api/auth/status", get(auth_status))
        // Settings
        .route("/api/settings", get(get_settings).put(update_settings))
        .layer(CorsLayer::permissive())
}

// ─── Handlers ───

async fn list_projects() -> Json<serde_json::Value> {
    Json(to_json(&projects::get_all(&db::get_db())))
}

async fn projects_summary() -> Json<serde_json::Value> {
    Json(to_json(&projects::get_summary(&db::get_db())))
}

async fn get_project(Path(id): Path<i64>) -> impl IntoResponse {
    match projects::get_by_id(&db::get_db(), id) {
        Some(p) => Json(to_json(&p)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn list_tasks(Path(project_id): Path<i64>) -> Json<serde_json::Value> {
    Json(to_json(&tasks::get_by_project(&db::get_db(), project_id)))
}

#[derive(Deserialize)]
struct CreateTaskBody {
    title: String,
    description: Option<String>,
    priority: Option<i64>,
    task_type: Option<String>,
    acceptance_criteria: Option<String>,
    model: Option<String>,
    thinking_effort: Option<String>,
    tags: Option<String>,
    parent_task_id: Option<i64>,
}

async fn create_task(
    Path(project_id): Path<i64>,
    Json(body): Json<CreateTaskBody>,
) -> impl IntoResponse {
    let db = db::get_db();
    let id = tasks::create(
        &db,
        project_id,
        &body.title,
        body.description.as_deref().unwrap_or(""),
        body.priority.unwrap_or(0),
        body.task_type.as_deref().unwrap_or("feature"),
        body.acceptance_criteria.as_deref().unwrap_or(""),
        body.model.as_deref().unwrap_or("sonnet"),
        body.thinking_effort.as_deref().unwrap_or("medium"),
        None,
        body.tags.as_deref(),
    );
    // Link as sub-task if parent_task_id provided
    if let Some(parent_id) = body.parent_task_id {
        if tasks::get_by_id(&db, parent_id).is_some() {
            tasks::set_parent_task_id(&db, id, parent_id);
            tasks::set_awaiting_subtasks(&db, parent_id, true);
        }
    }
    match tasks::get_by_id(&db, id) {
        Some(task) => (StatusCode::CREATED, Json(to_json(&task))).into_response(),
        None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn get_task(Path(id): Path<i64>) -> impl IntoResponse {
    match tasks::get_by_id(&db::get_db(), id) {
        Some(t) => Json(to_json(&t)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Deserialize)]
struct UpdateTaskBody {
    title: Option<String>,
    description: Option<String>,
    priority: Option<i64>,
    task_type: Option<String>,
    acceptance_criteria: Option<String>,
    model: Option<String>,
    thinking_effort: Option<String>,
    tags: Option<String>,
}

async fn update_task(Path(id): Path<i64>, Json(body): Json<UpdateTaskBody>) -> impl IntoResponse {
    let db = db::get_db();
    let task = match tasks::get_by_id(&db, id) {
        Some(t) => t,
        None => return StatusCode::NOT_FOUND.into_response(),
    };
    tasks::update(
        &db,
        id,
        body.title.as_deref().unwrap_or(&task.title),
        body.description
            .as_deref()
            .unwrap_or(task.description.as_deref().unwrap_or("")),
        body.priority.unwrap_or(task.priority.unwrap_or(0)),
        body.task_type
            .as_deref()
            .unwrap_or(task.task_type.as_deref().unwrap_or("feature")),
        body.acceptance_criteria
            .as_deref()
            .unwrap_or(task.acceptance_criteria.as_deref().unwrap_or("")),
        body.model
            .as_deref()
            .unwrap_or(task.model.as_deref().unwrap_or("sonnet")),
        body.thinking_effort
            .as_deref()
            .unwrap_or(task.thinking_effort.as_deref().unwrap_or("medium")),
        task.role_id,
        body.tags.as_deref().or(task.tags.as_deref()),
    );
    match tasks::get_by_id(&db, id) {
        Some(updated) => Json(to_json(&updated)).into_response(),
        None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn delete_task_handler(Path(id): Path<i64>) -> impl IntoResponse {
    tasks::delete(&db::get_db(), id);
    Json(serde_json::json!({"ok": true}))
}

#[derive(Deserialize)]
struct StatusBody {
    status: String,
}

async fn change_status(Path(id): Path<i64>, Json(body): Json<StatusBody>) -> impl IntoResponse {
    let db = db::get_db();
    tasks::update_status(&db, id, &body.status);
    // Keep GSD roadmap (DB + ROADMAP.md) in sync when task status is changed
    // via the MCP HTTP bridge. No AppHandle here → UI refresh is skipped but
    // the file/DB state stays consistent.
    crate::services::gsd::apply_task_status_cascade(&db, None, id);
    match tasks::get_by_id(&db, id) {
        Some(t) => Json(to_json(&t)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn task_detail(Path(id): Path<i64>) -> impl IntoResponse {
    let db = db::get_db();
    let task = match tasks::get_by_id(&db, id) {
        Some(t) => t,
        None => return StatusCode::NOT_FOUND.into_response(),
    };
    let revisions = tasks::get_revisions(&db, id);
    let atts = attachments::get_by_task(&db, id);
    let commits: serde_json::Value = task
        .commits
        .as_deref()
        .and_then(|c| serde_json::from_str(c).ok())
        .unwrap_or(serde_json::json!([]));

    let mut val = to_json(&task);
    if let Some(obj) = val.as_object_mut() {
        obj.insert("commits".into(), commits);
        obj.insert("revisions".into(), to_json(&revisions));
        obj.insert("attachments".into(), to_json(&atts));
    }
    Json(val).into_response()
}

#[derive(Deserialize)]
struct LogsQuery {
    limit: Option<i64>,
}

async fn task_logs(Path(id): Path<i64>, Query(q): Query<LogsQuery>) -> Json<serde_json::Value> {
    let mut logs = tasks::get_recent_logs(&db::get_db(), id, q.limit.unwrap_or(500));
    logs.reverse();
    Json(to_json(&logs))
}

async fn task_revisions(Path(id): Path<i64>) -> Json<serde_json::Value> {
    Json(to_json(&tasks::get_revisions(&db::get_db(), id)))
}

async fn project_stats(Path(pid): Path<i64>) -> Json<serde_json::Value> {
    Json(to_json(&stats::get_project_stats(&db::get_db(), pid)))
}

async fn claude_usage() -> Json<serde_json::Value> {
    let db = db::get_db();
    Json(serde_json::json!({
        "usage": stats::get_global_usage(&db),
        "models": stats::get_global_model_breakdown(&db),
        "timeline": stats::get_usage_timeline(&db),
        "limits": stats::get_claude_limits(&db),
    }))
}

#[derive(Deserialize)]
struct ActivityQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn project_activity(
    Path(pid): Path<i64>,
    Query(q): Query<ActivityQuery>,
) -> Json<serde_json::Value> {
    Json(to_json(&activity::get_by_project(
        &db::get_db(),
        pid,
        q.limit.unwrap_or(50),
        q.offset.unwrap_or(0),
    )))
}

async fn auth_status() -> Json<serde_json::Value> {
    Json(serde_json::json!({"enabled": crate::db::auth::is_auth_enabled(&db::get_db())}))
}

async fn get_settings() -> Json<serde_json::Value> {
    Json(to_json(&settings::get(&db::get_db())))
}

async fn update_settings(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let db = db::get_db();
    let mut current = settings::get(&db);
    if let Some(v) = body.get("confirm_before_delete").and_then(|v| v.as_bool()) {
        current.confirm_before_delete = v;
    }
    if let Some(v) = body.get("default_model").and_then(|v| v.as_str()) {
        current.default_model = v.to_string();
    }
    if let Some(v) = body.get("default_effort").and_then(|v| v.as_str()) {
        current.default_effort = v.to_string();
    }
    if let Some(v) = body.get("language").and_then(|v| v.as_str()) {
        current.language = v.to_string();
    }
    if let Some(v) = body.get("auto_open_terminal").and_then(|v| v.as_bool()) {
        current.auto_open_terminal = v;
    }
    if let Some(v) = body.get("sound_enabled").and_then(|v| v.as_bool()) {
        current.sound_enabled = v;
    }
    settings::update(&db, &current);
    Json(to_json(&current))
}

// ─── Artifacts ──────────────────────────────────────────────────────────────

fn artifact_data_dir() -> String {
    db::get_data_dir().to_string_lossy().to_string()
}

async fn list_artifacts(Path(project_id): Path<i64>) -> impl IntoResponse {
    Json(to_json(&db::artifacts::list_for_project(
        &db::get_db(),
        project_id,
    )))
}

#[derive(serde::Deserialize)]
struct SaveArtifactBody {
    title: String,
    kind: Option<String>,
    content: String,
    tags: Option<Vec<String>>,
    task_id: Option<i64>,
}

async fn save_artifact(
    Path(project_id): Path<i64>,
    Json(body): Json<SaveArtifactBody>,
) -> impl IntoResponse {
    let tags = body.tags.unwrap_or_default();
    match super::artifact_store::save(
        &db::get_db(),
        &artifact_data_dir(),
        project_id,
        &body.title,
        body.kind.as_deref().unwrap_or("other"),
        &body.content,
        &tags,
        body.task_id,
    ) {
        Ok(saved) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "id": saved.id,
                "storedName": saved.stored_name,
                "path": saved.path,
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e})),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct UpdateArtifactBody {
    title: Option<String>,
    kind: Option<String>,
    content: Option<String>,
    tags: Option<Vec<String>>,
    task_id: Option<i64>,
}

async fn update_artifact(
    Path(id): Path<i64>,
    Json(body): Json<UpdateArtifactBody>,
) -> impl IntoResponse {
    match super::artifact_store::revise(
        &db::get_db(),
        &artifact_data_dir(),
        id,
        body.title.as_deref(),
        body.kind.as_deref(),
        body.content.as_deref(),
        body.tags.as_deref(),
        body.task_id,
    ) {
        Ok(saved) => Json(serde_json::json!({
            "id": saved.id,
            "storedName": saved.stored_name,
            "path": saved.path,
        }))
        .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e})),
        )
            .into_response(),
    }
}

// ─── Blockers ───────────────────────────────────────────────────────────────

use crate::claude::state_machine::{is_valid_transition, TaskStatus};
use crate::db::blockers::{self, BlockerKind, NewBlocker};
use crate::db::DbPool;
use std::time::Duration;

/// How long a raise waits for an answer when the agent does not say.
///
/// Long enough that someone at their desk answers in the same session, short
/// enough that an agent asking into an empty room stops and frees its slot
/// instead of holding a run open all afternoon.
const DEFAULT_WAIT_SECONDS: u64 = 300;

/// Ceiling on the requested wait, whatever the agent asks for.
const MAX_WAIT_SECONDS: u64 = 1800;

#[derive(serde::Deserialize)]
struct BlockerOptionBody {
    label: String,
    description: Option<String>,
}

#[derive(serde::Deserialize)]
struct RaiseBlockerBody {
    kind: String,
    question: String,
    header: Option<String>,
    context: Option<String>,
    #[serde(rename = "artifactId")]
    artifact_id: Option<i64>,
    options: Option<Vec<BlockerOptionBody>>,
    #[serde(rename = "waitSeconds")]
    wait_seconds: Option<u64>,
}

/// Validate the request, record the question, and park the task on it.
///
/// Split from the handler so it can be tested against a database of its own: the
/// handler reads the process-global pool, which a test cannot replace.
///
/// Everything an agent sends is checked here. The CHECK constraints and the
/// state machine are the last line of defence, not the first — an agent can send
/// any string it likes.
fn raise_blocker_in(db: &DbPool, task_id: i64, body: &RaiseBlockerBody) -> Result<i64, String> {
    let kind = BlockerKind::from_str(body.kind.trim())
        .ok_or_else(|| format!("unknown blocker kind: {}", body.kind))?;

    let task = crate::db::tasks::get_by_id(db, task_id).ok_or("task not found")?;
    let from = TaskStatus::from_str(task.status.as_deref().unwrap_or("backlog"))
        .unwrap_or(TaskStatus::Backlog);
    // The user may have stopped, failed or re-queued the task while the agent was
    // working. Raising a question against it would park a task nobody is running.
    if !is_valid_transition(from, TaskStatus::Blocked) {
        return Err(format!(
            "a task that is {} cannot be blocked; stop and leave the work in place",
            from
        ));
    }

    let options: Vec<(String, String)> = body
        .options
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|o| (o.label.clone(), o.description.clone().unwrap_or_default()))
        .collect();

    // The question first. If it is refused — the task already has one open — the
    // task must not be moved to blocked on the strength of a raise that failed.
    let blocker_id = blockers::create(
        db,
        &NewBlocker {
            task_id,
            kind,
            header: body.header.as_deref().unwrap_or(""),
            question: &body.question,
            context: body.context.as_deref().unwrap_or(""),
            artifact_id: body.artifact_id,
            options: &options,
        },
    )
    .map_err(|e| e.to_string())?;

    crate::db::tasks::update_status(db, task_id, TaskStatus::Blocked.as_str());
    // Waiting on a person is not work. Left running it inflates work_duration_ms
    // and can trip task_timeout_minutes into killing a task that is only waiting.
    crate::db::tasks::pause_timer(db, task_id);

    Ok(blocker_id)
}

/// How long to wait, clamped. A zero is honoured: it means "record it and tell me
/// to stop", which is what the deadline path does anyway.
fn wait_duration(requested: Option<u64>) -> Duration {
    Duration::from_secs(
        requested
            .unwrap_or(DEFAULT_WAIT_SECONDS)
            .min(MAX_WAIT_SECONDS),
    )
}

/// The body returned to the agent, answered or not.
fn raise_result_json(
    blocker_id: i64,
    answer: Option<&super::blockers::AnswerPayload>,
) -> serde_json::Value {
    match answer {
        Some(payload) => serde_json::json!({
            "blockerId": blocker_id,
            "answered": true,
            "summary": payload.summary,
            "responses": payload.responses.iter().map(|r| serde_json::json!({
                "optionId": r.option_id,
                "note": r.note,
                "freeText": r.free_text,
            })).collect::<Vec<_>>(),
        }),
        // 200 rather than an error status: a deadline is a normal outcome the
        // agent should read and act on, not a transport failure to retry.
        None => serde_json::json!({ "blockerId": blocker_id, "answered": false }),
    }
}

async fn raise_blocker(
    Path(task_id): Path<i64>,
    Json(body): Json<RaiseBlockerBody>,
) -> impl IntoResponse {
    let db = db::get_db();
    let blocker_id = match raise_blocker_in(&db, task_id, &body) {
        Ok(id) => id,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": e})),
            )
                .into_response()
        }
    };

    // Tell the board, and tell the user. The board is event-driven and does not
    // poll, so without the emit a blocked task sits there unnoticed.
    if let Some(app) = crate::app_handle() {
        use tauri::Emitter;
        if let Some(task) = crate::db::tasks::get_by_id(&db, task_id) {
            app.emit("task:updated", &task).ok();
            super::notification::notify_blocker_raised(
                &app,
                &super::notification::TaskNotification::new(&task.title, task.task_key.as_deref()),
                &body.question,
            );
        }
        app.emit(
            "blocker:raised",
            &serde_json::json!({
                "taskId": task_id,
                "blockerId": blocker_id,
            }),
        )
        .ok();
    }

    let answer =
        super::blockers::wait_for_answer(&db, blocker_id, wait_duration(body.wait_seconds)).await;
    Json(raise_result_json(blocker_id, answer.as_ref())).into_response()
}

/// The task's open question, for a client that wants to poll rather than wait.
async fn open_blocker(Path(task_id): Path<i64>) -> impl IntoResponse {
    match blockers::open_for_task(&db::get_db(), task_id) {
        Some(b) => Json(to_json(&b)).into_response(),
        None => Json(serde_json::Value::Null).into_response(),
    }
}

#[cfg(test)]
mod blocker_tests {
    use super::*;
    use parking_lot::Mutex;
    use rusqlite::{params, Connection};
    use std::sync::Arc;

    /// Mirrors the real startup path in `db::init_db`.
    ///
    /// `run_migrations` is not optional here: `tasks.deleted_at` is added by a
    /// migration rather than by `create_tables`, and `tasks::get_by_id` filters on
    /// it — so without migrations every lookup fails on a missing column and reads
    /// as "task not found".
    fn test_db() -> DbPool {
        let conn = Connection::open_in_memory().expect("in-memory db");
        crate::db::schema::create_tables(&conn);
        crate::db::schema::run_migrations(&conn);
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        Arc::new(Mutex::new(conn))
    }

    /// A task that is running, with its work timer ticking.
    fn seed_running_task(db: &DbPool, id: i64) -> i64 {
        let conn = db.lock();
        conn.execute(
            "INSERT OR IGNORE INTO projects (id,name,slug,working_dir)
             VALUES (1,'B','b','/repo')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id,project_id,title,status,started_at,last_resumed_at)
             VALUES (?1,1,'t','in_progress',datetime('now','localtime'),
                     datetime('now','localtime'))",
            params![id],
        )
        .unwrap();
        id
    }

    fn body(kind: &str, question: &str) -> RaiseBlockerBody {
        RaiseBlockerBody {
            kind: kind.into(),
            question: question.into(),
            header: None,
            context: None,
            artifact_id: None,
            options: None,
            wait_seconds: None,
        }
    }

    fn with_options(mut b: RaiseBlockerBody, labels: &[&str]) -> RaiseBlockerBody {
        b.options = Some(
            labels
                .iter()
                .map(|l| BlockerOptionBody {
                    label: (*l).into(),
                    description: None,
                })
                .collect(),
        );
        b
    }

    fn status_of(db: &DbPool, id: i64) -> String {
        db.lock()
            .query_row("SELECT status FROM tasks WHERE id=?1", params![id], |r| {
                r.get(0)
            })
            .unwrap()
    }

    fn timer_paused(db: &DbPool, id: i64) -> bool {
        db.lock()
            .query_row(
                "SELECT last_resumed_at IS NULL FROM tasks WHERE id=?1",
                params![id],
                |r| r.get::<_, bool>(0),
            )
            .unwrap()
    }

    #[test]
    fn raising_a_blocker_moves_the_task_to_blocked_and_pauses_its_timer() {
        let db = test_db();
        let t = seed_running_task(&db, 601);

        let id = raise_blocker_in(
            &db,
            t,
            &with_options(
                body("single_choice", "Which auth flow?"),
                &["PKCE", "Implicit"],
            ),
        )
        .expect("should be accepted");

        assert_eq!(status_of(&db, t), "blocked");
        // Waiting on a human is not work; counting it can trip task_timeout_minutes.
        assert!(timer_paused(&db, t));
        assert_eq!(blockers::options(&db, id).len(), 2);
    }

    #[test]
    fn a_blocker_with_an_unknown_kind_is_rejected() {
        let db = test_db();
        let t = seed_running_task(&db, 602);

        // The agent can send anything; the CHECK constraint must not be the first
        // line of defence.
        assert!(raise_blocker_in(&db, t, &body("whatever", "?")).is_err());
        assert_eq!(status_of(&db, t), "in_progress");
    }

    #[test]
    fn a_choice_blocker_with_no_options_is_rejected() {
        let db = test_db();
        let t = seed_running_task(&db, 603);

        // A select with nothing to select is unanswerable.
        assert!(raise_blocker_in(&db, t, &body("single_choice", "Which?")).is_err());
        // And the task must not be left blocked on a question that was refused.
        assert_eq!(status_of(&db, t), "in_progress");
        assert!(!timer_paused(&db, t));
    }

    #[test]
    fn a_blocker_against_a_task_nobody_is_running_is_rejected() {
        let db = test_db();
        let t = seed_running_task(&db, 604);
        crate::db::tasks::update_status(&db, t, "failed");

        // The user stopped the task while the agent was working. Parking it on a
        // question would strand a task no one is running.
        let err = raise_blocker_in(&db, t, &body("free_text", "still there?")).unwrap_err();

        assert!(err.contains("cannot be blocked"), "got: {err}");
        assert_eq!(status_of(&db, t), "failed");
    }

    #[test]
    fn a_blocker_for_a_missing_task_is_rejected() {
        let db = test_db();

        assert!(raise_blocker_in(&db, 9_999, &body("free_text", "?")).is_err());
    }

    #[test]
    fn a_second_blocker_on_one_task_is_refused_without_disturbing_the_first() {
        let db = test_db();
        let t = seed_running_task(&db, 605);
        let first = raise_blocker_in(&db, t, &body("free_text", "first?")).unwrap();

        // Already blocked, so the transition is refused before storage even sees it.
        assert!(raise_blocker_in(&db, t, &body("free_text", "second?")).is_err());

        assert_eq!(blockers::open_for_task(&db, t).map(|b| b.id), Some(first));
    }

    #[test]
    fn an_empty_question_is_rejected() {
        let db = test_db();
        let t = seed_running_task(&db, 606);

        assert!(raise_blocker_in(&db, t, &body("free_text", "   ")).is_err());
        assert_eq!(status_of(&db, t), "in_progress");
    }

    #[test]
    fn the_wait_is_clamped_to_something_sane() {
        // An agent asking to wait for a week would hold a run open for a week.
        assert_eq!(
            wait_duration(Some(u64::MAX)),
            Duration::from_secs(MAX_WAIT_SECONDS)
        );
        assert_eq!(
            wait_duration(None),
            Duration::from_secs(DEFAULT_WAIT_SECONDS)
        );
        // Zero is honoured: record the question and tell me to stop.
        assert_eq!(wait_duration(Some(0)), Duration::ZERO);
    }

    #[tokio::test]
    async fn a_raise_that_nobody_answers_reports_the_deadline_rather_than_an_error() {
        let db = test_db();
        let t = seed_running_task(&db, 607);
        let id = raise_blocker_in(&db, t, &body("free_text", "anyone?")).unwrap();

        let answer = super::super::blockers::wait_for_answer(&db, id, Duration::ZERO).await;
        let out = raise_result_json(id, answer.as_ref());

        assert_eq!(out["answered"], serde_json::json!(false));
        assert_eq!(out["blockerId"], serde_json::json!(id));
        // The task stays blocked, to be resumed with the answer later.
        assert_eq!(status_of(&db, t), "blocked");
    }

    #[tokio::test]
    async fn an_answered_raise_returns_the_summary_and_the_responses() {
        let db = test_db();
        let t = seed_running_task(&db, 608);
        let id = raise_blocker_in(
            &db,
            t,
            &with_options(body("single_choice", "Which?"), &["PKCE"]),
        )
        .unwrap();
        let option_id = blockers::options(&db, id)[0].id;
        blockers::answer(
            &db,
            id,
            &[crate::db::blockers::BlockerResponse::option(
                option_id,
                Some("web only"),
            )],
        )
        .unwrap();

        let answer = super::super::blockers::wait_for_answer(&db, id, Duration::from_secs(5)).await;
        let out = raise_result_json(id, answer.as_ref());

        assert_eq!(out["answered"], serde_json::json!(true));
        assert_eq!(out["summary"], serde_json::json!("PKCE (web only)"));
        assert_eq!(
            out["responses"][0]["optionId"],
            serde_json::json!(option_id)
        );
        assert_eq!(out["responses"][0]["note"], serde_json::json!("web only"));
    }

    #[test]
    fn the_body_accepts_the_field_names_the_mcp_tool_sends() {
        // camelCase over the wire, snake_case in Rust. A rename that drifts makes
        // the field silently arrive as None.
        let parsed: RaiseBlockerBody = serde_json::from_value(serde_json::json!({
            "kind": "multi_choice",
            "question": "Which paths?",
            "header": "Scope",
            "context": "already read the router",
            "artifactId": 7,
            "options": [{"label": "Read", "description": "cached"}, {"label": "Write"}],
            "waitSeconds": 42
        }))
        .expect("the MCP tool's body must deserialise");

        assert_eq!(parsed.artifact_id, Some(7));
        assert_eq!(parsed.wait_seconds, Some(42));
        assert_eq!(parsed.options.as_ref().unwrap().len(), 2);
        assert_eq!(parsed.options.as_ref().unwrap()[1].description, None);
    }
}

/// One test, over real HTTP, through the real router.
///
/// Kept apart from `blocker_tests` because it initialises the process-global
/// database pool that the handlers read. Nothing else in the suite touches that
/// global, and it can only be set once per process, so exactly one test may do it.
#[cfg(test)]
mod blocker_http_tests {
    use super::*;
    use crate::db::blockers::{self, BlockerResponse};
    use std::time::Duration;

    #[tokio::test]
    async fn an_agent_raises_a_question_over_http_and_gets_the_answer_back() {
        let dir = std::env::temp_dir().join(format!("cb-blocker-http-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = match crate::db::init_db(&dir.to_string_lossy()) {
            Ok(pool) => pool,
            Err(e) => panic!("init_db: {e}"),
        };

        let task_id = 701;
        {
            let conn = db.lock();
            conn.execute(
                "INSERT OR IGNORE INTO projects (id,name,slug,working_dir)
                 VALUES (1,'B','b','/repo')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT OR REPLACE INTO tasks (id,project_id,title,status,last_resumed_at)
                 VALUES (?1,1,'t','in_progress',datetime('now','localtime'))",
                rusqlite::params![task_id],
            )
            .unwrap();
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, router()).await.ok();
        });

        let client = reqwest::Client::new();
        let url = format!("{}/api/tasks/{}/blockers", base, task_id);

        // ── The raise, with a wait long enough to be answered in flight ──
        let raising = {
            let client = client.clone();
            let url = url.clone();
            tokio::spawn(async move {
                client
                    .post(&url)
                    .json(&serde_json::json!({
                        "kind": "single_choice",
                        "header": "Auth flow",
                        "question": "Which auth flow?",
                        "options": [{"label": "PKCE", "description": "recommended"},
                                    {"label": "Implicit"}],
                        "waitSeconds": 30
                    }))
                    .send()
                    .await
                    .unwrap()
                    .json::<serde_json::Value>()
                    .await
                    .unwrap()
            })
        };

        // Wait for the request to have landed and parked.
        let mut open = None;
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            open = blockers::open_for_task(&db, task_id);
            if open.is_some() {
                break;
            }
        }
        let open = open.expect("the raise should have recorded a question");
        assert_eq!(open.header, "Auth flow");
        assert_eq!(open.options.len(), 2);
        // The board sees it as blocked, with its timer stopped, while the agent waits.
        let (status, resumed): (String, Option<String>) = {
            let conn = db.lock();
            conn.query_row(
                "SELECT status, last_resumed_at FROM tasks WHERE id=?1",
                rusqlite::params![task_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        };
        assert_eq!(status, "blocked");
        assert_eq!(resumed, None, "the work timer must be paused while waiting");

        // ── The user answers ──
        let option_id = open.options[0].id;
        blockers::answer(
            &db,
            open.id,
            &[BlockerResponse::option(option_id, Some("web only"))],
        )
        .unwrap();
        super::super::blockers::notify_settled(open.id);

        // ── The still-open request returns with the answer ──
        let body = tokio::time::timeout(Duration::from_secs(10), raising)
            .await
            .expect("the raise should return once answered")
            .unwrap();
        assert_eq!(body["answered"], serde_json::json!(true));
        assert_eq!(body["summary"], serde_json::json!("PKCE (web only)"));
        assert_eq!(body["responses"][0]["note"], serde_json::json!("web only"));

        // ── Answering frees the question, but not yet the task ──
        // Storage will accept another question now, and the transition check is
        // what refuses it: the task is still `blocked` until something moves it
        // back to `in_progress`. That is Task 5's answer_blocker command; here the
        // test stands in for it.
        let while_still_blocked = client
            .post(&url)
            .json(&serde_json::json!({"kind": "free_text", "question": "again?"}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            while_still_blocked.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "a blocked task must not be able to raise a second question"
        );
        crate::db::tasks::update_status(&db, task_id, "in_progress");

        // ── A deadline is a 200 with answered:false, not a transport error ──
        let second: serde_json::Value = client
            .post(&url)
            .json(&serde_json::json!({
                "kind": "free_text", "question": "anyone there?", "waitSeconds": 0
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(second["answered"], serde_json::json!(false));
        assert!(second["blockerId"].as_i64().unwrap() > 0);

        // ── A bad kind is rejected before anything is written ──
        let bad = client
            .post(&url)
            .json(&serde_json::json!({"kind": "whatever", "question": "?"}))
            .send()
            .await
            .unwrap();
        assert_eq!(bad.status(), reqwest::StatusCode::BAD_REQUEST);

        std::fs::remove_dir_all(&dir).ok();
    }
}
