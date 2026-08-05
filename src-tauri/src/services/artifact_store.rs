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

/// SHA-256 of a document's content, hex encoded.
///
/// Persisted on the artifact row, so it has to be stable across builds — which
/// rules out `DefaultHasher`, whose output is explicitly not guaranteed to be
/// consistent between Rust versions.
pub fn content_hash(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

/// Seconds since the Unix epoch, for stamping a new artifact's filename.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Kebab-case a title, for a filename that reads like the document.
fn slug_of(title: &str) -> String {
    let slug: String = title
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

/// A readable filename: the document's title in kebab-case, plus the Unix second
/// it was saved.
///
/// Uploads use an opaque UUID (`attachments.rs`), which is right for arbitrary
/// binaries. An artifact's path is handed to agents and browsed by hand, so it
/// earns a name you can recognise — and now that the title is given rather than
/// guessed, the filename finally matches what the document is called.
pub fn stored_name(title: &str, unix_secs: i64) -> String {
    format!("{}-{}.md", slug_of(title), unix_secs)
}

/// A `stored_name` that is not already taken on disk.
///
/// Two documents sharing a title *and* saved in the same second would otherwise
/// collide, and one would overwrite the other. Resolved rather than hoped away.
/// The result is persisted on the artifact row, so it stays stable afterwards.
pub fn unique_stored_name(data_dir: &str, title: &str, unix_secs: i64) -> String {
    let base = slug_of(title);
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

// ─── Metadata ───────────────────────────────────────────────────────────────

/// Index metadata for a document whose title, kind and tags the caller supplies.
///
/// Only the preview is derived. Title and kind used to be guessed from the prose —
/// first H1, then front matter, then the filename — which meant a document that
/// opened with a paragraph got a title made out of its filename while the agent
/// that wrote it knew the real one. They are parameters now. A preview is a
/// display convenience rather than identity, so deriving it costs nothing.
pub fn meta_for(title: &str, kind: &str, content: &str) -> DerivedMeta {
    let (_, preview) = crate::services::artifacts::title_and_preview(content);
    DerivedMeta {
        title: Some(title.trim().to_string()).filter(|t| !t.is_empty()),
        preview,
        kind: kind.trim().to_string(),
        size: content.len() as i64,
    }
}

/// Bring the index back in line with what is on disk.
///
/// Explicit saves write the file and the row together, so drift only happens when
/// a stored document is edited by hand outside the app. This is the repair path
/// for that, not a step in the task lifecycle.
pub fn refresh_from_disk(db: &DbPool, project_id: i64, data_dir: &str) -> usize {
    let mut refreshed = 0;

    for artifact in db::artifacts::list_for_project(db, project_id) {
        let Ok(on_disk) = read(data_dir, &artifact.stored_name) else {
            // A missing file is the repair pass's problem, not this one's.
            continue;
        };
        // The title and kind are the user's or the agent's, never re-derived; only
        // what actually follows from the bytes is refreshed.
        let (_, preview) = crate::services::artifacts::title_and_preview(&on_disk);
        let size = on_disk.len() as i64;
        if artifact.preview == preview && artifact.size == size {
            continue;
        }

        if let Err(e) = db::artifacts::update_meta(
            db,
            artifact.id,
            None,
            None,
            None,
            Some(&preview),
            Some(size),
            None,
        ) {
            log::error!(
                "artifact refresh: updating {} failed: {}",
                artifact.stored_name,
                e
            );
            continue;
        }
        refreshed += 1;
    }

    if refreshed > 0 {
        log::info!(
            "Refreshed {} artifact(s) from disk for project {}",
            refreshed,
            project_id
        );
    }
    refreshed
}

// ─── Explicit saves ─────────────────────────────────────────────────────────

/// The kinds an artifact may be classified as.
pub const KINDS: [&str; 6] = ["plan", "rfc", "spec", "readme", "doc", "other"];

/// A saved document, as reported back to whoever saved it.
pub struct SavedArtifact {
    pub id: i64,
    pub stored_name: String,
    pub path: String,
}

fn normalise_kind(kind: &str) -> String {
    let lower = kind.trim().to_lowercase();
    if KINDS.contains(&lower.as_str()) {
        lower
    } else {
        // An unrecognised kind becomes "other" rather than an error: the document
        // matters more than its label, and the label is easy to correct.
        "other".to_string()
    }
}

/// Tags as the JSON array `tasks.tags` uses, so the same frontend helpers read both.
fn tags_json(tags: &[String]) -> String {
    let cleaned: Vec<&str> = tags
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .collect();
    serde_json::to_string(&cleaned).unwrap_or_else(|_| "[]".to_string())
}

/// Save a new document: write the file, then index it.
///
/// Title and kind are the caller's, never inferred. The filename comes from the
/// title, so a stored document is recognisable on disk.
#[allow(clippy::too_many_arguments)]
pub fn save(
    db: &DbPool,
    data_dir: &str,
    project_id: i64,
    title: &str,
    kind: &str,
    content: &str,
    tags: &[String],
    task_id: Option<i64>,
) -> Result<SavedArtifact, String> {
    if title.trim().is_empty() {
        return Err("An artifact needs a title".into());
    }

    let kind = normalise_kind(kind);
    let meta = meta_for(title, &kind, content);
    let name = unique_stored_name(data_dir, title, now_unix());

    // File first: a document on disk with no row is recoverable by the repair
    // pass, while a row with no document shows something that cannot be opened.
    write(data_dir, &name, content)?;
    let id = db::artifacts::create(db, project_id, &name, &meta, &tags_json(tags), task_id)
        .map_err(|e| e.to_string())?;

    let path = resolve(data_dir, &name)?.to_string_lossy().to_string();
    log::info!("Saved artifact {} ({}) as {}", id, kind, name);
    Ok(SavedArtifact {
        id,
        stored_name: name,
        path,
    })
}

/// Revise a saved document. Only the fields supplied change.
#[allow(clippy::too_many_arguments)]
pub fn revise(
    db: &DbPool,
    data_dir: &str,
    id: i64,
    title: Option<&str>,
    kind: Option<&str>,
    content: Option<&str>,
    tags: Option<&[String]>,
    task_id: Option<i64>,
) -> Result<SavedArtifact, String> {
    let artifact = db::artifacts::get(db, id).ok_or("Artifact not found")?;

    // Only recompute preview and size when the body actually changed; a call that
    // only retags a document must not touch them.
    let (preview, size) = match content {
        Some(body) => {
            write(data_dir, &artifact.stored_name, body)?;
            let derived = meta_for(
                title.unwrap_or_else(|| artifact.title.as_deref().unwrap_or_default()),
                kind.unwrap_or(&artifact.kind),
                body,
            );
            (Some(derived.preview), Some(derived.size))
        }
        None => (None, None),
    };

    let normalised_kind = kind.map(normalise_kind);
    let tags_encoded = tags.map(tags_json);
    db::artifacts::update_meta(
        db,
        id,
        title.map(|t| t.trim()).filter(|t| !t.is_empty()),
        normalised_kind.as_deref(),
        tags_encoded.as_deref(),
        preview.as_deref(),
        size,
        task_id,
    )
    .map_err(|e| e.to_string())?;

    let path = resolve(data_dir, &artifact.stored_name)?
        .to_string_lossy()
        .to_string();
    Ok(SavedArtifact {
        id,
        stored_name: artifact.stored_name,
        path,
    })
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
        // The title is given now, so the filename matches what the document is
        // actually called instead of a kebab-cased source path.
        let name = stored_name("Auth rollout plan", 1_754_400_000);
        assert_eq!(name, "auth-rollout-plan-1754400000.md");
        assert!(!name.contains('/') && !name.contains('\\'), "got {}", name);
    }

    #[test]
    fn the_slug_is_kebab_case_with_runs_collapsed() {
        assert_eq!(stored_name("My  Plan!! v2", 100), "my-plan-v2-100.md");
    }

    #[test]
    fn an_unusable_title_still_produces_a_valid_name() {
        assert_eq!(stored_name("___", 100), "artifact-100.md");
        assert_eq!(stored_name("Ünïcödé", 100), "n-c-d-100.md");
        assert_eq!(stored_name("", 100), "artifact-100.md");
    }

    #[test]
    fn the_same_title_in_the_same_second_does_not_collide() {
        let dir = tmp("collide");
        // Two documents can legitimately share a title; one must not overwrite
        // the other.
        let first = unique_stored_name(&dir, "Notes", 500);
        write(&dir, &first, "first\n").unwrap();
        let second = unique_stored_name(&dir, "Notes", 500);

        assert_eq!(first, "notes-500.md");
        assert_eq!(second, "notes-500-2.md");
    }

    #[test]
    fn a_free_name_is_used_as_is() {
        let dir = tmp("free");
        assert_eq!(unique_stored_name(&dir, "Plan", 700), "plan-700.md");
    }

    #[test]
    fn meta_takes_the_title_and_kind_it_is_given() {
        // Content with no heading at all: the old capture path would have fallen
        // back to a filename here, which is the flakiness this replaces.
        let meta = meta_for(
            "Auth rollout plan",
            "plan",
            "We start with the read path.\n",
        );

        assert_eq!(meta.title.as_deref(), Some("Auth rollout plan"));
        assert_eq!(meta.kind, "plan");
        assert!(
            meta.preview.contains("read path"),
            "preview is still derived"
        );
        assert_eq!(meta.size, "We start with the read path.\n".len() as i64);
    }

    #[test]
    fn an_empty_title_is_recorded_as_absent_rather_than_blank() {
        assert_eq!(meta_for("   ", "doc", "body").title, None);
    }

    // ─── Explicit saves ─────────────────────────────────────────────────────

    struct SaveEnv {
        db: DbPool,
        project_id: i64,
        task_id: i64,
        data_dir: String,
    }

    fn save_env(suffix: &str) -> SaveEnv {
        use rusqlite::Connection;
        use std::sync::Arc;
        let base = std::env::temp_dir().join(format!(
            "cb-artifact-save-{}-{}",
            std::process::id(),
            suffix
        ));
        std::fs::remove_dir_all(&base).ok();
        let data_dir = base.join("data");
        std::fs::create_dir_all(&data_dir).unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::create_tables(&conn);
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute(
            "INSERT INTO projects (name,slug,working_dir) VALUES ('B','b','/repo')",
            [],
        )
        .unwrap();
        let project_id = conn.last_insert_rowid();
        // A real task, because attribution is a foreign key: passing an id that
        // does not exist fails the constraint rather than silently recording it.
        conn.execute(
            "INSERT INTO tasks (project_id,title,status) VALUES (?1,'t','done')",
            rusqlite::params![project_id],
        )
        .unwrap();
        let task_id = conn.last_insert_rowid();
        SaveEnv {
            db: Arc::new(parking_lot::Mutex::new(conn)),
            project_id,
            task_id,
            data_dir: data_dir.to_string_lossy().to_string(),
        }
    }

    #[test]
    fn saving_uses_the_given_title_rather_than_guessing_at_the_content() {
        let e = save_env("explicit-title");

        // Content with no heading at all: the removed capture path would have
        // fallen back to a filename here, which is the flakiness this replaces.
        let saved = save(
            &e.db,
            &e.data_dir,
            e.project_id,
            "Auth rollout plan",
            "plan",
            "We start with the read path.\n",
            &["context".to_string()],
            Some(e.task_id),
        )
        .unwrap();

        let row = db::artifacts::get(&e.db, saved.id).unwrap();
        assert_eq!(row.title.as_deref(), Some("Auth rollout plan"));
        assert_eq!(row.kind, "plan");
        assert_eq!(row.tags.as_deref(), Some(r#"["context"]"#));
        assert!(
            row.preview.contains("read path"),
            "preview is still derived"
        );
        assert_eq!(row.size, "We start with the read path.\n".len() as i64);
    }

    #[test]
    fn the_stored_filename_comes_from_the_title() {
        let e = save_env("title-filename");
        let saved = save(
            &e.db,
            &e.data_dir,
            e.project_id,
            "Auth rollout plan",
            "plan",
            "body\n",
            &[],
            None,
        )
        .unwrap();

        assert!(
            saved.stored_name.starts_with("auth-rollout-plan-"),
            "got {}",
            saved.stored_name
        );
        assert!(saved.path.ends_with(&saved.stored_name));
        assert_eq!(read(&e.data_dir, &saved.stored_name).unwrap(), "body\n");
    }

    #[test]
    fn two_saves_with_the_same_title_are_two_documents() {
        let e = save_env("no-upsert");
        let first = save(
            &e.db,
            &e.data_dir,
            e.project_id,
            "Notes",
            "doc",
            "one\n",
            &[],
            None,
        )
        .unwrap();
        let second = save(
            &e.db,
            &e.data_dir,
            e.project_id,
            "Notes",
            "doc",
            "two\n",
            &[],
            None,
        )
        .unwrap();

        // Identity is the id. A caller that means to revise calls revise().
        assert_ne!(first.id, second.id);
        assert_ne!(first.stored_name, second.stored_name);
        assert_eq!(read(&e.data_dir, &first.stored_name).unwrap(), "one\n");
    }

    #[test]
    fn a_title_is_required() {
        let e = save_env("no-title");
        // Without a title there is nothing to name the file after, and nothing
        // for the user to recognise in the list.
        assert!(save(
            &e.db,
            &e.data_dir,
            e.project_id,
            "  ",
            "doc",
            "x",
            &[],
            None
        )
        .is_err());
    }

    #[test]
    fn an_unknown_kind_becomes_other_rather_than_failing() {
        let e = save_env("odd-kind");
        let saved = save(
            &e.db,
            &e.data_dir,
            e.project_id,
            "Notes",
            "runbook",
            "x",
            &[],
            None,
        )
        .unwrap();

        // The document matters more than its label, and a label is easy to fix.
        assert_eq!(db::artifacts::get(&e.db, saved.id).unwrap().kind, "other");
    }

    #[test]
    fn revising_the_body_leaves_the_classification_alone() {
        let e = save_env("partial-update");
        let saved = save(
            &e.db,
            &e.data_dir,
            e.project_id,
            "Plan",
            "plan",
            "one\n",
            &["context".to_string()],
            None,
        )
        .unwrap();

        revise(
            &e.db,
            &e.data_dir,
            saved.id,
            None,
            None,
            Some("two\n\nmore\n"),
            None,
            None,
        )
        .unwrap();

        let row = db::artifacts::get(&e.db, saved.id).unwrap();
        assert_eq!(row.title.as_deref(), Some("Plan"), "title untouched");
        assert_eq!(row.kind, "plan", "kind untouched");
        assert_eq!(
            row.tags.as_deref(),
            Some(r#"["context"]"#),
            "tags untouched"
        );
        assert!(row.preview.contains("more"), "preview re-derived");
        assert!(read(&e.data_dir, &row.stored_name).unwrap().contains("two"));
    }

    #[test]
    fn retagging_does_not_touch_the_body_or_its_preview() {
        let e = save_env("retag");
        let saved = save(
            &e.db,
            &e.data_dir,
            e.project_id,
            "Plan",
            "plan",
            "the body\n",
            &[],
            None,
        )
        .unwrap();
        let before = db::artifacts::get(&e.db, saved.id).unwrap();

        revise(
            &e.db,
            &e.data_dir,
            saved.id,
            None,
            None,
            None,
            Some(&["context".to_string(), "shared".to_string()]),
            None,
        )
        .unwrap();

        let after = db::artifacts::get(&e.db, saved.id).unwrap();
        assert_eq!(after.tags.as_deref(), Some(r#"["context","shared"]"#));
        assert_eq!(after.preview, before.preview, "body untouched");
        assert_eq!(after.size, before.size);
        assert_eq!(read(&e.data_dir, &after.stored_name).unwrap(), "the body\n");
    }

    #[test]
    fn revising_records_the_task_that_did_it() {
        let e = save_env("attribution");
        let saved = save(
            &e.db,
            &e.data_dir,
            e.project_id,
            "Plan",
            "plan",
            "one\n",
            &[],
            None,
        )
        .unwrap();

        revise(
            &e.db,
            &e.data_dir,
            saved.id,
            None,
            None,
            Some("two\n"),
            None,
            Some(e.task_id),
        )
        .unwrap();

        assert_eq!(
            db::artifacts::get(&e.db, saved.id).unwrap().last_task_id,
            Some(e.task_id)
        );
    }

    #[test]
    fn blank_tags_are_dropped_rather_than_stored() {
        let e = save_env("blank-tags");
        let saved = save(
            &e.db,
            &e.data_dir,
            e.project_id,
            "Plan",
            "plan",
            "x",
            &["  ".to_string(), "context".to_string(), String::new()],
            None,
        )
        .unwrap();

        assert_eq!(
            db::artifacts::get(&e.db, saved.id).unwrap().tags.as_deref(),
            Some(r#"["context"]"#)
        );
    }

    #[test]
    fn revising_a_missing_artifact_is_an_error() {
        let e = save_env("missing");
        assert!(revise(&e.db, &e.data_dir, 9999, None, None, Some("x"), None, None).is_err());
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
