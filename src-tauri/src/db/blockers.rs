//! Questions an agent stopped to ask, and the answers it got.
//!
//! Kept out of `db/tasks.rs`, which is about the task row itself. A blocker is
//! three tables — the question, the options offered, the responses chosen — and
//! the invariant that matters spans them: a task has at most one open question at
//! a time, so there is exactly one thing to render and one answer to resume on.

use super::DbPool;
use crate::error::AppError;
use rusqlite::{params, Row};
use serde::Serialize;

/// The shape of the answer a blocker expects.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockerKind {
    #[serde(rename = "single_choice")]
    SingleChoice,
    #[serde(rename = "multi_choice")]
    MultiChoice,
    #[serde(rename = "free_text")]
    FreeText,
}

impl BlockerKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SingleChoice => "single_choice",
            Self::MultiChoice => "multi_choice",
            Self::FreeText => "free_text",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "single_choice" => Some(Self::SingleChoice),
            "multi_choice" => Some(Self::MultiChoice),
            "free_text" => Some(Self::FreeText),
            _ => None,
        }
    }

    /// Whether this kind is answered by picking from a list.
    ///
    /// A select with nothing to select is unanswerable, so the two choice kinds
    /// must be given options.
    pub fn needs_options(&self) -> bool {
        matches!(self, Self::SingleChoice | Self::MultiChoice)
    }
}

pub const STATUS_OPEN: &str = "open";
pub const STATUS_ANSWERED: &str = "answered";
pub const STATUS_CANCELLED: &str = "cancelled";

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Blocker {
    pub id: i64,
    pub task_id: i64,
    pub kind: BlockerKind,
    pub header: String,
    pub question: String,
    pub context: String,
    pub artifact_id: Option<i64>,
    pub status: String,
    pub created_at: Option<String>,
    pub answered_at: Option<String>,
    /// Empty for a free-text question.
    pub options: Vec<BlockerOption>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct BlockerOption {
    pub id: i64,
    pub blocker_id: i64,
    pub label: String,
    pub description: String,
    pub sort_order: i64,
}

/// One thing the user said in reply: an option they picked, free text they typed,
/// or an option with a note qualifying it.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct BlockerResponse {
    pub option_id: Option<i64>,
    pub note: Option<String>,
    pub free_text: Option<String>,
}

impl BlockerResponse {
    pub fn free_text(text: &str) -> Self {
        Self {
            option_id: None,
            note: None,
            free_text: Some(text.to_string()),
        }
    }

    pub fn option(option_id: i64, note: Option<&str>) -> Self {
        Self {
            option_id: Some(option_id),
            note: note.map(str::to_string),
            free_text: None,
        }
    }

    /// Whether this carries anything the agent could act on.
    ///
    /// A response that is neither a choice nor any text says nothing, and resuming
    /// an agent with it is worse than leaving the task blocked.
    pub fn is_empty(&self) -> bool {
        self.option_id.is_none()
            && self
                .free_text
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
    }
}

fn row_to_blocker(row: &Row) -> rusqlite::Result<Blocker> {
    let kind: String = row.get("kind")?;
    Ok(Blocker {
        id: row.get("id")?,
        task_id: row.get("task_id")?,
        // The CHECK constraint keeps the column to the three known values, so an
        // unreadable one means the row was written outside this module.
        kind: BlockerKind::from_str(&kind).unwrap_or(BlockerKind::FreeText),
        header: row.get::<_, Option<String>>("header")?.unwrap_or_default(),
        question: row.get("question")?,
        context: row.get::<_, Option<String>>("context")?.unwrap_or_default(),
        artifact_id: row.get("artifact_id").ok().flatten(),
        status: row
            .get::<_, Option<String>>("status")?
            .unwrap_or_else(|| STATUS_OPEN.into()),
        created_at: row.get("created_at").ok().flatten(),
        answered_at: row.get("answered_at").ok().flatten(),
        options: Vec::new(),
    })
}

fn row_to_option(row: &Row) -> rusqlite::Result<BlockerOption> {
    Ok(BlockerOption {
        id: row.get("id")?,
        blocker_id: row.get("blocker_id")?,
        label: row.get("label")?,
        description: row
            .get::<_, Option<String>>("description")?
            .unwrap_or_default(),
        sort_order: row.get::<_, Option<i64>>("sort_order")?.unwrap_or(0),
    })
}

