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

        let meta = DerivedMeta {
            title: artifact.title.clone(),
            preview,
            kind: artifact.kind.clone(),
            size,
        };
        if let Err(e) = db::artifacts::update_content_meta(db, artifact.id, &meta) {
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
