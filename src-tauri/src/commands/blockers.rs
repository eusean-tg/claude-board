//! Commands backing the blocker panel: answer a question, cancel it, read it.
//!
//! The distinction that drives everything here is whether the agent is still
//! waiting. Answer a question inside the wait and the agent picks the answer up
//! in the session it already has, so the task goes straight back to `in_progress`
//! and its timer restarts. Answer it after the wait expired and the agent is
//! gone, so the answer alone reaches nobody: the task is restarted with it,
//! reusing the worktree the agent left behind.
//!
//! A task whose restart fails goes back to `blocked` rather than being left in
//! `in_progress`, because `recover_orphaned_tasks` resets an `in_progress` task
//! with no runner to backlog on the next launch, taking the answer with it.

use crate::claude::state_machine::TaskStatus;
use crate::db::blockers::{Blocker, BlockerResponse};
use crate::db::{self, tasks as tq, DbPool};
use crate::services::blockers as wait;

/// What answering did, so the board knows whether the agent carried on.
#[derive(serde::Serialize, Debug, PartialEq)]
pub struct AnswerOutcome {
    /// True when the agent was still waiting and resumed in its own session.
    pub resumed_in_session: bool,
    /// True when the agent had stopped and restarting it did not work either, so
    /// the task is still blocked and the user has to start it themselves.
    pub needs_restart: bool,
    /// The answer as one line, the same text the agent is given.
    pub summary: String,
}

/// One response as it arrives from the panel.
#[derive(serde::Deserialize, Debug, Default)]
pub struct ResponseInput {
    #[serde(rename = "optionId")]
    pub option_id: Option<i64>,
    pub note: Option<String>,
    #[serde(rename = "freeText")]
    pub free_text: Option<String>,
}

impl From<&ResponseInput> for BlockerResponse {
    fn from(r: &ResponseInput) -> Self {
        BlockerResponse {
            option_id: r.option_id,
            // A note with nothing in it is noise in the summary the agent reads.
            note: r
                .note
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            free_text: r
                .free_text
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        }
    }
}

/// The open question on a task, with its options, or `None`.
#[tauri::command]
pub fn get_blocker(task_id: i64) -> Option<Blocker> {
    db::blockers::open_for_task(&db::get_db(), task_id)
}

/// Every question ever raised on a task, newest first, for the history view.
#[tauri::command]
pub fn task_blockers(task_id: i64) -> Vec<Blocker> {
    db::blockers::for_task(&db::get_db(), task_id)
}

#[tauri::command]
pub fn answer_blocker(
    app: tauri::AppHandle,
    blocker_id: i64,
    responses: Vec<ResponseInput>,
    mcp_port: u16,
) -> Result<AnswerOutcome, String> {
    let db = db::get_db();
    let mut outcome = answer_blocker_in(&db, blocker_id, &responses)?;
    let task_id = db::blockers::get(&db, blocker_id).map(|b| b.task_id);

    // The agent gave up before the answer arrived, so nothing is listening. Start
    // it again with the answer, in the worktree it left behind.
    if outcome.needs_restart {
        if let Some(task_id) = task_id {
            match restart_with_answer(&app, &db, task_id, &outcome.summary, mcp_port) {
                Ok(()) => outcome.needs_restart = false,
                Err(e) => {
                    log::error!("resuming task {} with its answer: {}", task_id, e);
                    tq::add_log(
                        &db,
                        task_id,
                        &format!("Could not resume with the answer: {}", e),
                        "error",
                        None,
                    );
                }
            }
        }
    }

    if let Some(task_id) = task_id {
        emit_task(&app, &db, task_id);
    }
    Ok(outcome)
}

/// Put a blocked task back to work with the answer it was waiting for.
///
/// The status moves first and is rolled back if the runner refuses, matching
/// `change_task_status`: an `in_progress` task with nothing running is reset to
/// backlog on the next launch.
fn restart_with_answer(
    app: &tauri::AppHandle,
    db: &DbPool,
    task_id: i64,
    answer: &str,
    mcp_port: u16,
) -> Result<(), String> {
    let task = tq::get_by_id(db, task_id).ok_or("task not found")?;
    let project = crate::db::projects::get_by_id(db, task.project_id).ok_or("project not found")?;

    tq::update_status(db, task_id, TaskStatus::InProgress.as_str());
    tq::set_resumed(db, task_id);
    let updated = tq::get_by_id(db, task_id).ok_or("task not found after the status change")?;

    if crate::claude::runner::resume_with_answer(
        &updated,
        app.clone(),
        &project.working_dir,
        &project,
        mcp_port,
        answer,
    ) {
        Ok(())
    } else {
        // Already running, or the spawn was refused. Back to blocked, which is the
        // state a stopped task can sit in safely.
        tq::update_status(db, task_id, TaskStatus::Blocked.as_str());
        tq::pause_timer(db, task_id);
        Err("the runner refused to start".to_string())
    }
}

