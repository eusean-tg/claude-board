//! On-disk storage for captured artifacts.
//!
//! Documents live in a flat directory beside the app's data directory rather
//! than as blobs in SQLite, so an artifact can be referenced by absolute path:
//! an agent handed a path can read the document itself, and the prompt stays
//! small.
//!
//! Filenames are readable — a kebab-case slug of the document's own name plus
//! the Unix second it was first captured — because these paths are handed to
//! agents and browsed by hand. They are still generated here and never taken
//! from input: `resolve` treats every name arriving from the frontend as hostile
//! and refuses anything that is not a single normal path component, which is
//! simpler to reason about than canonicalising and comparing prefixes, and
//! cannot be defeated by a symlink.

use crate::db::{self, artifacts::DerivedMeta, DbPool};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The directory holding every stored document.
///
/// A sibling of the data directory rather than a child of it, matching how
/// uploads are laid out (`attachments.rs` and `runner.rs` both build
/// `data_dir.parent()/uploads`). The data directory holds the SQLite database;
/// user content sits beside it. For a data dir of `~/.claudeboard/data` this is
/// `~/.claudeboard/artifacts`.
pub fn root(data_dir: &str) -> PathBuf {
    let data = Path::new(data_dir);
    data.parent().unwrap_or(data).join("artifacts")
}

/// Seconds since the Unix epoch, for stamping a new artifact's filename.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Kebab-case the document's basename, for a filename that reads like the doc.
fn slug_of(source_rel_path: &str) -> String {
    let stem = Path::new(source_rel_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("artifact");
    let slug: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    // Collapse runs so "my  plan!!.md" does not become "my--plan--".
    let mut collapsed = String::with_capacity(slug.len());
    for c in slug.chars() {
        if c == '-' && collapsed.ends_with('-') {
            continue;
        }
        collapsed.push(c);
    }
    let trimmed = collapsed.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "artifact".to_string()
    } else {
        trimmed
    }
}

/// A readable filename: the document's name in kebab-case, plus the Unix second
/// it was first captured.
///
/// Uploads use an opaque UUID (`attachments.rs`), which is right for arbitrary
/// binaries. An artifact's path is handed to agents and browsed by hand, so it
/// earns a name you can recognise. The timestamp is what keeps two documents
/// with the same basename — `docs/a/notes.md` and `docs/b/notes.md` — apart.
pub fn stored_name(source_rel_path: &str, unix_secs: i64) -> String {
    format!("{}-{}.md", slug_of(source_rel_path), unix_secs)
}

/// A `stored_name` that is not already taken on disk.
///
/// Two different documents sharing a basename *and* captured in the same second
/// would otherwise collide. Rare, but a collision would have one document
/// overwrite the other, so it is resolved rather than hoped away. The result is
/// persisted on the artifact row, so it stays stable afterwards.
pub fn unique_stored_name(data_dir: &str, source_rel_path: &str, unix_secs: i64) -> String {
    let base = slug_of(source_rel_path);
    let first = format!("{}-{}.md", base, unix_secs);
    if !root(data_dir).join(&first).exists() {
        return first;
    }
    for n in 2..1000 {
        let candidate = format!("{}-{}-{}.md", base, unix_secs, n);
        if !root(data_dir).join(&candidate).exists() {
            return candidate;
        }
    }
    // A thousand same-named documents in one second is not a real scenario, but
    // silently overwriting one would be worse than an ugly name.
    format!("{}-{}-{}.md", base, unix_secs, uuid::Uuid::new_v4())
}

