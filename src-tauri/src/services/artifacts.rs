//! Discovery and reading of markdown artifacts (plans, RFCs, specs, docs) inside
//! a project's working directory.
//!
//! Every path that crosses this module's boundary is a `rel_path`: forward-slash
//! separated and relative to the working directory. `resolve` is the single place
//! a `rel_path` becomes an absolute path, and it refuses anything that would land
//! outside the working directory.

use serde::Serialize;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use super::scanner::{GitignoreRules, ALWAYS_EXCLUDE_DIRS};

// ─── constants ───

const MAX_DEPTH: usize = 12;
const MAX_FILE_SIZE: u64 = 1_048_576; // 1 MiB
const MAX_ARTIFACTS: usize = 1000;
const PREVIEW_CHARS: usize = 200;

/// Extensions treated as markdown artifacts (compared lowercased).
const MARKDOWN_EXTENSIONS: &[&str] = &["md", "mdx"];

// ─── public types ───

#[derive(Serialize, Clone, Debug)]
pub struct Artifact {
    pub rel_path: String,
    pub name: String,
    pub dir: String,
    pub title: Option<String>,
    pub preview: String,
    pub kind: String,
    pub size_bytes: u64,
    pub modified_at: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct ArtifactContent {
    pub rel_path: String,
    pub abs_path: String,
    pub content: String,
    pub size_bytes: u64,
    pub modified_at: Option<String>,
}

// ─── discovery ───

/// List every markdown artifact under `working_dir`, newest first.
///
/// Honours the working directory's `.gitignore` and [`ALWAYS_EXCLUDE_DIRS`],
/// descends at most [`MAX_DEPTH`] levels, and skips files over [`MAX_FILE_SIZE`].
/// Unreadable entries are logged and skipped rather than failing the whole walk.
pub fn list(working_dir: &str) -> Vec<Artifact> {
    let root = PathBuf::from(working_dir);
    let gitignore = GitignoreRules::load(&root);

    let mut artifacts = Vec::new();
    walk(&root, &root, &gitignore, 0, &mut artifacts);

    artifacts.sort_by(|a, b| {
        b.modified_at
            .cmp(&a.modified_at)
            .then_with(|| a.rel_path.cmp(&b.rel_path))
    });
    artifacts.truncate(MAX_ARTIFACTS);
    artifacts
}

fn walk(
    dir: &Path,
    root: &Path,
    gitignore: &GitignoreRules,
    depth: usize,
    out: &mut Vec<Artifact>,
) {
    if depth >= MAX_DEPTH {
        return;
    }

    let read_dir = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            log::error!("artifacts: cannot read directory {}: {}", dir.display(), e);
            return;
        }
    };

    for entry in read_dir {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                log::error!("artifacts: cannot read an entry in {}: {}", dir.display(), e);
                continue;
            }
        };

        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy().to_string();

        // Metadata from the DirEntry does not follow symlinks, so a symlinked
        // directory is neither descended into nor mistaken for a regular file.
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(e) => {
                log::error!("artifacts: cannot stat {}: {}", path.display(), e);
                continue;
            }
        };
        let is_dir = metadata.is_dir();

        if is_dir && ALWAYS_EXCLUDE_DIRS.contains(&name.as_str()) {
            continue;
        }

        let rel_path = match path.strip_prefix(root) {
            Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };

        if gitignore.is_ignored(&rel_path, is_dir) {
            continue;
        }

        if is_dir {
            walk(&path, root, gitignore, depth + 1, out);
            continue;
        }

        if !metadata.is_file() || !is_markdown(&path) || metadata.len() > MAX_FILE_SIZE {
            continue;
        }

        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(e) => {
                log::error!("artifacts: cannot read {}: {}", path.display(), e);
                continue;
            }
        };
        let (title, preview) = title_and_preview(&content);

        out.push(Artifact {
            dir: rel_path
                .rsplit_once('/')
                .map(|(parent, _)| parent.to_string())
                .unwrap_or_default(),
            kind: classify(&rel_path).to_string(),
            rel_path,
            name,
            title,
            preview,
            size_bytes: metadata.len(),
            modified_at: metadata.modified().ok().map(rfc3339),
        });
    }
}

// ─── path resolution ───

