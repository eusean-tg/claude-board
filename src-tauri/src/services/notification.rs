use crate::db::{self, settings};
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

// ─── i18n strings ───

struct Strings {
    task_started: &'static str,
    task_completed: &'static str,
    task_failed: &'static str,
    test_passed: &'static str,
    test_failed: &'static str,
    revision_requested: &'static str,
    queue_started: &'static str,
    blocker_raised: &'static str,
    body_started: &'static str,
    body_completed: &'static str,
    body_failed: &'static str,
    body_test_passed: &'static str,
    body_test_failed: &'static str,
    body_revision: &'static str,
    body_queue: &'static str,
    body_blocker: &'static str,
    body_unknown_error: &'static str,
}

/// English is the only bundled locale. When a second one is added, match on
/// `lang` here and fall through to these strings for unrecognised codes; the
/// language still arrives from app settings at every call site.
fn strings(_lang: &str) -> Strings {
    Strings {
        task_started: "Task Started",
        task_completed: "Task Completed",
        task_failed: "Task Failed",
        test_passed: "Test Passed",
        test_failed: "Test Failed",
        revision_requested: "Revision Requested",
        queue_started: "Queue Started",
        blocker_raised: "Question Waiting",
        body_started: "Claude started working",
        body_completed: "Completed \u{2014} ready for review",
        body_failed: "Failed",
        body_test_passed: "Auto-test passed \u{2014} approved",
        body_test_failed: "Auto-test failed \u{2014} revision needed",
        body_revision: "Revision feedback sent to Claude",
        body_queue: "Auto-started from queue",
        body_blocker: "Claude is blocked and needs an answer",
        body_unknown_error: "unknown error",
    }
}

// ─── TaskNotification info ───

pub struct TaskNotification<'a> {
    pub title: &'a str,
    pub task_key: Option<&'a str>,
}

impl<'a> TaskNotification<'a> {
    pub fn new(title: &'a str, task_key: Option<&'a str>) -> Self {
        Self { title, task_key }
    }

    fn format_tag(&self) -> String {
        match self.task_key {
            Some(key) if !key.is_empty() => format!("[{}]", key),
            _ => String::new(),
        }
    }

    fn body_line(&self) -> String {
        let tag = self.format_tag();
        if tag.is_empty() {
            self.title.to_string()
        } else {
            format!("{} {}", tag, self.title)
        }
    }
}

// ─── Public API ───

pub fn notify_task_started(app: &AppHandle, info: &TaskNotification) {
    let db = db::get_db();
    let s = settings::get(&db);
    if !s.notify_task_started {
        return;
    }
    let i = strings(&s.language);
    let body = format!("{}\n\u{25B6} {}", info.body_line(), i.body_started);
    send(
        app,
        &format!("Claude Board \u{2014} {}", i.task_started),
        &body,
    );
}

pub fn notify_task_completed(app: &AppHandle, info: &TaskNotification) {
    let db = db::get_db();
    let s = settings::get(&db);
    if !s.notify_task_completed {
        return;
    }
    let i = strings(&s.language);
    let body = format!("{}\n\u{2714} {}", info.body_line(), i.body_completed);
    send(
        app,
        &format!("Claude Board \u{2014} {}", i.task_completed),
        &body,
    );
}

pub fn notify_task_failed(app: &AppHandle, info: &TaskNotification, reason: &str) {
    let db = db::get_db();
    let s = settings::get(&db);
    if !s.notify_task_failed {
        return;
    }
    let i = strings(&s.language);
    let detail = if reason.is_empty() {
        i.body_unknown_error.to_string()
    } else {
        reason.to_string()
    };
    let body = format!(
        "{}\n\u{2718} {}: {}",
        info.body_line(),
        i.body_failed,
        detail
    );
    send(
        app,
        &format!("Claude Board \u{2014} {}", i.task_failed),
        &body,
    );
}

pub fn notify_revision_requested(app: &AppHandle, info: &TaskNotification) {
    let db = db::get_db();
    let s = settings::get(&db);
    if !s.notify_revision_requested {
        return;
    }
    let i = strings(&s.language);
    let body = format!("{}\n\u{21BB} {}", info.body_line(), i.body_revision);
    send(
        app,
        &format!("Claude Board \u{2014} {}", i.revision_requested),
        &body,
    );
}

pub fn notify_queue_started(app: &AppHandle, info: &TaskNotification) {
    let db = db::get_db();
    let s = settings::get(&db);
    if !s.notify_queue_started {
        return;
    }
    let i = strings(&s.language);
    let body = format!("{}\n\u{23F5} {}", info.body_line(), i.body_queue);
    send(
        app,
        &format!("Claude Board \u{2014} {}", i.queue_started),
        &body,
    );
}

pub fn notify_test_passed(app: &AppHandle, info: &TaskNotification) {
    let db = db::get_db();
    let s = settings::get(&db);
    if !s.notify_task_completed {
        return;
    }
    let i = strings(&s.language);
    let body = format!("{}\n\u{2714} {}", info.body_line(), i.body_test_passed);
    send(
        app,
        &format!("Claude Board \u{2014} {}", i.test_passed),
        &body,
    );
}

pub fn notify_test_failed(app: &AppHandle, info: &TaskNotification) {
    let db = db::get_db();
    let s = settings::get(&db);
    if !s.notify_task_failed {
        return;
    }
    let i = strings(&s.language);
    let body = format!("{}\n\u{2718} {}", info.body_line(), i.body_test_failed);
    send(
        app,
        &format!("Claude Board \u{2014} {}", i.test_failed),
        &body,
    );
}

/// An agent stopped to ask something and is waiting on an answer.
///
/// Carries the question itself rather than a generic line: the point of the
/// notification is that the user can decide from it whether to go and answer now.
pub fn notify_blocker_raised(app: &AppHandle, info: &TaskNotification, question: &str) {
    let db = db::get_db();
    let s = settings::get(&db);
    if !s.notify_blocker_raised {
        return;
    }
    let i = strings(&s.language);
    let asked = question.trim();
    let detail = if asked.is_empty() {
        i.body_blocker
    } else {
        asked
    };
    let body = format!("{}\n\u{2753} {}", info.body_line(), detail);
    send(
        app,
        &format!("Claude Board \u{2014} {}", i.blocker_raised),
        &body,
    );
}

// ─── Internal ───

fn send(app: &AppHandle, title: &str, body: &str) {
    let mut builder = app.notification().builder().title(title).body(body);

    // Set app icon for the notification
    if let Ok(resource_dir) = app.path().resource_dir() {
        let icon_path: std::path::PathBuf = resource_dir.join("icons").join("32x32.png");
        if icon_path.exists() {
            builder = builder.icon(icon_path.to_string_lossy().into_owned());
        }
    }

    builder.show().ok();
}
