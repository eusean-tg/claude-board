//! Running a dependency chain as one group.
//!
//! The scheduling half of dependency-ordered runs. `queue.rs` answers "what can
//! run now within the concurrency limit"; this answers "what must run at all for
//! the task the user clicked to finish", and gives that set a shared trunk branch
//! to build on.

use crate::db::tasks;
use std::process::Stdio;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Run a git command, reporting only whether it succeeded.
fn git_ok(working_dir: &str, args: &[&str]) -> bool {
    let mut cmd = crate::child_env::command("git");
    cmd.args(args)
        .current_dir(working_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

/// The trunk branch name for a group delivering `target`.
///
/// `trunk/<type>/<slug>` keeps groups visually distinct from the `feature/` and
/// `bugfix/` branches of individual tasks, and gives cleanup a single glob.
pub fn trunk_branch_name(target: &tasks::Task) -> String {
    let slug = crate::claude::runner::generate_branch_slug(&target.title);
    // A title made entirely of characters the slug drops — punctuation, or a script
    // it does not transliterate — leaves nothing to name the branch after.
    let slug = if slug.is_empty() {
        format!("task-{}", target.id)
    } else {
        slug
    };
    crate::claude::runner::sanitize_branch_name(&format!(
        "trunk/{}/{}",
        target.task_type.as_deref().unwrap_or("feature"),
        slug
    ))
}

/// Create `trunk` at `base`'s tip, or leave it alone if it is already there.
///
/// `git branch <new> <base>` rather than `checkout -b`: creating a group must not
/// move whatever the user has checked out.
pub fn create_trunk_branch(working_dir: &str, trunk: &str, base: &str) -> Result<(), String> {
    if git_ok(working_dir, &["rev-parse", "--verify", "--quiet", trunk]) {
        // A retried group start finds its trunk already made. Recreating it would
        // move the branch and orphan whatever had already been merged into it.
        return Ok(());
    }
    if git_ok(working_dir, &["branch", trunk, base]) {
        Ok(())
    } else {
        Err(format!(
            "could not create trunk branch {} from {}",
            trunk, base
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn git(args: &[&str], dir: &Path) -> bool {
        let mut cmd = crate::child_env::command("git");
        cmd.args(args)
            .current_dir(dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd.output().map(|o| o.status.success()).unwrap_or(false)
    }

    fn capture(args: &[&str], dir: &Path) -> String {
        let mut cmd = crate::child_env::command("git");
        let out = cmd.args(args).current_dir(dir).output().expect("git");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn rev_parse(dir: &Path, rev: &str) -> String {
        capture(&["rev-parse", rev], dir)
    }

    fn repo(suffix: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "cb-orchestration-{}-{}",
            std::process::id(),
            suffix
        ));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        assert!(git(&["init", "--quiet"], &root));
        git(&["config", "user.email", "test@example.com"], &root);
        git(&["config", "user.name", "Test"], &root);
        std::fs::write(root.join("seed.txt"), "seed\n").unwrap();
        git(&["add", "."], &root);
        assert!(git(&["commit", "--quiet", "-m", "seed"], &root));
        git(&["branch", "-M", "main"], &root);
        root
    }

    fn task(id: i64, title: &str, task_type: &str) -> tasks::Task {
        serde_json::from_value(serde_json::json!({
            "id": id, "project_id": 1, "title": title, "task_type": task_type,
        }))
        .unwrap()
    }

    #[test]
    fn trunk_is_created_at_the_base_branch_tip() {
        let root = repo("trunk-create");
        let dir = root.to_string_lossy().to_string();

        create_trunk_branch(&dir, "trunk/feature/x", "main").unwrap();

        // The trunk starts life identical to base, so the first task branching from
        // it sees exactly what it would have seen branching from main.
        assert_eq!(
            rev_parse(&root, "trunk/feature/x"),
            rev_parse(&root, "main")
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn creating_an_existing_trunk_is_not_an_error() {
        let root = repo("trunk-idempotent");
        let dir = root.to_string_lossy().to_string();
        create_trunk_branch(&dir, "trunk/feature/y", "main").unwrap();

        // A retried group start must not fail on the branch already being there.
        assert!(create_trunk_branch(&dir, "trunk/feature/y", "main").is_ok());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_existing_trunk_keeps_the_work_already_merged_into_it() {
        let root = repo("trunk-preserve");
        let dir = root.to_string_lossy().to_string();
        create_trunk_branch(&dir, "trunk/feature/z", "main").unwrap();
        // A member task has already landed on the trunk.
        assert!(git(&["checkout", "--quiet", "trunk/feature/z"], &root));
        std::fs::write(root.join("member.txt"), "landed\n").unwrap();
        git(&["add", "."], &root);
        assert!(git(&["commit", "--quiet", "-m", "member"], &root));
        let with_work = rev_parse(&root, "trunk/feature/z");
        assert!(git(&["checkout", "--quiet", "main"], &root));

        create_trunk_branch(&dir, "trunk/feature/z", "main").unwrap();

        // Recreating it would reset the branch to main and orphan that commit.
        assert_eq!(rev_parse(&root, "trunk/feature/z"), with_work);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn creating_a_trunk_does_not_move_the_users_checkout() {
        let root = repo("trunk-checkout");
        let dir = root.to_string_lossy().to_string();
        assert!(git(&["checkout", "--quiet", "-b", "my-work"], &root));
        let before = capture(&["rev-parse", "--abbrev-ref", "HEAD"], &root);

        create_trunk_branch(&dir, "trunk/feature/q", "main").unwrap();

        // `checkout -b` would have dragged the user onto the trunk mid-edit.
        assert_eq!(
            capture(&["rev-parse", "--abbrev-ref", "HEAD"], &root),
            before
        );
        assert_eq!(before, "my-work");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_missing_base_branch_is_an_error_rather_than_a_silent_no_op() {
        let root = repo("trunk-no-base");
        let dir = root.to_string_lossy().to_string();

        // Reporting success here would leave the group branching every task from a
        // trunk that does not exist.
        let err = create_trunk_branch(&dir, "trunk/feature/w", "nonexistent").unwrap_err();

        assert!(err.contains("nonexistent"), "got: {err}");
        assert!(!git(
            &["rev-parse", "--verify", "--quiet", "trunk/feature/w"],
            &root
        ));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_trunk_name_carries_the_targets_type_and_title() {
        let name = trunk_branch_name(&task(7, "Add OAuth login", "feature"));

        assert_eq!(name, "trunk/feature/add-oauth-login");
    }

    #[test]
    fn the_trunk_name_is_a_usable_ref_whatever_the_title_contains() {
        let root = repo("trunk-name-safe");
        let dir = root.to_string_lossy().to_string();
        let name = trunk_branch_name(&task(8, "Fix: the parser!! (again?) ~v2", "bugfix"));

        // The proof that sanitising worked is that git accepts it.
        create_trunk_branch(&dir, &name, "main").unwrap();

        assert!(
            git(&["rev-parse", "--verify", "--quiet", &name], &root),
            "{name}"
        );
        assert!(name.starts_with("trunk/bugfix/"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_title_with_nothing_sluggable_falls_back_to_the_task_id() {
        let root = repo("trunk-name-empty");
        let dir = root.to_string_lossy().to_string();
        // Punctuation only: the slug comes out empty and "trunk/feature/" is not a
        // valid ref.
        let name = trunk_branch_name(&task(42, "!!! ???", "feature"));

        create_trunk_branch(&dir, &name, "main").unwrap();

        assert_eq!(name, "trunk/feature/task-42");
        assert!(git(&["rev-parse", "--verify", "--quiet", &name], &root));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_task_with_no_type_still_gets_a_trunk() {
        let name = trunk_branch_name(
            &serde_json::from_value(serde_json::json!({
                "id": 9, "project_id": 1, "title": "Do the thing",
            }))
            .unwrap(),
        );

        assert_eq!(name, "trunk/feature/do-the-thing");
    }
}