/// What a caller supplies when raising a question.
///
/// A struct rather than positional arguments because `header`, `question` and
/// `context` are three adjacent strings: swapping two of them compiles cleanly and
/// produces a blocker asking the wrong thing.
#[derive(Clone, Debug)]
pub struct NewBlocker<'a> {
    pub task_id: i64,
    pub kind: BlockerKind,
    /// Short chip above the question. Optional.
    pub header: &'a str,
    pub question: &'a str,
    /// What the agent had established before it got stuck. Optional.
    pub context: &'a str,
    /// The document the question is about, when there is one.
    pub artifact_id: Option<i64>,
    /// Label and description pairs, in the order they should be shown.
    pub options: &'a [(String, String)],
}

impl<'a> NewBlocker<'a> {
    /// A bare question: no header, no context, no options, no document.
    pub fn question(task_id: i64, kind: BlockerKind, question: &'a str) -> Self {
        Self {
            task_id,
            kind,
            header: "",
            question,
            context: "",
            artifact_id: None,
            options: &[],
        }
    }

    pub fn with_header(mut self, header: &'a str) -> Self {
        self.header = header;
        self
    }

    pub fn with_context(mut self, context: &'a str) -> Self {
        self.context = context;
        self
    }

    pub fn with_options(mut self, options: &'a [(String, String)]) -> Self {
        self.options = options;
        self
    }

    pub fn about(mut self, artifact_id: i64) -> Self {
        self.artifact_id = Some(artifact_id);
        self
    }
}

