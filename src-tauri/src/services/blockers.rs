//! Parking a raised blocker until someone answers it.
//!
//! The agent's `raise_blocker` call is a request that does not return until the
//! question is settled or the deadline passes, so something has to hold that
//! request open. This module owns that wait: in-process synchronisation, not
//! storage, which is why it is separate from `db::blockers`.
//!
//! The wait is `async`. The HTTP API is axum on tokio and its handlers are
//! `async fn`, so a thread-blocking wait — a `Condvar`, a `sleep` loop — parks a
//! tokio worker rather than a spare thread. With a handful of blocked tasks that
//! starves every other request, including the ones the board needs in order to
//! answer them.

use crate::db::blockers::{self, BlockerResponse};
use crate::db::DbPool;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// How long a parked waiter goes before re-reading the database even with no
/// notification.
///
/// A safety net, not the mechanism: an answer normally arrives through
/// [`notify_settled`] within milliseconds. This only bounds the damage if a
/// notification is ever missed — a wait that never re-checks would hang until its
/// deadline while the answer sat in the database.
const WAKE_CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// What the agent is told when its question is answered.
#[derive(Clone, Debug, PartialEq)]
pub struct AnswerPayload {
    /// The reply as one line of prose, naming the labels the agent offered.
    pub summary: String,
    pub responses: Vec<BlockerResponse>,
}

/// Why a wait ended.
#[derive(Clone, Debug, PartialEq)]
enum Settled {
    Answered(AnswerPayload),
    /// Cancelled, or the blocker is gone with its task. Either way there is no
    /// answer coming and waiting longer is pointless.
    Unanswered,
}

static WAITERS: Lazy<Mutex<HashMap<i64, Arc<Notify>>>> = Lazy::new(|| Mutex::new(HashMap::new()));

/// Wake anything parked on this blocker, whether it was answered or cancelled.
///
/// Cheap and safe to call for a blocker nobody is waiting on: it notifies an
/// existing registration and never creates one, so the map cannot grow through
/// this path.
pub fn notify_settled(blocker_id: i64) {
    let waiter = WAITERS.lock().get(&blocker_id).cloned();
    if let Some(n) = waiter {
        // notify_waiters wakes everyone already parked; notify_one leaves a permit
        // for a waiter that has not registered yet, so an answer landing in the
        // gap between raise and park is not lost. A spurious wake costs one
        // database read, because every wake re-checks.
        n.notify_waiters();
        n.notify_one();
    }
}

/// Park until the blocker is settled, or until `deadline` elapses.
///
/// Returns the answer, or `None` when the deadline passed, the question was
/// cancelled, or the task it belonged to is gone. A `None` is the agent's cue to
/// stop cleanly and leave its work in place — the task stays blocked and is
/// resumed with the answer later.
pub async fn wait_for_answer(
    db: &DbPool,
    blocker_id: i64,
    deadline: Duration,
) -> Option<AnswerPayload> {
    let notify = register(blocker_id);
    let started = Instant::now();
    let outcome = loop {
        let Some(remaining) = deadline.checked_sub(started.elapsed()) else {
            break None;
        };
        if remaining.is_zero() {
            break None;
        }
        let wake = notify.notified();
        // Read before awaiting, every time round. This is what covers the answer
        // that lands before the wait even starts — the board may already have been
        // open on this task — and the one that lands between a read and the next
        // await. Parking first and reading only on a notification would sit out the
        // whole deadline with the answer already in the database.
        match settled(db, blocker_id) {
            Some(Settled::Answered(payload)) => break Some(payload),
            Some(Settled::Unanswered) => break None,
            None => {}
        }
        // Waking early is fine — the loop re-reads. Waking late is not, hence the
        // interval cap on top of the deadline.
        let _ = tokio::time::timeout(remaining.min(WAKE_CHECK_INTERVAL), wake).await;
    };

    unregister(blocker_id);
    outcome
}

/// Whether anything is currently parked on this blocker.
///
/// Scoped to one id rather than reporting the size of the map, because the map is
/// process-global: a total would count every other caller's waiters too.
pub fn is_waiting_on(blocker_id: i64) -> bool {
    WAITERS.lock().contains_key(&blocker_id)
}

fn register(blocker_id: i64) -> Arc<Notify> {
    WAITERS
        .lock()
        .entry(blocker_id)
        .or_insert_with(|| Arc::new(Notify::new()))
        .clone()
}