/// Resolve a stored name to an absolute path inside the store, or refuse.
///
/// `stored_name` reaches this function from the frontend, so it is untrusted.
/// Only a single path component is accepted; a separator, a parent component or
/// an absolute path is rejected outright rather than sanitised.
pub fn resolve(data_dir: &str, stored_name: &str) -> Result<PathBuf, String> {
    // Counting components is not enough on its own: `..` is a single component,
    // so it would pass a count check and then escape the root. The component has
    // to be a Normal one. Backslashes are rejected explicitly because they are
    // not separators on Unix, so `..\..\x.md` would otherwise read as one Normal
    // component here and as a traversal on Windows.
    let mut components = Path::new(stored_name).components();
    let single_normal = matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(_)), None)
    );
    if stored_name.is_empty()
        || stored_name.contains('/')
        || stored_name.contains('\\')
        || !single_normal
    {
        return Err(format!("invalid artifact name: {}", stored_name));
    }
    Ok(root(data_dir).join(stored_name))
}

/// Write a document, creating the store root on first use.
pub fn write(data_dir: &str, stored_name: &str, content: &str) -> Result<(), String> {
    let path = resolve(data_dir, stored_name)?;
    let dir = root(data_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {:?}: {}", dir, e))?;
    std::fs::write(&path, content).map_err(|e| format!("could not write {:?}: {}", path, e))
}

pub fn read(data_dir: &str, stored_name: &str) -> Result<String, String> {
    let path = resolve(data_dir, stored_name)?;
    std::fs::read_to_string(&path).map_err(|e| format!("could not read {:?}: {}", path, e))
}

/// Delete a document. A file that is already gone is not an error, so a
/// half-deleted artifact can still be cleared.
pub fn remove(data_dir: &str, stored_name: &str) -> Result<(), String> {
    let path = resolve(data_dir, stored_name)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("could not remove {:?}: {}", path, e)),
    }
}

// ─── Capture ────────────────────────────────────────────────────────────────

/// Tool calls that create or modify a file. Read-only tools are ignored.
const WRITE_TOOLS: [&str; 4] = ["Write", "Edit", "MultiEdit", "NotebookEdit"];

const MARKDOWN_EXTENSIONS: [&str; 2] = ["md", "mdx"];

/// Markdown paths an agent has written, per task, awaiting capture.
///
/// A set rather than a list: a document edited ten times should be copied once,
/// at whatever state it ends up in. The paths are read from disk at flush time
/// because the logged tool-call `content` is truncated to 500 characters and
/// never holds the whole document.
static PENDING: once_cell::sync::Lazy<Mutex<HashMap<i64, HashSet<String>>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashMap::new()));

