//! Commands backing the project Artifacts tab.
//!
//! Artifacts are the markdown documents agents wrote, captured into the store as
//! they were written (`crate::services::artifact_store`) and indexed in the
//! `artifacts` table (`crate::db::artifacts`). These commands read that index and
//! the stored copies — never the project's working directory.
//!
//! Editing or deleting an artifact touches only the store. The file the agent
//! originally wrote is still in the repository, under version control, and is
//! left alone.

use crate::db::{self, artifacts::StoredArtifact};
use crate::services::artifact_store;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// An artifact's metadata plus its full content.
#[derive(serde::Serialize)]
pub struct ArtifactContent {
    #[serde(flatten)]
    pub artifact: StoredArtifact,
    pub content: String,
}

/// What an agent needs to read a referenced document: where it is on disk.
#[derive(serde::Serialize)]
pub struct ArtifactReference {
    pub id: i64,
    pub title: Option<String>,
    pub kind: String,
    pub path: String,
}

fn data_dir() -> String {
    db::get_data_dir().to_string_lossy().to_string()
}

/// Every artifact captured for a project, most recently updated first.
#[tauri::command]
pub fn list_artifacts(project_id: i64) -> Result<Vec<StoredArtifact>, String> {
    let db = db::get_db();
    Ok(db::artifacts::list_for_project(&db, project_id))
}

/// Read one artifact's stored content.
#[tauri::command]
pub fn get_artifact(id: i64) -> Result<ArtifactContent, String> {
    let db = db::get_db();
    let artifact = db::artifacts::get(&db, id).ok_or("Artifact not found")?;
    let content = artifact_store::read(&data_dir(), &artifact.stored_name)?;
    Ok(ArtifactContent { artifact, content })
}

/// Overwrite an artifact with edited content and refresh its derived metadata.
#[tauri::command]
pub fn update_artifact(id: i64, content: String) -> Result<StoredArtifact, String> {
    let db = db::get_db();
    update_artifact_in(&db, &data_dir(), id, &content)
}

/// Delete an artifact: the index row and the stored copy.
///
/// The project's own file is untouched — the store holds a copy, so deleting from
/// it must never remove the user's document from their repository.
#[tauri::command]
pub fn delete_artifact(id: i64) -> Result<(), String> {
    let db = db::get_db();
    delete_artifact_in(&db, &data_dir(), id)
}

/// Relate a task to a document it should read or track progress against.
#[tauri::command]
pub fn add_artifact_ref(
    task_id: i64,
    artifact_id: i64,
    role: Option<String>,
) -> Result<Vec<StoredArtifact>, String> {
    let db = db::get_db();
    let role = role.unwrap_or_else(|| db::artifact_refs::ROLE_REFERENCE.to_string());
    db::artifact_refs::add_ref(&db, task_id, artifact_id, &role).map_err(|e| e.to_string())?;
    Ok(db::artifact_refs::artifacts_for_task(&db, task_id))
}

#[tauri::command]
pub fn remove_artifact_ref(task_id: i64, artifact_id: i64) -> Result<Vec<StoredArtifact>, String> {
    let db = db::get_db();
    db::artifact_refs::remove_ref(&db, task_id, artifact_id).map_err(|e| e.to_string())?;
    Ok(db::artifact_refs::artifacts_for_task(&db, task_id))
}

/// The documents a task references.
#[tauri::command]
pub fn task_artifacts(task_id: i64) -> Result<Vec<StoredArtifact>, String> {
    let db = db::get_db();
    Ok(db::artifact_refs::artifacts_for_task(&db, task_id))
}

/// Acknowledge that the repository has a newer version of this document.
///
/// Only clears the flag. The stored copy is untouched, which is the point: the
/// edits it holds exist nowhere else, while the repository version is still in
/// the repository under version control.
#[tauri::command]
pub fn dismiss_artifact_conflict(id: i64) -> Result<StoredArtifact, String> {
    let db = db::get_db();
    db::artifacts::clear_conflict(&db, id).map_err(|e| e.to_string())?;
    db::artifacts::get(&db, id).ok_or_else(|| "Artifact not found".to_string())
}

/// The absolute store path of an artifact, for referencing it from a task.
///
/// A path rather than the content: the agent reads the document itself, the
/// prompt stays small, and the reference keeps working when the document is
/// edited afterwards.
#[tauri::command]
pub fn artifact_reference(id: i64) -> Result<ArtifactReference, String> {
    let db = db::get_db();
    let artifact = db::artifacts::get(&db, id).ok_or("Artifact not found")?;
    let path = artifact_store::resolve(&data_dir(), &artifact.stored_name)?;
    Ok(ArtifactReference {
        id: artifact.id,
        title: artifact.title,
        kind: artifact.kind,
        path: path.to_string_lossy().to_string(),
    })
}