/// Turn a `rel_path` into a verified absolute path inside `working_dir`.
///
/// This is the security boundary for [`read`] and [`write`]: the path must be
/// relative, free of `..` components, and a markdown file, and after
/// canonicalization it must still sit inside the canonicalized working
/// directory — which also rules out escapes through symlinks.
pub fn resolve(working_dir: &str, rel_path: &str) -> Result<PathBuf, String> {
    let normalized = rel_path.replace('\\', "/");
    if normalized.trim().is_empty() {
        return Err("artifact path is empty".to_string());
    }

    let candidate = Path::new(&normalized);
    if candidate.is_absolute() {
        return Err(format!("artifact path must be relative: {}", rel_path));
    }
    if !is_markdown(candidate) {
        return Err(format!("not a markdown artifact: {}", rel_path));
    }

    let mut joined = PathBuf::from(working_dir);
    for component in candidate.components() {
        match component {
            Component::Normal(part) => joined.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!(
                    "artifact path may not traverse upwards: {}",
                    rel_path
                ))
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!("artifact path must be relative: {}", rel_path))
            }
        }
    }

    let root = fs::canonicalize(working_dir)
        .map_err(|e| format!("cannot resolve working directory {}: {}", working_dir, e))?;
    let resolved = fs::canonicalize(&joined)
        .map_err(|e| format!("cannot resolve artifact {}: {}", rel_path, e))?;

    if !resolved.starts_with(&root) {
        return Err(format!(
            "artifact path escapes the working directory: {}",
            rel_path
        ));
    }

    Ok(resolved)
}

/// Read a markdown artifact's full contents.
pub fn read(working_dir: &str, rel_path: &str) -> Result<ArtifactContent, String> {
    let abs_path = resolve(working_dir, rel_path)?;

    let metadata = fs::metadata(&abs_path)
        .map_err(|e| format!("cannot stat artifact {}: {}", rel_path, e))?;
    if !metadata.is_file() {
        return Err(format!("artifact is not a file: {}", rel_path));
    }

    let content = fs::read_to_string(&abs_path)
        .map_err(|e| format!("cannot read artifact {}: {}", rel_path, e))?;

    Ok(ArtifactContent {
        rel_path: rel_path.replace('\\', "/"),
        abs_path: abs_path.to_string_lossy().to_string(),
        content,
        size_bytes: metadata.len(),
        modified_at: metadata.modified().ok().map(rfc3339),
    })
}

/// Overwrite an existing markdown artifact.
///
/// The target must already exist, so this cannot be used to create files.
pub fn write(working_dir: &str, rel_path: &str, content: &str) -> Result<(), String> {
    let abs_path = resolve(working_dir, rel_path)?;
    if !abs_path.is_file() {
        return Err(format!("artifact does not exist: {}", rel_path));
    }
    fs::write(&abs_path, content).map_err(|e| format!("cannot write artifact {}: {}", rel_path, e))
}

// ─── classification ───

/// Label an artifact by what its path says it is. Checks run from the most
/// specific match to the least.
fn classify(rel_path: &str) -> &'static str {
    let lower = rel_path.replace('\\', "/").to_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(lower.as_str());

    if name == "readme.md" || name == "readme.mdx" {
        return "readme";
    }
    if lower.contains("plan") {
        return "plan";
    }
    if lower.contains("rfc") {
        return "rfc";
    }
    if lower.contains("spec") || lower.contains("design") {
        return "spec";
    }
    if lower.split('/').any(|segment| segment == "docs") {
        return "doc";
    }
    "other"
}

// ─── title and preview extraction ───

/// Pull a display title and a short plain-text preview out of markdown source.
///
/// The title is the first H1, falling back to a top-level `title:` key in YAML
/// front matter. The preview is the leading body text with front matter, fenced
/// code, and markdown syntax removed.
fn title_and_preview(content: &str) -> (Option<String>, String) {
    let (front_matter, body) = split_front_matter(content);

    let mut heading_title: Option<String> = None;
    let mut preview = String::new();
    let mut in_fence = false;

    for line in body.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }

        if let Some((level, text)) = heading(trimmed) {
            // The first H1 becomes the title, so it is not repeated in the preview.
            if level == 1 && heading_title.is_none() {
                heading_title = non_empty(strip_inline(text));
                continue;
            }
            append_words(&mut preview, &strip_inline(text));
        } else {
            append_words(&mut preview, &strip_inline(&strip_block_markers(trimmed)));
        }

        if preview.chars().count() >= PREVIEW_CHARS {
            break;
        }
    }

    let title = heading_title.or_else(|| front_matter.and_then(front_matter_title));
    let preview = preview
        .chars()
        .take(PREVIEW_CHARS)
        .collect::<String>()
        .trim_end()
        .to_string();

    (title, preview)
}

/// Split leading YAML front matter off the body. Returns `(None, content)` when
/// there is no front matter or its closing delimiter is missing.
fn split_front_matter(content: &str) -> (Option<&str>, &str) {
    let rest = match content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))
    {
        Some(rest) => rest,
        None => return (None, content),
    };

    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end();
        if trimmed == "---" || trimmed == "..." {
            return (Some(&rest[..offset]), &rest[offset + line.len()..]);
        }
        offset += line.len();
    }

    (None, content)
}