/// True when a tool call wrote a markdown file.
pub fn is_markdown_write(tool_name: &str, file_path: &str) -> bool {
    if !WRITE_TOOLS.contains(&tool_name) {
        return false;
    }
    Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| MARKDOWN_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Remember that `abs_path` was written by `task_id`, for capture at flush time.
pub fn note_markdown_write(task_id: i64, abs_path: &str) {
    PENDING
        .lock()
        .entry(task_id)
        .or_default()
        .insert(abs_path.to_string());
}

/// Normalise a written path to one that identifies the document across tasks.
///
/// Agents run in `<working_dir>/.worktrees/task-<id>`, so the same document
/// written by two tasks arrives under two different absolute paths. Stripping the
/// worktree prefix makes both map to one repo-relative key, which is what keeps
/// the artifact index at one row per document.
///
/// Returns `None` for anything outside the project — agents write scratch files
/// in temp directories, and those are not project documents.
pub fn source_rel_path(written: &str, working_dir: &str, worktree_dir: &str) -> Option<String> {
    let norm = |p: &str| p.replace('\\', "/").trim_end_matches('/').to_string();
    let path = norm(written.trim());
    if path.is_empty() {
        return None;
    }

    // The worktree lives *inside* the working dir, so it has to be tried first
    // or every capture would come back as ".worktrees/task-N/docs/plan.md".
    let candidates = [norm(worktree_dir), norm(working_dir)];
    let mut rel: Option<String> = None;
    for base in candidates.iter().filter(|b| !b.is_empty()) {
        if let Some(stripped) = path.strip_prefix(base.as_str()) {
            let stripped = stripped.trim_start_matches('/');
            if !stripped.is_empty() {
                rel = Some(stripped.to_string());
                break;
            }
        }
    }
    // A relative path was already written relative to the agent's cwd.
    let rel = rel.or_else(|| {
        if Path::new(&path).is_absolute() {
            None
        } else {
            Some(path.trim_start_matches("./").to_string())
        }
    })?;

    if rel.is_empty() || rel.split('/').any(|part| part == "..") {
        return None;
    }
    Some(rel)
}

/// Derive the index metadata for a document from its content and path.
pub fn derive_meta(source_rel_path: &str, content: &str) -> DerivedMeta {
    let (title, preview) = crate::services::artifacts::title_and_preview(content);
    DerivedMeta {
        title,
        preview,
        kind: crate::services::artifacts::classify(source_rel_path).to_string(),
        size: content.len() as i64,
    }
}

/// Copy every markdown file `task_id` wrote into the store and index it.
///
/// Must run before the task's worktree is removed: `cleanup_task_branch` deletes
/// it as its first act, and these files live inside it.
///
/// Best-effort by design. A file the agent wrote and then deleted, or one that
/// cannot be read, is skipped — an artifact is a convenience and must never fail
/// a task that otherwise succeeded.
pub fn flush_captures(
    db: &DbPool,
    task_id: i64,
    project_id: i64,
    working_dir: &str,
    worktree_dir: &str,
    data_dir: &str,
) -> usize {
    let paths = PENDING.lock().remove(&task_id).unwrap_or_default();
    let mut stored = 0;

    for written in paths {
        let Some(rel) = source_rel_path(&written, working_dir, worktree_dir) else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(&written) else {
            // Written then deleted, or unreadable. Nothing to store.
            continue;
        };

        let meta = derive_meta(&rel, &content);

        // A document that is already indexed keeps its filename, so a path
        // already handed to an agent stays valid across re-captures.
        let name = db::artifacts::find_by_source(db, project_id, &rel)
            .map(|a| a.stored_name)
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| unique_stored_name(data_dir, &rel, now_unix()));

        // Index first: a row whose file is missing is repairable, while a file
        // with no row is invisible.
        if let Err(e) =
            db::artifacts::insert_or_replace(db, project_id, &rel, &name, &meta, task_id)
        {
            log::error!("artifact capture: indexing {} failed: {}", rel, e);
            continue;
        }
        if let Err(e) = write(data_dir, &name, &content) {
            log::error!("artifact capture: writing {} failed: {}", name, e);
            continue;
        }
        stored += 1;
    }

    if stored > 0 {
        log::info!("Captured {} artifact(s) for task {}", stored, task_id);
    }
    stored
}