/// Show an artifact in the OS file manager and return its absolute path.
///
/// The path comes from [`artifact_store::resolve`] rather than any raw argument,
/// so a traversal attempt fails before a process is spawned.
#[tauri::command]
pub fn reveal_artifact(id: i64) -> Result<String, String> {
    let db = db::get_db();
    let artifact = db::artifacts::get(&db, id).ok_or("Artifact not found")?;
    let path = artifact_store::resolve(&data_dir(), &artifact.stored_name)?;
    let path_str = path.to_string_lossy().to_string();

    // Through child_env so the program is resolved against a real search path:
    // an installed app inherits launchd's PATH, and on Windows the executable
    // extension comes from PATHEXT.
    #[cfg(target_os = "windows")]
    {
        let mut cmd = crate::child_env::command("explorer");
        cmd.arg(format!("/select,{}", path_str));
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd.spawn()
            .map_err(|e| format!("Could not open explorer: {}", e))?;
    }

    #[cfg(target_os = "macos")]
    {
        crate::child_env::command("open")
            .arg("-R")
            .arg(&path_str)
            .spawn()
            .map_err(|e| format!("Could not open Finder: {}", e))?;
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let parent = path.parent().unwrap_or(&path).to_string_lossy().to_string();
        crate::child_env::command("xdg-open")
            .arg(&parent)
            .spawn()
            .map_err(|e| format!("Could not run xdg-open: {}", e))?;
    }

    Ok(path_str)
}

// ─── Testable cores ─────────────────────────────────────────────────────────
//
// The `#[tauri::command]` wrappers above reach for the global database and data
// directory. These take both explicitly so the behaviour can be tested.

pub(crate) fn update_artifact_in(
    db: &db::DbPool,
    data_dir: &str,
    id: i64,
    content: &str,
) -> Result<StoredArtifact, String> {
    let artifact = db::artifacts::get(db, id).ok_or("Artifact not found")?;
    // An in-app edit changes the body, never the classification: the title and
    // kind stay whatever they were given.
    let meta = artifact_store::meta_for(
        artifact.title.as_deref().unwrap_or_default(),
        &artifact.kind,
        content,
    );

    artifact_store::write(data_dir, &artifact.stored_name, content)?;
    db::artifacts::update_content_meta(db, id, &meta).map_err(|e| e.to_string())?;

    db::artifacts::get(db, id).ok_or_else(|| "Artifact vanished mid-update".to_string())
}

