//! Markdown parsing for captured artifacts.
//!
//! Derives the title, plain-text preview and kind of a stored document. The
//! parsing is deliberately separate from where documents live: capture
//! (`super::artifact_store`) reads a file and asks this module what it is.
//!
//! This module used to walk a project's working directory looking for markdown
//! files. That was replaced by capturing writes as they happen, which knows
//! exactly which documents an agent produced instead of listing every `.md` in
//! the repository and guessing.

const PREVIEW_CHARS: usize = 200;

/// Label an artifact by what its path says it is. Checks run from the most
/// specific match to the least.
pub(crate) fn classify(rel_path: &str) -> &'static str {
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
pub(crate) fn title_and_preview(content: &str) -> (Option<String>, String) {
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
        non_empty(
            value
                .trim_matches('"')
                .trim_matches('\'')
                .trim()
                .to_string(),
        )
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
    let rest = rest
        .strip_prefix(". ")
        .or_else(|| rest.strip_prefix(") "))?;
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
        let (title, preview) = title_and_preview(
            "---\ntitle: \"Introduction\"\nsidebar: 2\n---\n\nGetting started.\n",
        );
        assert_eq!(title.as_deref(), Some("Introduction"));
        assert_eq!(preview, "Getting started.");

        // An H1 wins over front matter.
        let (title, _) =
            title_and_preview("---\ntitle: From Front Matter\n---\n\n# From Heading\n");
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
}