/// Raise a question against a task.
///
/// Refused when the task already has an open one: the partial unique index makes
/// that a constraint violation rather than a race, so two agents asking at once
/// cannot both win.
///
/// The question and its options are written in one transaction so they become
/// visible together. The board polls [`open_for_task`], and outside a transaction
/// it can read a question whose options have not been inserted yet — rendering a
/// select with nothing to select.
pub fn create(db: &DbPool, new: &NewBlocker) -> Result<i64, AppError> {
    let NewBlocker {
        task_id,
        kind,
        header,
        context,
        artifact_id,
        ..
    } = *new;
    let question = new.question.trim();
    if question.is_empty() {
        return Err(AppError::Validation(
            "a blocker needs a question".to_string(),
        ));
    }
    let labelled: Vec<&(String, String)> = new
        .options
        .iter()
        .filter(|(label, _)| !label.trim().is_empty())
        .collect();
    if kind.needs_options() && labelled.is_empty() {
        return Err(AppError::Validation(format!(
            "a {} blocker needs at least one option",
            kind.as_str()
        )));
    }

    super::with_transaction(db, |conn| {
        conn.execute(
            "INSERT INTO task_blockers
                (task_id, kind, header, question, context, artifact_id, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'open')",
            params![
                task_id,
                kind.as_str(),
                header.trim(),
                question,
                context,
                artifact_id
            ],
        )
        .map_err(|e| e.to_string())?;
        let id = conn.last_insert_rowid();
        for (i, (label, description)) in labelled.iter().enumerate() {
            conn.execute(
                "INSERT INTO task_blocker_options (blocker_id, label, description, sort_order)
                 VALUES (?1, ?2, ?3, ?4)",
                params![id, label.trim(), description.trim(), i as i64],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(id)
    })
    .map_err(AppError::Database)
}

/// The task's open question, with its options, or `None` when it has none.
pub fn open_for_task(db: &DbPool, task_id: i64) -> Option<Blocker> {
    let conn = db.lock();
    let mut blocker = conn
        .query_row(
            "SELECT * FROM task_blockers WHERE task_id=?1 AND status='open'",
            params![task_id],
            row_to_blocker,
        )
        .ok()?;
    blocker.options = options_with(&conn, blocker.id);
    Some(blocker)
}

/// Every question raised on a task, newest first, each with its options.
///
/// The whole history rather than just the open one: what was asked and what was
/// answered is the record of why the task went the way it did.
pub fn for_task(db: &DbPool, task_id: i64) -> Vec<Blocker> {
    let conn = db.lock();
    let mut stmt =
        match conn.prepare("SELECT * FROM task_blockers WHERE task_id=?1 ORDER BY id DESC") {
            Ok(s) => s,
            Err(e) => {
                log::error!("blockers for_task: {}", e);
                return vec![];
            }
        };
    let mut out: Vec<Blocker> = Vec::new();
    match stmt.query_map(params![task_id], row_to_blocker) {
        Ok(rows) => out.extend(rows.filter_map(|r| r.ok())),
        Err(e) => log::error!("blockers for_task: {}", e),
    }
    for b in &mut out {
        b.options = options_with(&conn, b.id);
    }
    out
}

/// A blocker by id, whatever its status, with its options.
pub fn get(db: &DbPool, blocker_id: i64) -> Option<Blocker> {
    let conn = db.lock();
    let mut blocker = conn
        .query_row(
            "SELECT * FROM task_blockers WHERE id=?1",
            params![blocker_id],
            row_to_blocker,
        )
        .ok()?;
    blocker.options = options_with(&conn, blocker.id);
    Some(blocker)
}

fn options_with(conn: &rusqlite::Connection, blocker_id: i64) -> Vec<BlockerOption> {
    let mut stmt = match conn
        .prepare("SELECT * FROM task_blocker_options WHERE blocker_id=?1 ORDER BY sort_order, id")
    {
        Ok(s) => s,
        Err(e) => {
            log::error!("blocker options: {}", e);
            return vec![];
        }
    };
    let mut out = Vec::new();
    match stmt.query_map(params![blocker_id], row_to_option) {
        Ok(rows) => out.extend(rows.filter_map(|r| r.ok())),
        Err(e) => log::error!("blocker options: {}", e),
    }
    out
}

/// The options offered for a blocker, in the order they were given.
pub fn options(db: &DbPool, blocker_id: i64) -> Vec<BlockerOption> {
    let conn = db.lock();
    options_with(&conn, blocker_id)
}

/// Record the user's answer and close the question.
///
/// Rejects an answer that says nothing, and an option that belongs to a different
/// blocker — the responses are what the agent is resumed with, so a wrong or empty
/// one is worse than the task staying blocked.
pub fn answer(db: &DbPool, blocker_id: i64, responses: &[BlockerResponse]) -> Result<(), AppError> {
    let usable: Vec<&BlockerResponse> = responses.iter().filter(|r| !r.is_empty()).collect();
    if usable.is_empty() {
        return Err(AppError::Validation(
            "an answer needs a choice or some text".to_string(),
        ));
    }

    let valid_options: Vec<i64> = options(db, blocker_id).into_iter().map(|o| o.id).collect();
    for r in &usable {
        if let Some(id) = r.option_id {
            if !valid_options.contains(&id) {
                return Err(AppError::Validation(format!(
                    "option {} does not belong to blocker {}",
                    id, blocker_id
                )));
            }
        }
    }

    super::with_transaction(db, |conn| {
        let closed = conn
            .execute(
                "UPDATE task_blockers
                    SET status='answered', answered_at=datetime('now','localtime')
                  WHERE id=?1 AND status='open'",
                params![blocker_id],
            )
            .map_err(|e| e.to_string())?;
        if closed == 0 {
            // Already answered or cancelled. Writing responses now would attach
            // them to a question nobody is waiting on.
            return Err(format!("blocker {} is not open", blocker_id));
        }
        for r in &usable {
            conn.execute(
                "INSERT INTO task_blocker_responses (blocker_id, option_id, note, free_text)
                 VALUES (?1, ?2, ?3, ?4)",
                params![blocker_id, r.option_id, r.note, r.free_text],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    })
    .map_err(AppError::Validation)
}

/// Close a question without answering it.
///
/// The task stays blocked: cancelling says "stop waiting", not "carry on".
pub fn cancel(db: &DbPool, blocker_id: i64) -> Result<(), AppError> {
    let conn = db.lock();
    conn.execute(
        "UPDATE task_blockers SET status='cancelled' WHERE id=?1 AND status='open'",
        params![blocker_id],
    )?;
    Ok(())
}

/// What the user replied, in the order it was recorded.
pub fn responses(db: &DbPool, blocker_id: i64) -> Vec<BlockerResponse> {
    let conn = db.lock();
    let mut stmt = match conn.prepare(
        "SELECT option_id, note, free_text FROM task_blocker_responses
         WHERE blocker_id=?1 ORDER BY id",
    ) {
        Ok(s) => s,
        Err(e) => {
            log::error!("blocker responses: {}", e);
            return vec![];
        }
    };
    let mut out = Vec::new();
    match stmt.query_map(params![blocker_id], |r| {
        Ok(BlockerResponse {
            option_id: r.get(0).ok().flatten(),
            note: r.get(1).ok().flatten(),
            free_text: r.get(2).ok().flatten(),
        })
    }) {
        Ok(rows) => out.extend(rows.filter_map(|r| r.ok())),
        Err(e) => log::error!("blocker responses: {}", e),
    }
    out
}

/// The answer as one line of prose, for injecting into the agent's prompt.
///
/// Reads the chosen labels rather than their ids, because the agent never saw the
/// ids — it offered labels and needs to be told which of them won.
pub fn answer_summary(db: &DbPool, blocker_id: i64) -> String {
    let by_id: std::collections::HashMap<i64, String> = options(db, blocker_id)
        .into_iter()
        .map(|o| (o.id, o.label))
        .collect();
    let mut parts: Vec<String> = Vec::new();
    for r in responses(db, blocker_id) {
        let label = r.option_id.and_then(|id| by_id.get(&id).cloned());
        let note = r.note.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let text = r
            .free_text
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        match (label, note, text) {
            (Some(l), Some(n), _) => parts.push(format!("{} ({})", l, n)),
            (Some(l), None, _) => parts.push(l),
            (None, _, Some(t)) => parts.push(t.to_string()),
            (None, _, None) => {}
        }
    }
    parts.join("; ")
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
        // The CASCADE and SET NULL behaviour these tests assert needs foreign keys.
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        Arc::new(Mutex::new(conn))
    }

    fn seed_task(db: &DbPool, title: &str) -> i64 {
        let conn = db.lock();
        conn.execute(
            "INSERT OR IGNORE INTO projects (id,name,slug,working_dir)
             VALUES (1,'B','b','/repo')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (project_id,title,status) VALUES (1,?1,'in_progress')",
            params![title],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn two_options() -> Vec<(String, String)> {
        vec![
            ("Read path".into(), "first".into()),
            ("Write path".into(), "second".into()),
        ]
    }

    #[test]
    fn a_task_can_only_have_one_open_blocker() {
        let db = test_db();
        let t = seed_task(&db, "t");
        create(
            &db,
            &NewBlocker::question(t, BlockerKind::FreeText, "first?"),
        )
        .unwrap();

        let second = create(
            &db,
            &NewBlocker::question(t, BlockerKind::FreeText, "second?"),
        );

        assert!(second.is_err(), "a second open blocker must be refused");
        // And the refusal must leave the first one answerable.
        assert_eq!(
            open_for_task(&db, t).map(|b| b.question),
            Some("first?".to_string())
        );
    }

    #[test]
    fn two_tasks_can_each_have_their_own_open_blocker() {
        let db = test_db();
        let one = seed_task(&db, "one");
        let two = seed_task(&db, "two");

        create(&db, &NewBlocker::question(one, BlockerKind::FreeText, "q?")).unwrap();

        // The unique index is per task, not global.
        assert!(create(&db, &NewBlocker::question(two, BlockerKind::FreeText, "q?")).is_ok());
    }

    #[test]
    fn answering_closes_the_blocker_and_frees_the_task_for_another() {
        let db = test_db();
        let t = seed_task(&db, "t");
        let id = create(&db, &NewBlocker::question(t, BlockerKind::FreeText, "q?")).unwrap();

        answer(&db, id, &[BlockerResponse::free_text("do it this way")]).unwrap();

        assert!(open_for_task(&db, t).is_none());
        assert!(create(
            &db,
            &NewBlocker::question(t, BlockerKind::FreeText, "next?")
        )
        .is_ok());
    }

    #[test]
    fn multi_choice_answers_keep_their_per_option_notes() {
        let db = test_db();
        let t = seed_task(&db, "t");
        let id = create(
            &db,
            &NewBlocker::question(t, BlockerKind::MultiChoice, "Which?")
                .with_header("Scope")
                .with_options(&two_options()),
        )
        .unwrap();
        let opts = options(&db, id);

        answer(
            &db,
            id,
            &[
                BlockerResponse::option(opts[0].id, Some("but only the cached reads")),
                BlockerResponse::option(opts[1].id, None),
            ],
        )
        .unwrap();

        let saved = responses(&db, id);
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[0].note.as_deref(), Some("but only the cached reads"));
        assert_eq!(saved[1].note, None);
    }

    #[test]
    fn options_come_back_in_the_order_they_were_offered() {
        let db = test_db();
        let t = seed_task(&db, "t");
        // The agent's order carries meaning — its preferred option tends to be
        // first — so the panel must not reorder them.
        let id = create(
            &db,
            &NewBlocker::question(t, BlockerKind::SingleChoice, "Which?")
                .with_options(&two_options()),
        )
        .unwrap();

        let labels: Vec<String> = options(&db, id).into_iter().map(|o| o.label).collect();

        assert_eq!(labels, vec!["Read path", "Write path"]);
    }

    #[test]
    fn a_choice_blocker_with_no_options_is_refused() {
        let db = test_db();
        let t = seed_task(&db, "t");

        // A select with nothing to select is unanswerable.
        assert!(create(
            &db,
            &NewBlocker::question(t, BlockerKind::SingleChoice, "Which?")
        )
        .is_err());
        assert!(create(
            &db,
            &NewBlocker::question(t, BlockerKind::MultiChoice, "Which?")
                .with_options(&[(" ".into(), "blank".into())])
        )
        .is_err());
        // Nothing was written, so the task is still free to be asked properly.
        assert!(open_for_task(&db, t).is_none());
    }

    #[test]
    fn a_blocker_with_no_question_is_refused() {
        let db = test_db();
        let t = seed_task(&db, "t");

        assert!(create(&db, &NewBlocker::question(t, BlockerKind::FreeText, "   ")).is_err());
    }

    #[test]
    fn a_refused_blocker_writes_nothing_at_all() {
        let db = test_db();
        let t = seed_task(&db, "t");
        create(
            &db,
            &NewBlocker::question(t, BlockerKind::SingleChoice, "first?")
                .with_options(&two_options()),
        )
        .unwrap();

        // Refused by the one-open-blocker index.
        assert!(create(
            &db,
            &NewBlocker::question(t, BlockerKind::SingleChoice, "second?")
                .with_options(&two_options())
        )
        .is_err());

        let conn = db.lock();
        let blockers: i64 = conn
            .query_row("SELECT COUNT(*) FROM task_blockers", [], |r| r.get(0))
            .unwrap();
        let opts: i64 = conn
            .query_row("SELECT COUNT(*) FROM task_blocker_options", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(blockers, 1);
        assert_eq!(opts, 2, "only the first question's options exist");
    }

    #[test]
    fn an_answer_that_says_nothing_is_refused() {
        let db = test_db();
        let t = seed_task(&db, "t");
        let id = create(&db, &NewBlocker::question(t, BlockerKind::FreeText, "q?")).unwrap();

        // Resuming an agent with an empty answer is worse than leaving it blocked.
        assert!(answer(&db, id, &[]).is_err());
        assert!(answer(&db, id, &[BlockerResponse::free_text("   ")]).is_err());
        assert!(answer(&db, id, &[BlockerResponse::default()]).is_err());
        // Still open, so it can still be answered properly.
        assert!(open_for_task(&db, t).is_some());
    }

    #[test]
    fn an_option_from_another_blocker_is_refused() {
        let db = test_db();
        let mine = seed_task(&db, "mine");
        let other = seed_task(&db, "other");
        let a = create(
            &db,
            &NewBlocker::question(mine, BlockerKind::SingleChoice, "q?")
                .with_options(&two_options()),
        )
        .unwrap();
        let b = create(
            &db,
            &NewBlocker::question(other, BlockerKind::SingleChoice, "q?")
                .with_options(&two_options()),
        )
        .unwrap();
        let theirs = options(&db, b)[0].id;

        assert!(answer(&db, a, &[BlockerResponse::option(theirs, None)]).is_err());
        assert!(open_for_task(&db, mine).is_some());
    }

    #[test]
    fn a_blocker_cannot_be_answered_twice() {
        let db = test_db();
        let t = seed_task(&db, "t");
        let id = create(&db, &NewBlocker::question(t, BlockerKind::FreeText, "q?")).unwrap();
        answer(&db, id, &[BlockerResponse::free_text("first")]).unwrap();

        // The agent resumed on the first answer; a second would never reach it.
        assert!(answer(&db, id, &[BlockerResponse::free_text("second")]).is_err());
        assert_eq!(responses(&db, id).len(), 1);
    }

    #[test]
    fn cancelling_closes_the_question_without_an_answer() {
        let db = test_db();
        let t = seed_task(&db, "t");
        let id = create(&db, &NewBlocker::question(t, BlockerKind::FreeText, "q?")).unwrap();

        cancel(&db, id).unwrap();

        assert!(open_for_task(&db, t).is_none());
        assert_eq!(get(&db, id).unwrap().status, STATUS_CANCELLED);
        assert!(responses(&db, id).is_empty());
        // A cancelled question cannot be answered after the fact.
        assert!(answer(&db, id, &[BlockerResponse::free_text("late")]).is_err());
    }

    #[test]
    fn deleting_the_task_takes_its_blockers_and_their_options() {
        let db = test_db();
        let t = seed_task(&db, "t");
        let id = create(
            &db,
            &NewBlocker::question(t, BlockerKind::SingleChoice, "q?").with_options(&two_options()),
        )
        .unwrap();

        {
            let conn = db.lock();
            conn.execute("DELETE FROM tasks WHERE id=?1", params![t])
                .unwrap();
        }

        assert!(get(&db, id).is_none());
        assert!(options(&db, id).is_empty());
    }

    #[test]
    fn deleting_the_document_leaves_the_question_standing() {
        let db = test_db();
        let t = seed_task(&db, "t");
        let artifact = {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO artifacts (project_id, stored_name) VALUES (1,'plan-1.md')",
                [],
            )
            .unwrap();
            conn.last_insert_rowid()
        };
        let id = create(
            &db,
            &NewBlocker::question(t, BlockerKind::FreeText, "About this plan?").about(artifact),
        )
        .unwrap();

        {
            let conn = db.lock();
            conn.execute("DELETE FROM artifacts WHERE id=?1", params![artifact])
                .unwrap();
        }

        // The agent is still waiting on the question, so it has to survive its
        // subject being deleted.
        let still_there = get(&db, id).expect("the question must survive");
        assert_eq!(still_there.artifact_id, None);
        assert_eq!(still_there.status, STATUS_OPEN);
    }

    #[test]
    fn the_summary_names_the_labels_the_agent_offered() {
        let db = test_db();
        let t = seed_task(&db, "t");
        let id = create(
            &db,
            &NewBlocker::question(t, BlockerKind::MultiChoice, "Which?")
                .with_options(&two_options()),
        )
        .unwrap();
        let opts = options(&db, id);
        answer(
            &db,
            id,
            &[
                BlockerResponse::option(opts[0].id, Some("cached only")),
                BlockerResponse::option(opts[1].id, None),
            ],
        )
        .unwrap();

        // Ids mean nothing to the agent; it offered labels and gets labels back.
        assert_eq!(
            answer_summary(&db, id),
            "Read path (cached only); Write path"
        );
    }

    #[test]
    fn the_summary_carries_free_text_verbatim() {
        let db = test_db();
        let t = seed_task(&db, "t");
        let id = create(&db, &NewBlocker::question(t, BlockerKind::FreeText, "How?")).unwrap();

        answer(&db, id, &[BlockerResponse::free_text("  use PKCE  ")]).unwrap();

        assert_eq!(answer_summary(&db, id), "use PKCE");
    }

    #[test]
    fn a_kind_round_trips_through_its_stored_name() {
        for k in [
            BlockerKind::SingleChoice,
            BlockerKind::MultiChoice,
            BlockerKind::FreeText,
        ] {
            assert_eq!(BlockerKind::from_str(k.as_str()), Some(k));
        }
        // The HTTP handler validates with this, so an agent's typo must not pass.
        assert_eq!(BlockerKind::from_str("whatever"), None);
    }
}
