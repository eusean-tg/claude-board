//! Commands backing the project Artifacts tab.
//!
//! Artifacts are the markdown files inside a project's working directory —
//! plans, specs, notes, READMEs. `crate::services::artifacts` finds and reads
//! them; `crate::db::artifacts` says which task wrote each one. These commands
//! join the two so the frontend gets a file and its authoring tasks together.

use crate::db::{self, projects};
use crate::services::artifacts;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// An artifact plus the tasks that wrote it, flattened into one JSON object so
/// the frontend reads `rel_path` and `tasks` off the same value.
#[derive(serde::Serialize)]
pub struct ArtifactWithTasks {
    #[serde(flatten)]
    pub artifact: artifacts::Artifact,
    pub tasks: Vec<db::artifacts::ArtifactTaskRef>,
}

/// List every markdown artifact in the project, each joined to its authoring
/// tasks. Artifacts nothing wrote get an empty `tasks` list.
#[tauri::command]
pub async fn list_artifacts(project_id: i64) -> Result<Vec<ArtifactWithTasks>, String> {
    // The DB guard is a parking_lot guard and is not `Send`, so every database
    // read has to finish inside this block — before the `.await` below.
    let (working_dir, writes) = {
        let db = db::get_db();
        let project = projects::get_by_id(&db, project_id).ok_or("Project not found")?;
        let writes = db::artifacts::markdown_writes_by_project(&db, project_id, &project.working_dir);
        (project.working_dir, writes)
    };

    // Walking a large repository is slow enough to block the async runtime.
    let dir = working_dir.clone();
    let found = tokio::task::spawn_blocking(move || artifacts::list(&dir))
        .await
        .map_err(|e| format!("Task join error: {}", e))?;

    Ok(found
        .into_iter()
        .map(|artifact| {
            let tasks = writes
                .get(&artifact.rel_path.to_lowercase())
                .cloned()
                .unwrap_or_default();
            ArtifactWithTasks { artifact, tasks }
        })
        .collect())
}

/// Read one artifact's full contents.
#[tauri::command]
pub fn get_artifact(project_id: i64, rel_path: String) -> Result<artifacts::ArtifactContent, String> {
    let db = db::get_db();
    let project = projects::get_by_id(&db, project_id).ok_or("Project not found")?;
    artifacts::read(&project.working_dir, &rel_path)
}

/// Overwrite an existing artifact with edited content.
#[tauri::command]
pub fn save_artifact(project_id: i64, rel_path: String, content: String) -> Result<(), String> {
    let db = db::get_db();
    let project = projects::get_by_id(&db, project_id).ok_or("Project not found")?;
    artifacts::write(&project.working_dir, &rel_path, &content)
}

/// Show an artifact in the OS file manager and return its absolute path.
///
/// The path comes from [`artifacts::resolve`] rather than the raw argument, so a
/// traversal attempt fails before any process is spawned.
#[tauri::command]
pub fn reveal_artifact(project_id: i64, rel_path: String) -> Result<String, String> {
    let db = db::get_db();
    let project = projects::get_by_id(&db, project_id).ok_or("Project not found")?;
    let path = artifacts::resolve(&project.working_dir, &rel_path)?;
    let path_str = path.to_string_lossy().to_string();

    #[cfg(target_os = "windows")]
    {
        let mut cmd = std::process::Command::new("explorer");
        cmd.arg(format!("/select,{}", path_str));
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd.spawn()
            .map_err(|e| format!("Could not open explorer: {}", e))?;
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(&path_str)
            .spawn()
            .map_err(|e| format!("Could not open Finder: {}", e))?;
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let parent = path.parent().unwrap_or(&path).to_string_lossy().to_string();
        std::process::Command::new("xdg-open")
            .arg(&parent)
            .spawn()
            .map_err(|e| format!("Could not run xdg-open: {}", e))?;
    }

    Ok(path_str)
}