/// Answer a question and let the agent know.
///
/// Split from the command so it can be tested against its own database; the
/// command reads the process-global pool.
pub(crate) fn answer_blocker_in(
    db: &DbPool,
    blocker_id: i64,
    responses: &[ResponseInput],
) -> Result<AnswerOutcome, String> {
    let blocker = db::blockers::get(db, blocker_id).ok_or("blocker not found")?;
    let converted: Vec<BlockerResponse> = responses.iter().map(BlockerResponse::from).collect();

    // Read this before answering, not after. Answering wakes the waiter, which
    // then unregisters itself; on the multi-threaded runtime the app actually
    // uses, that can land on another worker before the check below, making an
    // answer the agent is waiting for look like one it gave up on — and leaving
    // the task blocked with a live agent that just resumed.
    let agent_waiting = wait::is_waiting_on(blocker_id);

    db::blockers::answer(db, blocker_id, &converted).map_err(|e| e.to_string())?;
    wait::notify_settled(blocker_id);

    if agent_waiting {
        // The agent is mid-call and will carry on in the session it has. Put the
        // task back where it was, or the agent cannot raise a second question:
        // Blocked → Blocked is not a legal transition.
        tq::update_status(db, blocker.task_id, TaskStatus::InProgress.as_str());
        tq::set_resumed(db, blocker.task_id);
    }

    Ok(AnswerOutcome {
        resumed_in_session: agent_waiting,
        // Left blocked deliberately. Moving it to in_progress with nothing running
        // would have recover_orphaned_tasks reset it to backlog on the next launch.
        needs_restart: !agent_waiting,
        summary: db::blockers::answer_summary(db, blocker_id),
    })
}

/// Close a question without answering it.
///
/// The task stays blocked: this says "stop waiting", not "carry on". To start the
/// task moving again the user changes its status, which is the way out that keeps
/// a blocker from wedging a task permanently.
#[tauri::command]
pub fn cancel_blocker(app: tauri::AppHandle, blocker_id: i64) -> Result<(), String> {
    let db = db::get_db();
    cancel_blocker_in(&db, blocker_id)?;
    if let Some(task_id) = db::blockers::get(&db, blocker_id).map(|b| b.task_id) {
        emit_task(&app, &db, task_id);
    }
    Ok(())
}

pub(crate) fn cancel_blocker_in(db: &DbPool, blocker_id: i64) -> Result<(), String> {
    db::blockers::cancel(db, blocker_id).map_err(|e| e.to_string())?;
    // Wake the agent so it stops cleanly now rather than sitting out its deadline.
    wait::notify_settled(blocker_id);
    Ok(())
}

/// Close whatever question a task has open, because the task is moving on.
///
/// Called when the user drags a blocked task somewhere else. Without it the task
/// leaves `blocked` still carrying an open question, which blocks the next raise
/// and leaves the panel rendering a question nobody is waiting on.
pub(crate) fn cancel_open_blocker_for_task(db: &DbPool, task_id: i64) {
    if let Some(open) = db::blockers::open_for_task(db, task_id) {
        if let Err(e) = cancel_blocker_in(db, open.id) {
            log::error!("cancelling the open blocker on task {}: {}", task_id, e);
        }
    }
}