fn front_matter_title(front_matter: &str) -> Option<String> {
    front_matter.lines().find_map(|line| {
        // Only a top-level key counts; nested keys are indented.
        let value = line.strip_prefix("title:")?.trim();
        non_empty(value.trim_matches('"').trim_matches('\'').trim().to_string())
    })
}

/// Recognize an ATX heading, returning its level and text.
fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.len() - line.trim_start_matches('#').len();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &line[level..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    Some((level, rest.trim().trim_end_matches('#').trim()))
}

/// Strip blockquote markers and list bullets; horizontal rules become empty.
fn strip_block_markers(line: &str) -> String {
    let mut rest = line.trim();
    loop {
        let stripped = rest
            .strip_prefix('>')
            .or_else(|| rest.strip_prefix("- "))
            .or_else(|| rest.strip_prefix("* "))
            .or_else(|| rest.strip_prefix("+ "))
            .map(str::trim_start)
            .or_else(|| ordered_list_marker(rest));
        match stripped {
            Some(next) => rest = next,
            None => break,
        }
    }

    if !rest.is_empty()
        && rest
            .chars()
            .all(|c| matches!(c, '-' | '=' | '*' | '_' | ' '))
    {
        return String::new();
    }
    rest.to_string()
}

fn ordered_list_marker(line: &str) -> Option<&str> {
    let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None;
    }
    let rest = &line[digits..];
    let rest = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") "))?;
    Some(rest.trim_start())
}

/// Reduce inline markdown to plain text: images drop out, links keep their text,
/// HTML tags and emphasis markers are removed.
fn strip_inline(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;

    while i < chars.len() {
        match chars[i] {
            '!' if chars.get(i + 1) == Some(&'[') => {
                i += 2;
                while i < chars.len() && chars[i] != ']' {
                    i += 1;
                }
                i = skip_link_target(&chars, i + 1);
            }
            '[' => {
                i += 1;
                while i < chars.len() && chars[i] != ']' {
                    out.push(chars[i]);
                    i += 1;
                }
                i = skip_link_target(&chars, i + 1);
            }
            '<' => {
                while i < chars.len() && chars[i] != '>' {
                    i += 1;
                }
                i += 1;
            }
            '`' | '*' => i += 1,
            // Single `_` and `~` are left alone: they are common in identifiers
            // and paths, where stripping them would mangle the text.
            '_' | '~' if chars.get(i + 1) == Some(&chars[i]) => i += 2,
            c => {
                out.push(c);
                i += 1;
            }
        }
    }

    out
}

/// Skip a `(...)` link target starting at `i`, if there is one.
fn skip_link_target(chars: &[char], mut i: usize) -> usize {
    if chars.get(i) != Some(&'(') {
        return i;
    }
    let mut depth = 0;
    while i < chars.len() {
        match chars[i] {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        i += 1;
        if depth == 0 {
            break;
        }
    }
    i
}

/// Append `text`'s words to `preview`, collapsing all whitespace to single spaces.
fn append_words(preview: &mut String, text: &str) {
    for word in text.split_whitespace() {
        if !preview.is_empty() {
            preview.push(' ');
        }
        preview.push_str(word);
    }
}

// ─── small helpers ───

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| MARKDOWN_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        .unwrap_or(false)
}