pub(crate) fn delete_artifact_in(db: &db::DbPool, data_dir: &str, id: i64) -> Result<(), String> {
    let artifact = db::artifacts::get(db, id).ok_or("Artifact not found")?;

    // Row first: an orphaned file is invisible and reclaimable by the repair
    // pass, while an orphaned row shows a document that cannot be opened.
    db::artifacts::delete(db, id).map_err(|e| e.to_string())?;
    artifact_store::remove(data_dir, &artifact.stored_name)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use rusqlite::{params, Connection};
    use std::path::PathBuf;
    use std::sync::Arc;

    struct Env {
        db: db::DbPool,
        project_id: i64,
        task_id: i64,
        data_dir: String,
        working_dir: PathBuf,
    }

    fn env(suffix: &str) -> Env {
        let base =
            std::env::temp_dir().join(format!("cb-artifact-cmd-{}-{}", std::process::id(), suffix));
        std::fs::remove_dir_all(&base).ok();
        // `data`, not the base: the store root is the data dir's sibling.
        let data_dir = base.join("data");
        let working_dir = base.join("repo");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(working_dir.join("docs")).unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::create_tables(&conn);
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute(
            "INSERT INTO projects (name,slug,working_dir) VALUES ('B','b',?1)",
            params![working_dir.to_string_lossy()],
        )
        .unwrap();
        let project_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO tasks (project_id,title,status) VALUES (?1,'t','done')",
            params![project_id],
        )
        .unwrap();
        let task_id = conn.last_insert_rowid();

        Env {
            db: Arc::new(Mutex::new(conn)),
            project_id,
            task_id,
            data_dir: data_dir.to_string_lossy().to_string(),
            working_dir,
        }
    }

    /// Index and store a document, the way capture would.
    fn seed(e: &Env, rel: &str, content: &str) -> i64 {
        let meta = artifact_store::meta_for("Seeded", "doc", content);
        let name = artifact_store::unique_stored_name(&e.data_dir, rel, 1_000);
        // The hash capture would have recorded, so these fixtures start
        // un-diverged.
        let hash = artifact_store::content_hash(content);
        let id = db::artifacts::insert_or_replace(
            &e.db,
            e.project_id,
            rel,
            &name,
            &meta,
            e.task_id,
            &hash,
        )
        .unwrap();
        artifact_store::write(&e.data_dir, &name, content).unwrap();
        id
    }

    #[test]
    fn listing_returns_the_indexed_artifacts() {
        let e = env("list");
        seed(&e, "docs/a.md", "# A\n");
        seed(&e, "docs/b.md", "# B\n");

        let rows = db::artifacts::list_for_project(&e.db, e.project_id);

        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn updating_an_artifact_rewrites_the_file_and_refreshes_metadata() {
        let e = env("update");
        let id = seed(&e, "docs/p.md", "# Old\n\nold body\n");

        let updated = update_artifact_in(&e.db, &e.data_dir, id, "# New heading\n\nnew body\n")
            .expect("update should succeed");

        // The title is the classification the agent or user gave, not something
        // re-read out of the prose on every edit.
        assert_eq!(updated.title.as_deref(), Some("Seeded"));
        assert!(updated.preview.contains("new body"));
        assert_eq!(updated.size, "# New heading\n\nnew body\n".len() as i64);
        let on_disk = artifact_store::read(&e.data_dir, &updated.stored_name).unwrap();
        assert!(on_disk.contains("new body"));
        assert!(!on_disk.contains("old body"));
    }

    #[test]
    fn updating_keeps_the_filename_so_references_stay_valid() {
        let e = env("update-name");
        let id = seed(&e, "docs/p.md", "# Old\n");
        let before = db::artifacts::get(&e.db, id).unwrap().stored_name;

        let after = update_artifact_in(&e.db, &e.data_dir, id, "# Renamed heading\n").unwrap();

        assert_eq!(after.stored_name, before);
    }

    #[test]
    fn deleting_an_artifact_removes_the_row_and_the_file() {
        let e = env("delete");
        let id = seed(&e, "docs/d.md", "# D\n");
        let name = db::artifacts::get(&e.db, id).unwrap().stored_name;

        delete_artifact_in(&e.db, &e.data_dir, id).unwrap();

        assert!(db::artifacts::get(&e.db, id).is_none());
        assert!(artifact_store::read(&e.data_dir, &name).is_err());
    }

    #[test]
    fn deleting_an_artifact_does_not_touch_the_project_repository() {
        let e = env("repo-untouched");
        let repo_file = e.working_dir.join("docs/d.md");
        std::fs::write(&repo_file, "# D\n\nthe real document\n").unwrap();
        let id = seed(&e, "docs/d.md", "# D\n\nthe real document\n");

        delete_artifact_in(&e.db, &e.data_dir, id).unwrap();

        // The store holds a copy. Deleting from it must never remove the user's
        // own file — this is the one mistake here that destroys work.
        assert!(repo_file.exists(), "the repository file must survive");
        assert_eq!(
            std::fs::read_to_string(&repo_file).unwrap(),
            "# D\n\nthe real document\n"
        );
    }

    #[test]
    fn editing_an_artifact_does_not_touch_the_project_repository() {
        let e = env("edit-untouched");
        let repo_file = e.working_dir.join("docs/p.md");
        std::fs::write(&repo_file, "# Original\n").unwrap();
        let id = seed(&e, "docs/p.md", "# Original\n");

        update_artifact_in(&e.db, &e.data_dir, id, "# Edited in the app\n").unwrap();

        assert_eq!(
            std::fs::read_to_string(&repo_file).unwrap(),
            "# Original\n",
            "an in-app edit must not rewrite the repository copy"
        );
    }

    #[test]
    fn operating_on_a_missing_artifact_is_an_error_not_a_panic() {
        let e = env("missing");
        assert!(update_artifact_in(&e.db, &e.data_dir, 9999, "x").is_err());
        assert!(delete_artifact_in(&e.db, &e.data_dir, 9999).is_err());
    }

    #[test]
    fn deleting_an_artifact_whose_file_is_already_gone_still_clears_the_row() {
        let e = env("half-deleted");
        let id = seed(&e, "docs/d.md", "# D\n");
        let name = db::artifacts::get(&e.db, id).unwrap().stored_name;
        artifact_store::remove(&e.data_dir, &name).unwrap();

        // A half-deleted artifact must still be clearable from the UI.
        delete_artifact_in(&e.db, &e.data_dir, id).unwrap();

        assert!(db::artifacts::get(&e.db, id).is_none());
    }
}