/// Drop any pending captures for a task without storing them.
pub fn discard_captures(task_id: i64) {
    PENDING.lock().remove(&task_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway data dir. `suffix` keeps concurrently running tests out of
    /// each other's directories.
    ///
    /// Returns a `data` subdirectory, not the base: `root()` resolves to the data
    /// dir's *parent*, so handing back the base would put every test's store in
    /// one shared directory outside the fixture.
    fn tmp(suffix: &str) -> String {
        let base = std::env::temp_dir().join(format!(
            "cb-artifact-store-{}-{}",
            std::process::id(),
            suffix
        ));
        std::fs::remove_dir_all(&base).ok();
        let data = base.join("data");
        std::fs::create_dir_all(&data).unwrap();
        data.to_string_lossy().to_string()
    }

    #[test]
    fn stored_names_are_flat_readable_and_timestamped() {
        let name = stored_name("docs/plans/auth-plan.md", 1_754_400_000);
        assert_eq!(name, "auth-plan-1754400000.md");
        assert!(!name.contains('/') && !name.contains('\\'), "got {}", name);
    }

    #[test]
    fn the_slug_is_kebab_case_with_runs_collapsed() {
        // A name a person reads, not a UUID: punctuation becomes single dashes.
        assert_eq!(
            stored_name("docs/My  Plan!! v2.md", 100),
            "my-plan-v2-100.md"
        );
    }

    #[test]
    fn an_unusable_basename_still_produces_a_valid_name() {
        // A punctuation-only stem must not collapse to "-100.md".
        assert_eq!(stored_name("docs/___.md", 100), "artifact-100.md");
        // Non-ASCII characters become separators, trimmed rather than dangling.
        assert_eq!(stored_name("docs/Ünïcödé.md", 100), "n-c-d-100.md");
        // No basename at all still resolves to something writable.
        assert_eq!(stored_name("", 100), "artifact-100.md");
    }

    #[test]
    fn same_basename_in_the_same_second_does_not_collide() {
        let dir = tmp("collide");
        // docs/a/notes.md and docs/b/notes.md captured in the same second would
        // otherwise have one silently overwrite the other.
        let first = unique_stored_name(&dir, "docs/a/notes.md", 500);
        write(&dir, &first, "first\n").unwrap();
        let second = unique_stored_name(&dir, "docs/b/notes.md", 500);

        assert_eq!(first, "notes-500.md");
        assert_eq!(second, "notes-500-2.md");
    }

    #[test]
    fn a_free_name_is_used_as_is() {
        let dir = tmp("free");
        assert_eq!(unique_stored_name(&dir, "docs/plan.md", 700), "plan-700.md");
    }

    #[test]
    fn the_store_root_sits_beside_the_data_dir_not_inside_it() {
        // Matches uploads, which live at data_dir.parent()/uploads.
        assert_eq!(
            root("/Users/x/.claudeboard/data"),
            PathBuf::from("/Users/x/.claudeboard/artifacts")
        );
    }

    #[test]
    fn resolve_refuses_to_escape_the_store_root() {
        let dir = tmp("escape");
        // The frontend supplies stored_name; treat it as hostile.
        for bad in [
            "../../etc/passwd",
            "/etc/passwd",
            "..\\..\\secrets.md",
            "..",
            "",
            "sub/dir.md",
        ] {
            assert!(resolve(&dir, bad).is_err(), "{} should be rejected", bad);
        }
    }

    #[test]
    fn resolve_accepts_a_generated_name() {
        let dir = tmp("accept");
        let name = stored_name("docs/x.md", 1);
        let path = resolve(&dir, &name).unwrap();
        assert!(path.starts_with(root(&dir)));
    }

    #[test]
    fn write_creates_the_root_and_read_returns_the_content() {
        let dir = tmp("roundtrip");
        let name = stored_name("docs/plan.md", 7);

        write(&dir, &name, "# Plan\n\nbody\n").unwrap();

        assert!(
            root(&dir).is_dir(),
            "the store root is created on first write"
        );
        assert_eq!(read(&dir, &name).unwrap(), "# Plan\n\nbody\n");
    }

    #[test]
    fn write_overwrites_an_existing_document() {
        let dir = tmp("overwrite");
        let name = stored_name("docs/plan.md", 8);
        write(&dir, &name, "old\n").unwrap();

        write(&dir, &name, "new\n").unwrap();

        assert_eq!(read(&dir, &name).unwrap(), "new\n");
    }

    #[test]
    fn removing_a_document_twice_is_not_an_error() {
        let dir = tmp("remove");
        let name = stored_name("docs/gone.md", 9);
        write(&dir, &name, "x\n").unwrap();

        remove(&dir, &name).unwrap();
        // A half-deleted artifact must still be clearable.
        remove(&dir, &name).unwrap();

        assert!(read(&dir, &name).is_err());
    }
}

#[cfg(test)]
mod capture_tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::Arc;

    struct Env {
        db: DbPool,
        project_id: i64,
        task_id: i64,
        working_dir: String,
        worktree: PathBuf,
        worktree_str: String,
        data_dir: String,
    }

    /// A repo-shaped fixture: a working dir with a worktree inside it, the way
    /// `ensure_task_worktree` lays them out.
    fn env(suffix: &str, task_id: i64) -> Env {
        let base =
            std::env::temp_dir().join(format!("cb-capture-{}-{}", std::process::id(), suffix));
        std::fs::remove_dir_all(&base).ok();
        let working_dir = base.join("repo");
        let worktree = working_dir
            .join(".worktrees")
            .join(format!("task-{}", task_id));
        let data_dir = base.join("data");
        std::fs::create_dir_all(worktree.join("docs")).unwrap();
        std::fs::create_dir_all(&data_dir).unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::create_tables(&conn);
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute(
            "INSERT INTO projects (name,slug,working_dir) VALUES ('B','b',?1)",
            rusqlite::params![working_dir.to_string_lossy()],
        )
        .unwrap();
        let project_id = conn.last_insert_rowid();
        conn.execute(
            // Explicit id, not autoincrement. Every test builds a fresh in-memory
            // database, so autoincrement hands out 1 to all of them — and PENDING
            // is process-global and keyed by task id, so tests running in parallel
            // would share and clobber each other's pending captures.
            "INSERT INTO tasks (id,project_id,title,status) VALUES (?1,?2,'t','in_progress')",
            rusqlite::params![task_id, project_id],
        )
        .unwrap();
        let real_task_id = task_id;

        Env {
            db: Arc::new(Mutex::new(conn)),
            project_id,
            task_id: real_task_id,
            working_dir: working_dir.to_string_lossy().to_string(),
            worktree_str: worktree.to_string_lossy().to_string(),
            worktree,
            data_dir: data_dir.to_string_lossy().to_string(),
        }
    }

    impl Env {
        fn flush(&self) -> usize {
            flush_captures(
                &self.db,
                self.task_id,
                self.project_id,
                &self.working_dir,
                &self.worktree_str,
                &self.data_dir,
            )
        }
    }

    #[test]
    fn only_markdown_writes_are_captured() {
        assert!(is_markdown_write("Write", "/repo/docs/plan.md"));
        assert!(is_markdown_write("Edit", "/repo/docs/plan.MDX"));
        assert!(is_markdown_write("MultiEdit", "/repo/a.md"));
        // Reads are not writes, and code is not a document.
        assert!(!is_markdown_write("Read", "/repo/docs/plan.md"));
        assert!(!is_markdown_write("Write", "/repo/src/main.rs"));
        assert!(!is_markdown_write("Write", "/repo/README"));
    }

    #[test]
    fn a_worktree_path_normalises_to_a_repo_relative_path() {
        // The same document written by two tasks must map to one key, or the
        // index gets a row per task instead of a row per document.
        let a = source_rel_path(
            "/repo/.worktrees/task-7/docs/plan.md",
            "/repo",
            "/repo/.worktrees/task-7",
        );
        let b = source_rel_path(
            "/repo/.worktrees/task-9/docs/plan.md",
            "/repo",
            "/repo/.worktrees/task-9",
        );
        assert_eq!(a.as_deref(), Some("docs/plan.md"));
        assert_eq!(a, b);
    }

    #[test]
    fn a_path_in_the_working_dir_itself_is_captured() {
        // auto_branch off means no worktree; the agent writes in the repo.
        let rel = source_rel_path("/repo/docs/plan.md", "/repo", "");
        assert_eq!(rel.as_deref(), Some("docs/plan.md"));
    }

    #[test]
    fn a_path_outside_the_project_is_not_captured() {
        // Agents write scratch files in temp dirs; those are not documents.
        assert!(source_rel_path("/tmp/scratch.md", "/repo", "/repo/.worktrees/task-1").is_none());
        assert!(source_rel_path("/elsewhere/notes.md", "/repo", "").is_none());
        assert!(source_rel_path("", "/repo", "").is_none());
    }

    #[test]
    fn a_traversal_in_a_relative_path_is_refused() {
        assert!(source_rel_path("../outside.md", "/repo", "").is_none());
        assert!(source_rel_path("docs/../../outside.md", "/repo", "").is_none());
    }

    #[test]
    fn capture_copies_the_file_and_survives_worktree_removal() {
        let e = env("survives", 1);
        let file = e.worktree.join("docs/plan.md");
        std::fs::write(&file, "# Auth plan\n\nDetails here.\n").unwrap();
        note_markdown_write(e.task_id, file.to_str().unwrap());

        assert_eq!(e.flush(), 1);
        // The whole point of copying rather than referencing in place.
        std::fs::remove_dir_all(&e.worktree).unwrap();

        let rows = db::artifacts::list_for_project(&e.db, e.project_id);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source_rel_path, "docs/plan.md");
        assert_eq!(rows[0].title.as_deref(), Some("Auth plan"));
        assert_eq!(rows[0].kind, "plan");
        assert_eq!(rows[0].last_task_id, Some(e.task_id));
        assert!(!rows[0].stored_name.is_empty(), "a name must be recorded");
        let content = read(&e.data_dir, &rows[0].stored_name).unwrap();
        assert!(content.contains("Details here."));
    }

    #[test]
    fn a_missing_file_is_skipped_without_failing_the_flush() {
        let e = env("missing", 2);
        note_markdown_write(e.task_id, e.worktree.join("docs/gone.md").to_str().unwrap());

        // Written and then deleted again. Capture is a convenience, not a
        // contract, and must not fail a task that otherwise succeeded.
        assert_eq!(e.flush(), 0);
        assert!(db::artifacts::list_for_project(&e.db, e.project_id).is_empty());
    }

    #[test]
    fn re_capturing_keeps_the_filename_so_references_stay_valid() {
        let e = env("rename", 3);
        let file = e.worktree.join("docs/plan.md");
        std::fs::write(&file, "# One\n").unwrap();
        note_markdown_write(e.task_id, file.to_str().unwrap());
        e.flush();
        let first = db::artifacts::list_for_project(&e.db, e.project_id)[0].clone();

        std::fs::write(&file, "# Two\n\nmore\n").unwrap();
        note_markdown_write(e.task_id, file.to_str().unwrap());
        e.flush();

        let rows = db::artifacts::list_for_project(&e.db, e.project_id);
        assert_eq!(rows.len(), 1, "still one document");
        assert_eq!(
            rows[0].stored_name, first.stored_name,
            "the path an agent was given must keep working"
        );
        assert_eq!(rows[0].title.as_deref(), Some("Two"));
        assert!(read(&e.data_dir, &rows[0].stored_name)
            .unwrap()
            .contains("more"));
    }

    #[test]
    fn a_document_edited_many_times_is_stored_once() {
        let e = env("dedupe", 4);
        let file = e.worktree.join("docs/plan.md");
        std::fs::write(&file, "# Plan\n").unwrap();
        for _ in 0..5 {
            note_markdown_write(e.task_id, file.to_str().unwrap());
        }

        assert_eq!(e.flush(), 1, "five edits, one copy");
    }

    #[test]
    fn flushing_twice_does_not_re_store_anything() {
        let e = env("drain", 5);
        let file = e.worktree.join("docs/plan.md");
        std::fs::write(&file, "# Plan\n").unwrap();
        note_markdown_write(e.task_id, file.to_str().unwrap());

        assert_eq!(e.flush(), 1);
        // The pending set is drained, so a second phase of the same task does
        // not copy files that may no longer exist.
        assert_eq!(e.flush(), 0);
    }

    #[test]
    fn discarding_pending_captures_stores_nothing() {
        let e = env("discard", 6);
        let file = e.worktree.join("docs/plan.md");
        std::fs::write(&file, "# Plan\n").unwrap();
        note_markdown_write(e.task_id, file.to_str().unwrap());

        discard_captures(e.task_id);

        assert_eq!(e.flush(), 0);
    }
}