fn emit_task(app: &tauri::AppHandle, db: &DbPool, task_id: i64) {
    use tauri::Emitter;
    if let Some(task) = tq::get_by_id(db, task_id) {
        app.emit("task:updated", &task).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::blockers::{BlockerKind, NewBlocker};
    use parking_lot::Mutex;
    use rusqlite::{params, Connection};
    use std::sync::Arc;
    use std::time::Duration;

    /// Mirrors `db::init_db`: `tasks.deleted_at` comes from a migration, and
    /// `tasks::get_by_id` filters on it, so create_tables alone is not enough.
    fn test_db() -> DbPool {
        let conn = Connection::open_in_memory().expect("in-memory db");
        crate::db::schema::create_tables(&conn);
        crate::db::schema::run_migrations(&conn);
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        Arc::new(Mutex::new(conn))
    }

    /// A blocked task with its timer already paused, and one open question.
    ///
    /// **Both** ids are given by the test, and the blocker row is written directly
    /// rather than through `db::blockers::create`. The waiter registry is a
    /// process-global keyed by blocker id, and every test builds its own in-memory
    /// database — so an autoincremented blocker id is 1 in all of them, and one
    /// test's parked waiter makes another test's answer look like the agent was
    /// still there. Distinct task ids are not enough.
    fn seed_blocked_task(
        db: &DbPool,
        task_id: i64,
        blocker_id: i64,
        options: &[(&str, &str)],
    ) -> (i64, i64) {
        {
            let conn = db.lock();
            conn.execute(
                "INSERT OR IGNORE INTO projects (id,name,slug,working_dir)
                 VALUES (1,'B','b','/repo')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO tasks (id,project_id,title,status,last_resumed_at)
                 VALUES (?1,1,'t','in_progress',datetime('now','localtime'))",
                params![task_id],
            )
            .unwrap();
            let kind = if options.is_empty() {
                BlockerKind::FreeText
            } else {
                BlockerKind::SingleChoice
            };
            conn.execute(
                "INSERT INTO task_blockers (id,task_id,kind,question,status)
                 VALUES (?1,?2,?3,'Which auth flow?','open')",
                params![blocker_id, task_id, kind.as_str()],
            )
            .unwrap();
            for (i, (label, description)) in options.iter().enumerate() {
                conn.execute(
                    "INSERT INTO task_blocker_options (blocker_id,label,description,sort_order)
                     VALUES (?1,?2,?3,?4)",
                    params![blocker_id, label, description, i as i64],
                )
                .unwrap();
            }
        }
        tq::update_status(db, task_id, "blocked");
        tq::pause_timer(db, task_id);
        (task_id, blocker_id)
    }

    fn status_of(db: &DbPool, id: i64) -> String {
        db.lock()
            .query_row("SELECT status FROM tasks WHERE id=?1", params![id], |r| {
                r.get(0)
            })
            .unwrap()
    }

    fn timer_running(db: &DbPool, id: i64) -> bool {
        db.lock()
            .query_row(
                "SELECT last_resumed_at IS NOT NULL FROM tasks WHERE id=?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn free_text(text: &str) -> ResponseInput {
        ResponseInput {
            option_id: None,
            note: None,
            free_text: Some(text.into()),
        }
    }

    #[tokio::test]
    async fn answering_while_the_agent_waits_resumes_the_task_and_its_timer() {
        let db = test_db();
        let (task, blocker) = seed_blocked_task(&db, 801, 8801, &[]);
        let waiting = {
            let db = db.clone();
            tokio::spawn(async move {
                wait::wait_for_answer(&db, blocker, Duration::from_secs(30)).await
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;

        let outcome = answer_blocker_in(&db, blocker, &[free_text("PKCE")]).unwrap();

        assert!(outcome.resumed_in_session);
        assert!(!outcome.needs_restart);
        assert_eq!(outcome.summary, "PKCE");
        // The agent carries on in the session it has, so the task is running again.
        assert_eq!(status_of(&db, task), "in_progress");
        assert!(timer_running(&db, task));
        // And the answer reached it.
        assert!(waiting.await.unwrap().is_some());
    }

    #[tokio::test]
    async fn answering_after_the_agent_gave_up_leaves_the_task_blocked() {
        let db = test_db();
        let (task, blocker) = seed_blocked_task(&db, 802, 8802, &[]);
        // Nobody is waiting: the deadline passed and the agent stopped.

        let outcome = answer_blocker_in(&db, blocker, &[free_text("PKCE")]).unwrap();

        assert!(!outcome.resumed_in_session);
        assert!(outcome.needs_restart);
        // in_progress with nothing running is reset to backlog by
        // recover_orphaned_tasks on the next launch, taking the answer with it.
        assert_eq!(status_of(&db, task), "blocked");
        assert!(!timer_running(&db, task));
        // The answer is recorded either way, ready for the restart.
        assert_eq!(db::blockers::answer_summary(&db, blocker), "PKCE");
    }

    #[tokio::test]
    async fn a_resumed_task_can_be_asked_again() {
        let db = test_db();
        let (task, blocker) = seed_blocked_task(&db, 803, 8803, &[]);
        let waiting = {
            let db = db.clone();
            tokio::spawn(async move {
                wait::wait_for_answer(&db, blocker, Duration::from_secs(30)).await
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        answer_blocker_in(&db, blocker, &[free_text("PKCE")]).unwrap();
        waiting.await.unwrap();

        // Blocked → Blocked is not a legal transition, so an agent left in blocked
        // after an answer would be unable to ask anything else all run.
        let second = db::blockers::create(
            &db,
            &NewBlocker::question(task, BlockerKind::FreeText, "and the refresh token?"),
        );

        assert!(second.is_ok(), "the agent must be able to ask again");
    }

    #[test]
    fn an_answer_with_nothing_in_it_is_refused() {
        let db = test_db();
        let (task, blocker) = seed_blocked_task(&db, 804, 8804, &[]);

        // Resuming an agent with an empty answer is worse than leaving it blocked.
        assert!(answer_blocker_in(&db, blocker, &[]).is_err());
        assert!(answer_blocker_in(&db, blocker, &[free_text("   ")]).is_err());
        assert!(answer_blocker_in(&db, blocker, &[ResponseInput::default()]).is_err());
        assert_eq!(status_of(&db, task), "blocked");
        assert!(db::blockers::open_for_task(&db, task).is_some());
    }

    #[test]
    fn a_blank_note_is_dropped_rather_than_carried_into_the_summary() {
        let db = test_db();
        let opts = [("PKCE", "")];
        let (_, blocker) = seed_blocked_task(&db, 805, 8805, &opts);
        let option_id = db::blockers::options(&db, blocker)[0].id;

        answer_blocker_in(
            &db,
            blocker,
            &[ResponseInput {
                option_id: Some(option_id),
                note: Some("   ".into()),
                free_text: None,
            }],
        )
        .unwrap();

        // "PKCE ( )" would be the alternative, in the text the agent reads.
        assert_eq!(db::blockers::answer_summary(&db, blocker), "PKCE");
    }

    #[test]
    fn answering_a_missing_blocker_is_an_error_not_a_panic() {
        let db = test_db();
        assert!(answer_blocker_in(&db, 9_999, &[free_text("hello")]).is_err());
    }

    #[tokio::test]
    async fn cancelling_stops_the_agent_and_leaves_the_task_blocked() {
        let db = test_db();
        let (task, blocker) = seed_blocked_task(&db, 806, 8806, &[]);
        let waiting = {
            let db = db.clone();
            tokio::spawn(async move {
                wait::wait_for_answer(&db, blocker, Duration::from_secs(30)).await
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;

        cancel_blocker_in(&db, blocker).unwrap();

        // The agent stops now rather than sitting out its deadline.
        assert!(waiting.await.unwrap().is_none());
        // Cancelling says "stop waiting", not "carry on".
        assert_eq!(status_of(&db, task), "blocked");
        assert!(db::blockers::open_for_task(&db, task).is_none());
    }

    #[test]
    fn moving_a_blocked_task_elsewhere_closes_its_question() {
        let db = test_db();
        let (task, _) = seed_blocked_task(&db, 807, 8807, &[]);

        cancel_open_blocker_for_task(&db, task);
        tq::update_status(&db, task, "backlog");

        // A task carrying an open question it is no longer blocked on would refuse
        // the next raise and render a panel nobody is waiting on.
        assert!(db::blockers::open_for_task(&db, task).is_none());
    }

    #[test]
    fn closing_the_question_on_a_task_that_has_none_is_harmless() {
        let db = test_db();
        let (task, blocker) = seed_blocked_task(&db, 808, 8808, &[]);
        cancel_blocker_in(&db, blocker).unwrap();

        cancel_open_blocker_for_task(&db, task);

        assert!(db::blockers::open_for_task(&db, task).is_none());
    }

    #[test]
    fn the_panel_reads_the_open_question_with_its_options_in_order() {
        let db = test_db();
        let opts = [("PKCE", "recommended"), ("Implicit", "")];
        let (task, blocker) = seed_blocked_task(&db, 809, 8809, &opts);

        let open = db::blockers::open_for_task(&db, task).expect("there is a question");

        assert_eq!(open.id, blocker);
        assert_eq!(open.question, "Which auth flow?");
        let labels: Vec<&str> = open.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, vec!["PKCE", "Implicit"]);
    }

    #[test]
    fn the_history_lists_answered_and_cancelled_questions_newest_first() {
        let db = test_db();
        let (task, first) = seed_blocked_task(&db, 810, 8810, &[]);
        answer_blocker_in(&db, first, &[free_text("yes")]).unwrap();
        tq::update_status(&db, task, "in_progress");
        let second = db::blockers::create(
            &db,
            &NewBlocker::question(task, BlockerKind::FreeText, "second?"),
        )
        .unwrap();

        let all = db::blockers::for_task(&db, task);

        assert_eq!(
            all.iter().map(|b| b.id).collect::<Vec<_>>(),
            vec![second, first]
        );
    }

    #[test]
    fn a_response_from_the_panel_deserialises_with_the_names_it_sends() {
        // camelCase over the IPC boundary, snake_case in Rust. A drifted rename
        // makes the field arrive as None and the answer read as empty.
        let parsed: Vec<ResponseInput> = serde_json::from_value(serde_json::json!([
            {"optionId": 3, "note": "only the cached reads"},
            {"freeText": "something else entirely"}
        ]))
        .expect("the panel's payload must deserialise");

        assert_eq!(parsed[0].option_id, Some(3));
        assert_eq!(parsed[0].note.as_deref(), Some("only the cached reads"));
        assert_eq!(
            parsed[1].free_text.as_deref(),
            Some("something else entirely")
        );
    }
}