fn rfc3339(time: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(time)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn non_empty(value: String) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a throwaway fixture tree. `suffix` keeps concurrently running tests
    /// from sharing a directory.
    fn fixture(suffix: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "cb-artifacts-{}-{}",
            std::process::id(),
            suffix
        ));
        std::fs::remove_dir_all(&root).ok();
        for (rel_path, content) in files {
            let path = root.join(rel_path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, content).unwrap();
        }
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn list_finds_nested_markdown_and_skips_excluded_paths() {
        let root = fixture(
            "list",
            &[
                (".gitignore", "ignored/\n"),
                ("README.md", "# Board\n\nA project.\n"),
                ("docs/features/x.md", "# Feature X\n\nDetails.\n"),
                ("plans/auth-plan.md", "# Auth plan\n"),
                ("node_modules/pkg/guide.md", "# Vendored\n"),
                ("ignored/secret.md", "# Ignored\n"),
                ("notes.txt", "not markdown\n"),
            ],
        );

        let found: Vec<String> = list(root.to_str().unwrap())
            .into_iter()
            .map(|a| a.rel_path)
            .collect();

        assert!(found.contains(&"README.md".to_string()), "{:?}", found);
        assert!(
            found.contains(&"docs/features/x.md".to_string()),
            "a nested markdown file is found: {:?}",
            found
        );
        assert!(found.contains(&"plans/auth-plan.md".to_string()), "{:?}", found);

        assert!(
            !found.iter().any(|p| p.starts_with("node_modules/")),
            "node_modules is always excluded: {:?}",
            found
        );
        assert!(
            !found.iter().any(|p| p.starts_with("ignored/")),
            "a .gitignored directory is excluded: {:?}",
            found
        );
        assert!(
            !found.iter().any(|p| p.ends_with(".txt")),
            "non-markdown files are excluded: {:?}",
            found
        );

        // Metadata is filled in from the file itself.
        let artifacts = list(root.to_str().unwrap());
        let readme = artifacts
            .iter()
            .find(|a| a.rel_path == "README.md")
            .expect("README.md is listed");
        assert_eq!(readme.name, "README.md");
        assert_eq!(readme.dir, "");
        assert_eq!(readme.kind, "readme");
        assert_eq!(readme.title.as_deref(), Some("Board"));
        assert_eq!(readme.preview, "A project.");
        assert!(readme.modified_at.is_some());

        let nested = artifacts
            .iter()
            .find(|a| a.rel_path == "docs/features/x.md")
            .expect("the nested file is listed");
        assert_eq!(nested.dir, "docs/features");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn classify_labels_paths_by_kind() {
        assert_eq!(classify("docs/features/x.md"), "doc");
        assert_eq!(classify("plans/auth-plan.md"), "plan");
        assert_eq!(classify("RFC-001.md"), "rfc");
        assert_eq!(classify("README.md"), "readme");
        assert_eq!(classify("docs/design/tokens.md"), "spec");
        assert_eq!(classify("SPEC.md"), "spec");
        assert_eq!(classify("notes/random.md"), "other");
    }

    #[test]
    fn title_and_preview_extract_the_heading_and_body() {
        let (title, preview) = title_and_preview(
            "# Auth Plan\n\nWe will use **OAuth** for [login](https://example.com).\n\n```rust\nlet skipped = 1;\n```\n",
        );
        assert_eq!(title.as_deref(), Some("Auth Plan"));
        assert_eq!(preview, "We will use OAuth for login.");

        // Front matter supplies the title when there is no H1, and never leaks
        // into the preview.
        let (title, preview) =
            title_and_preview("---\ntitle: \"Introduction\"\nsidebar: 2\n---\n\nGetting started.\n");
        assert_eq!(title.as_deref(), Some("Introduction"));
        assert_eq!(preview, "Getting started.");

        // An H1 wins over front matter.
        let (title, _) = title_and_preview("---\ntitle: From Front Matter\n---\n\n# From Heading\n");
        assert_eq!(title.as_deref(), Some("From Heading"));

        // Bullets, rules, and sub-headings reduce to plain words.
        let (_, preview) = title_and_preview("# T\n\n---\n\n## Goals\n\n- first\n- second\n");
        assert_eq!(preview, "Goals first second");

        let (title, preview) = title_and_preview("no title here\n");
        assert_eq!(title, None);
        assert_eq!(preview, "no title here");

        // The preview is capped at roughly PREVIEW_CHARS (trailing whitespace at
        // the cut is trimmed, so it can land just under).
        let long = format!("# T\n\n{}\n", "word ".repeat(200));
        let (_, preview) = title_and_preview(&long);
        let length = preview.chars().count();
        assert!(
            (PREVIEW_CHARS - 5..=PREVIEW_CHARS).contains(&length),
            "preview length {} is not capped near {}",
            length,
            PREVIEW_CHARS
        );
    }

    #[test]
    fn resolve_rejects_paths_outside_the_working_directory() {
        let root = fixture(
            "resolve",
            &[("notes.md", "# Notes\n"), ("notes.txt", "plain\n")],
        );
        let working_dir = root.to_str().unwrap();

        // A legitimate relative markdown path resolves inside the working dir.
        let resolved = resolve(working_dir, "notes.md").expect("notes.md resolves");
        assert!(resolved.starts_with(std::fs::canonicalize(&root).unwrap()));

        assert!(
            resolve(working_dir, "../../etc/passwd").is_err(),
            "parent traversal is rejected"
        );
        assert!(
            resolve(working_dir, "/etc/passwd").is_err(),
            "an absolute path is rejected"
        );
        assert!(
            resolve(working_dir, "notes.txt").is_err(),
            "a non-markdown extension is rejected"
        );
        assert!(
            resolve(working_dir, "docs/../notes.md").is_err(),
            "a `..` in the middle is rejected too"
        );

        // write refuses to create a file that is not already there.
        assert!(write(working_dir, "new.md", "x").is_err());
        write(working_dir, "notes.md", "# Edited\n").expect("an existing artifact is writable");
        let content = read(working_dir, "notes.md").expect("the artifact reads back");
        assert_eq!(content.content, "# Edited\n");
        assert_eq!(content.rel_path, "notes.md");

        std::fs::remove_dir_all(&root).ok();
    }
}
