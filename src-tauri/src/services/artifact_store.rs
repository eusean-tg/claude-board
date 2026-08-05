//! On-disk storage for captured artifacts.
//!
//! Documents live in a flat directory under the app's data directory rather than
//! as blobs in SQLite, so an artifact can be referenced by absolute path: an
//! agent handed a path can read the document itself, and the prompt stays small.
//!
//! Names are generated here and never taken from input. `resolve` treats every
//! name arriving from the frontend as hostile and refuses anything that is not a
//! single path component, which is simpler to reason about than canonicalising
//! and comparing prefixes — and cannot be defeated by a symlink.

use std::path::{Path, PathBuf};

/// The directory holding every stored document, under the configured data dir.
pub fn root(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join("artifacts")
}

/// A flat, collision-free filename that still reads like the document it holds.
///
/// `id_hint` is the artifact's row id, which is why the row is inserted before
/// the file is written: it guarantees uniqueness without a second lookup, so two
/// documents whose basenames match — `docs/a/notes.md` and `docs/b/notes.md` —
/// cannot collide in a flat directory.
pub fn stored_name(id_hint: &str, source_rel_path: &str) -> String {
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
    let slug = slug.trim_matches('-').to_string();
    let slug = if slug.is_empty() {
        "artifact".to_string()
    } else {
        slug
    };
    format!("{}-{}.md", id_hint, slug)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway data dir. `suffix` keeps concurrently running tests out of
    /// each other's directories.
    fn tmp(suffix: &str) -> String {
        let dir = std::env::temp_dir().join(format!(
            "cb-artifact-store-{}-{}",
            std::process::id(),
            suffix
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().to_string()
    }

    #[test]
    fn stored_names_are_flat_and_carry_no_path_separators() {
        let name = stored_name("42", "docs/plans/auth-plan.md");
        assert!(!name.contains('/') && !name.contains('\\'), "got {}", name);
        assert!(name.ends_with(".md"));
        assert!(
            name.contains("auth-plan"),
            "should stay recognisable: {}",
            name
        );
    }

    #[test]
    fn two_documents_with_the_same_basename_get_distinct_names() {
        // A flat store has one namespace; the row id is what keeps these apart.
        let one = stored_name("1", "docs/a/notes.md");
        let two = stored_name("2", "docs/b/notes.md");
        assert_ne!(one, two);
    }

    #[test]
    fn an_unusable_basename_still_produces_a_valid_name() {
        // A punctuation-only stem must not collapse to "1-.md".
        assert_eq!(stored_name("1", "docs/___.md"), "1-artifact.md");
        // Non-ASCII characters become separators, and the leading and trailing
        // ones are trimmed rather than left dangling.
        assert_eq!(stored_name("2", "docs/Ünïcödé.md"), "2-n-c-d.md");
        // No basename at all still resolves to something writable.
        assert_eq!(stored_name("3", ""), "3-artifact.md");
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
        let name = stored_name("1", "docs/x.md");
        let path = resolve(&dir, &name).unwrap();
        assert!(path.starts_with(root(&dir)));
    }

    #[test]
    fn write_creates_the_root_and_read_returns_the_content() {
        let dir = tmp("roundtrip");
        let name = stored_name("7", "docs/plan.md");

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
        let name = stored_name("8", "docs/plan.md");
        write(&dir, &name, "old\n").unwrap();

        write(&dir, &name, "new\n").unwrap();

        assert_eq!(read(&dir, &name).unwrap(), "new\n");
    }

    #[test]
    fn removing_a_document_twice_is_not_an_error() {
        let dir = tmp("remove");
        let name = stored_name("9", "docs/gone.md");
        write(&dir, &name, "x\n").unwrap();

        remove(&dir, &name).unwrap();
        // A half-deleted artifact must still be clearable.
        remove(&dir, &name).unwrap();

        assert!(read(&dir, &name).is_err());
    }
}