fn unregister(blocker_id: i64) {
    let mut map = WAITERS.lock();
    // Only the last waiter out clears the entry. Two callers can be parked on one
    // question — the agent's long poll and a board poll — and removing the shared
    // Notify while the other is still parked loses it the notification, leaving it
    // to wait out WAKE_CHECK_INTERVAL instead. The map holds one reference and this
    // caller holds the other, so anything above two means someone else is parked.
    if let Some(n) = map.get(&blocker_id) {
        if Arc::strong_count(n) <= 2 {
            map.remove(&blocker_id);
        }
    }
}

/// The blocker's outcome, or `None` while it is still open.
fn settled(db: &DbPool, blocker_id: i64) -> Option<Settled> {
    let Some(blocker) = blockers::get(db, blocker_id) else {
        // The task was deleted and took its question with it. Nothing will answer.
        return Some(Settled::Unanswered);
    };
    if blocker.status == blockers::STATUS_OPEN {
        return None;
    }
    if blocker.status != blockers::STATUS_ANSWERED {
        return Some(Settled::Unanswered);
    }
    let responses = blockers::responses(db, blocker_id);
    if responses.is_empty() {
        // Answered with nothing recorded should be impossible — db::blockers
        // refuses an empty answer — but resuming an agent on it would be worse
        // than telling it to stop.
        return Some(Settled::Unanswered);
    }
    Some(Settled::Answered(AnswerPayload {
        summary: blockers::answer_summary(db, blocker_id),
        responses,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::blockers::{BlockerKind, NewBlocker};
    use rusqlite::params;
    use std::sync::Arc as StdArc;

    fn test_db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory db");
        crate::db::schema::create_tables(&conn);
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        StdArc::new(Mutex::new(conn))
    }

    /// Seeds a task and an open blocker under an explicit id.
    ///
    /// The id has to be given by the test, not autoincremented: WAITERS is a
    /// process-global keyed by blocker id, every test builds its own in-memory
    /// database, and autoincrement would hand id 1 to all of them — so tests
    /// running in parallel would wake each other's waiters.
    fn seed_open_blocker(db: &DbPool, id: i64) -> i64 {
        let conn = db.lock();
        conn.execute(
            "INSERT OR IGNORE INTO projects (id,name,slug,working_dir)
             VALUES (1,'B','b','/repo')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id,project_id,title,status)
             VALUES (?1,1,'t','in_progress')",
            params![id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO task_blockers (id,task_id,kind,question,status)
             VALUES (?1,?1,'free_text','q?','open')",
            params![id],
        )
        .unwrap();
        id
    }

    fn answer_it(db: &DbPool, id: i64, text: &str) {
        blockers::answer(db, id, &[BlockerResponse::free_text(text)]).unwrap();
    }

    #[tokio::test]
    async fn a_waiter_wakes_as_soon_as_the_answer_lands() {
        let db = test_db();
        let id = seed_open_blocker(&db, 901);
        let waiting = {
            let db = db.clone();
            tokio::spawn(async move { wait_for_answer(&db, id, Duration::from_secs(30)).await })
        };

        // Let the waiter park, then answer.
        tokio::time::sleep(Duration::from_millis(50)).await;
        answer_it(&db, id, "yes");
        notify_settled(id);

        let started = Instant::now();
        let payload = waiting.await.unwrap().expect("should have woken");
        assert!(payload.summary.contains("yes"));
        // Through the notification, not the safety-net re-read. If this only
        // passes after WAKE_CHECK_INTERVAL, the notify path is broken.
        assert!(
            started.elapsed() < WAKE_CHECK_INTERVAL / 2,
            "woke after {:?}, which means it polled rather than being notified",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn a_waiter_gives_up_at_the_deadline() {
        let db = test_db();
        let id = seed_open_blocker(&db, 902);
        let started = Instant::now();

        let result = wait_for_answer(&db, id, Duration::from_millis(120)).await;

        assert!(result.is_none());
        // A wedged agent is worse than an unanswered question.
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn an_already_answered_blocker_returns_immediately() {
        let db = test_db();
        let id = seed_open_blocker(&db, 903);
        answer_it(&db, id, "early");

        // The answer may land before the waiter parks; that must not hang.
        let started = Instant::now();
        let payload = wait_for_answer(&db, id, Duration::from_secs(30)).await;

        assert_eq!(payload.map(|p| p.summary), Some("early".to_string()));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn a_cancelled_blocker_stops_the_wait_rather_than_holding_it() {
        let db = test_db();
        let id = seed_open_blocker(&db, 904);
        let waiting = {
            let db = db.clone();
            tokio::spawn(async move { wait_for_answer(&db, id, Duration::from_secs(30)).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;

        blockers::cancel(&db, id).unwrap();
        notify_settled(id);

        // No answer is coming, so the agent should be told to stop now rather
        // than sit for the full deadline.
        let started = Instant::now();
        assert!(waiting.await.unwrap().is_none());
        assert!(started.elapsed() < WAKE_CHECK_INTERVAL / 2);
    }

    #[tokio::test]
    async fn a_blocker_deleted_with_its_task_stops_the_wait() {
        let db = test_db();
        let id = seed_open_blocker(&db, 905);
        let waiting = {
            let db = db.clone();
            tokio::spawn(async move { wait_for_answer(&db, id, Duration::from_secs(30)).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;

        {
            let conn = db.lock();
            conn.execute("DELETE FROM tasks WHERE id=?1", params![id])
                .unwrap();
        }
        notify_settled(id);

        // The question went with the task. Nothing will ever answer it.
        assert!(waiting.await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_finished_wait_leaves_no_registration_behind() {
        let db = test_db();
        let id = seed_open_blocker(&db, 906);

        wait_for_answer(&db, id, Duration::from_millis(60)).await;

        // A registration that is never cleaned up is a leak for the life of the
        // process, and every raised blocker would add one.
        assert!(!is_waiting_on(id));
    }

    #[tokio::test]
    async fn notifying_a_blocker_nobody_waits_on_is_harmless() {
        notify_settled(9_999_999);

        // Must not register anything: this is called on every answer, including
        // answers to blockers whose agent already gave up and went away.
        assert!(!is_waiting_on(9_999_999));
    }

    #[tokio::test]
    async fn two_waiters_on_one_blocker_both_wake() {
        let db = test_db();
        let id = seed_open_blocker(&db, 907);
        // The agent's long poll and a board poll can both be parked on the same
        // question; whichever finishes first must not strand the other.
        let one = {
            let db = db.clone();
            tokio::spawn(async move { wait_for_answer(&db, id, Duration::from_secs(30)).await })
        };
        let two = {
            let db = db.clone();
            tokio::spawn(async move { wait_for_answer(&db, id, Duration::from_secs(30)).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;

        answer_it(&db, id, "both");
        notify_settled(id);

        let started = Instant::now();
        assert!(one.await.unwrap().is_some());
        assert!(two.await.unwrap().is_some());
        assert!(started.elapsed() < WAKE_CHECK_INTERVAL / 2);
        assert!(!is_waiting_on(id), "both waiters must clean up");
    }

    #[tokio::test]
    async fn one_waiter_timing_out_does_not_strand_another() {
        let db = test_db();
        let id = seed_open_blocker(&db, 909);
        // The agent's long poll alongside a short poll from the board. They share
        // one registration, so the short one's cleanup must not take it away.
        let long = {
            let db = db.clone();
            tokio::spawn(async move { wait_for_answer(&db, id, Duration::from_secs(30)).await })
        };
        let short = {
            let db = db.clone();
            tokio::spawn(async move { wait_for_answer(&db, id, Duration::from_millis(80)).await })
        };

        assert!(
            short.await.unwrap().is_none(),
            "the short poll should expire"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        answer_it(&db, id, "still listening");
        notify_settled(id);

        let started = Instant::now();
        assert!(long.await.unwrap().is_some());
        assert!(
            started.elapsed() < WAKE_CHECK_INTERVAL / 2,
            "the long poll lost its notification and fell back to re-reading"
        );
    }

    #[tokio::test]
    async fn the_payload_carries_the_responses_the_agent_needs() {
        let db = test_db();
        let t = {
            let conn = db.lock();
            conn.execute(
                "INSERT OR IGNORE INTO projects (id,name,slug,working_dir)
                 VALUES (1,'B','b','/repo')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO tasks (id,project_id,title,status) VALUES (908,1,'t','in_progress')",
                [],
            )
            .unwrap();
            908
        };
        let opts = vec![("PKCE".to_string(), "recommended".to_string())];
        let id = blockers::create(
            &db,
            &NewBlocker::question(t, BlockerKind::SingleChoice, "Which flow?").with_options(&opts),
        )
        .unwrap();
        let option_id = blockers::options(&db, id)[0].id;
        blockers::answer(
            &db,
            id,
            &[BlockerResponse::option(option_id, Some("web only"))],
        )
        .unwrap();

        let payload = wait_for_answer(&db, id, Duration::from_secs(5))
            .await
            .expect("answered");

        // The summary is what goes into the prompt; the responses are what a
        // caller needs to record or render.
        assert_eq!(payload.summary, "PKCE (web only)");
        assert_eq!(payload.responses.len(), 1);
        assert_eq!(payload.responses[0].option_id, Some(option_id));
    }
}
