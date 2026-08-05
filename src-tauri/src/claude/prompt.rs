use crate::db::attachments::Attachment;
use crate::db::projects::Project;
use crate::db::roles::Role;
use crate::db::snippets::Snippet;
use crate::db::tasks::{Task, TaskRevision};
use crate::db::templates::Template;

/// A referenced document, reduced to what the prompt needs.
pub struct ArtifactRef {
    pub title: String,
    pub kind: String,
    pub path: String,
}

/// How many referenced documents to name inline before deferring to the tool.
const MAX_LISTED_ARTIFACTS: usize = 10;

/// The prompt section naming the documents a task should read and update.
///
/// Paths rather than content: the agent reads what it needs with its own tools,
/// the prompt stays small whatever the documents' size, and a reference keeps
/// working after the document is edited.
///
/// The guidance matters as much as the list. A referenced document lives in the
/// store, and once its stored copy has been edited, capture never overwrites it
/// from the repository again — so an agent that edits a repository copy instead
/// produces a second lineage that nothing reconciles.
pub fn artifact_section(refs: &[ArtifactRef], omitted: usize) -> String {
    if refs.is_empty() && omitted == 0 {
        return String::new();
    }

    let mut lines = vec![
        "\n## Referenced Documents".to_string(),
        "These documents are kept in Claude Board's artifact store. Read and update them \
         at the paths below — those are the live copies. Editing a copy inside the \
         repository instead leaves the two versions to drift apart."
            .to_string(),
    ];

    for r in refs.iter().take(MAX_LISTED_ARTIFACTS) {
        lines.push(format!("- **{}** ({}) → `{}`", r.title, r.kind, r.path));
    }

    // Stating the omission rather than truncating silently: a partial list that
    // looks complete invites the agent to conclude a document does not exist.
    let hidden = omitted + refs.len().saturating_sub(MAX_LISTED_ARTIFACTS);
    if hidden > 0 {
        lines.push(format!(
            "\n{} further document(s) are not listed here. Use the `list_artifacts` \
             tool to see them.",
            hidden
        ));
    }

    lines.join("\n")
}

#[allow(clippy::too_many_arguments)]
pub fn build_prompt(
    task: &Task,
    revisions: &[TaskRevision],
    snippets: &[Snippet],
    attachments: &[Attachment],
    artifacts: &[ArtifactRef],
    role: Option<&Role>,
    project_id: i64,
    parent_contexts: &[(String, String)],
    template: Option<&Template>,
    project: Option<&Project>,
) -> String {
    let mut parts = Vec::new();
    let is_revision = !revisions.is_empty();
    let revision_num = task.revision_count.unwrap_or(revisions.len() as i64);

    if let Some(r) = role {
        if let Some(ref prompt) = r.prompt {
            if !prompt.is_empty() {
                parts.push(format!("## Role: {}", r.name));
                parts.push(prompt.clone());
                parts.push(String::new());
            }
        }
    }

    // Prompt template: inject custom instructions for this task type
    if let Some(tmpl) = template {
        if !tmpl.template.is_empty() {
            parts.push(format!("## Prompt Template: {}", tmpl.name));
            if let Some(ref desc) = tmpl.description {
                if !desc.is_empty() {
                    parts.push(desc.clone());
                }
            }
            parts.push(tmpl.template.clone());
            parts.push(String::new());
        }
    }

    if is_revision {
        parts.push(format!("# REVISION #{}: {}", revision_num, task.title));
        parts.push("\n> This task has been reviewed and sent back for changes. You MUST address ALL feedback below.".into());
    } else {
        parts.push(format!("# Task: {}", task.title));
    }

    if let Some(ref desc) = task.description {
        if !desc.is_empty() {
            parts.push(format!("\n## Description\n{}", desc));
        }
    }
    if let Some(ref criteria) = task.acceptance_criteria {
        if !criteria.is_empty() {
            parts.push(format!("\n## Acceptance Criteria\n{}", criteria));
        }
    }

    if is_revision {
        parts.push("\n## Revision History".into());
        parts.push(format!("This task has been reviewed {} time(s). Address ALL feedback from the latest revision.", revisions.len()));
        parts.push("Previous work has already been committed — build on top of it, do NOT start from scratch.\n".into());
        for rev in revisions {
            parts.push(format!(
                "### Revision #{} ({})",
                rev.revision_number,
                rev.created_at.as_deref().unwrap_or("")
            ));
            parts.push(rev.feedback.clone());
            parts.push(String::new());
        }
        parts.push("\n## IMPORTANT: Revision Instructions".into());
        parts.push(format!(
            "- Focus on the LATEST revision feedback (#{}) above.",
            revision_num
        ));
        parts.push("- The previous work is already in the codebase — review what was done and fix/improve based on the feedback.".into());
        parts.push("- Do NOT redo work that was already accepted — only change what the feedback asks for.".into());
        parts.push(format!(
            "- Commit your revision changes with a clear message referencing revision #{}.",
            revision_num
        ));
    }

    // Context from completed parent dependencies
    if !parent_contexts.is_empty() {
        parts.push("\n## Context from Completed Dependencies".into());
        parts.push("The following tasks were completed before this one. Their changes are already in the codebase:".into());
        for (title, summary) in parent_contexts {
            parts.push(format!("\n### {}", title));
            parts.push(summary.clone());
        }
        parts.push(String::new());
    }

    if !snippets.is_empty() {
        parts.push("\n## Project Context".into());
        for s in snippets {
            parts.push(format!("### {}", s.title));
            parts.push(s.content.clone());
            parts.push(String::new());
        }
    }

    if !attachments.is_empty() {
        parts.push("\n## Attached Files".into());
        parts.push("The following files have been provided as reference for this task:".into());
        for a in attachments {
            let size_kb = a.size.unwrap_or(0) as f64 / 1024.0;
            parts.push(format!(
                "- **{}** ({}, {:.1}KB) → `.claude-attachments/{}`",
                a.original_name,
                a.mime_type.as_deref().unwrap_or(""),
                size_kb,
                a.filename
            ));
        }
        parts.push("\nThese files are available in the `.claude-attachments/` directory relative to the working directory. Read them as needed for context.".into());
    }

    let artifact_part = artifact_section(artifacts, 0);
    if !artifact_part.is_empty() {
        parts.push(artifact_part);
    }

    parts.push("\n## Claude Board Integration".into());
    parts.push(
        "You have access to Claude Board MCP tools. Use them to manage tasks on the project board:"
            .into(),
    );
    parts.push(format!(
        "- **list_tasks** — See all tasks in this project (project_id: {})",
        project_id
    ));
    parts.push(format!("- **create_task** — Create sub-tasks if this task needs to be broken down. Pass parent_task_id: {} to link them — the parent will automatically wait for all sub-tasks to complete.", task.id));
    parts.push("- **change_task_status** — Move tasks between statuses".into());
    parts.push("- **get_task_detail** — Get full details of any task".into());
    parts.push("- **list_task_summary** — Get a grouped summary of all tasks".into());
    parts.push(format!(
        "- **save_artifact** — Record a document *about the work* — a plan, RFC, spec, \
         research notes, a progress log — so the user can read it and later tasks can \
         reference it. Give it a real title, a kind, and tags. Pass task_id: {} for \
         attribution. Files that belong in the codebase are not artifacts: write those \
         to the repository as usual.",
        task.id
    ));
    parts.push(
        "- **list_artifacts** / **update_artifact** — Find documents saved earlier and \
         revise one instead of saving a second copy of it."
            .into(),
    );
    // Scoped to the task-management tools on purpose. This line used to gate every
    // tool listed above behind "when the task description asks you to", which told
    // the agent to wait to be asked — so save_artifact was never called unless the
    // user named it.
    parts.push(format!("Use the task tools above when the task description asks you to plan, break down work, or manage tasks. The current project_id is {}.", project_id));
    parts.push(
        "The artifact tools are different: reach for save_artifact whenever this task \
         produces a document, without being asked."
            .into(),
    );

    parts.push("\n## Instructions".into());
    parts.push(format!(
        "- Task type: {}",
        task.task_type.as_deref().unwrap_or("feature")
    ));
    parts.push("- Complete this task thoroughly and commit your changes.".into());
    // Placed among the imperatives rather than only in the tool inventory above: a
    // list of capabilities describes what is possible, while this describes what to
    // do, and it triggers on the agent's own output rather than on being asked.
    parts.push(format!(
        "- If you produce a document as part of this task — a plan, a design note, a \
         research summary, a decision record, a progress log — save it with the \
         **save_artifact** tool (project_id: {}, task_id: {}) instead of writing it \
         into the repository. Give it a real title, a kind, and tags. Code, config, \
         and documentation that ships as part of the codebase are not artifacts and \
         belong in the repository as usual.",
        project_id, task.id
    ));

    let branch = task.branch_name.as_deref().unwrap_or("");
    let auto_branch_on = project
        .map(|p| p.auto_branch.unwrap_or(1) == 1)
        .unwrap_or(true);
    let auto_push_on = project
        .map(|p| p.auto_push.unwrap_or(0) == 1)
        .unwrap_or(false);

    if !is_revision {
        if !branch.is_empty() {
            if auto_push_on {
                parts.push(format!("- You are already on branch \"{}\". Commit and push your changes to this branch.", branch));
            } else {
                parts.push(format!("- You are already on branch \"{}\". Commit your changes to this branch. Do NOT run git push.", branch));
            }
        } else if auto_branch_on {
            let branch_hint = format!(
                "{}/task-{}",
                task.task_type.as_deref().unwrap_or("feature"),
                task.id
            );
            if auto_push_on {
                parts.push(format!(
                    "- Create a new branch named {}, commit, and push.",
                    branch_hint
                ));
            } else {
                parts.push(format!(
                    "- Create a new branch named {}, and commit your changes. Do NOT run git push.",
                    branch_hint
                ));
            }
        } else {
            // auto_branch OFF: work on current branch, no new branches
            parts.push("- IMPORTANT: Do NOT create any new git branches. Do NOT run git checkout -b or git branch. Work on the current branch only.".into());
            if !auto_push_on {
                parts.push("- IMPORTANT: Do NOT run git push under any circumstances. Do NOT push to any remote.".into());
            }
        }
    } else if !branch.is_empty() {
        if auto_push_on {
            parts.push(format!(
                "- You are on branch \"{}\". Commit and push your revision changes to this branch.",
                branch
            ));
        } else {
            parts.push(format!("- You are on branch \"{}\". Commit your revision changes to this branch. Do NOT run git push.", branch));
        }
    } else {
        parts.push("- Work on the existing branch. Commit your revision changes.".into());
        if !auto_push_on {
            parts.push("- IMPORTANT: Do NOT run git push under any circumstances.".into());
        }
    }

    parts.push("- Write clear commit messages describing what was done.".into());
    parts.push("- If acceptance criteria are provided, ensure all criteria are met.".into());

    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A prompt with nothing but the defaults, for asserting on the boilerplate.
    fn minimal_prompt() -> String {
        let task: Task = serde_json::from_value(serde_json::json!({
            "id": 7,
            "project_id": 1,
            "title": "Do the thing",
        }))
        .unwrap();
        build_prompt(&task, &[], &[], &[], &[], None, 1, &[], None, None)
    }

    fn artifact_ref(title: &str, kind: &str, path: &str) -> ArtifactRef {
        ArtifactRef {
            title: title.into(),
            kind: kind.into(),
            path: path.into(),
        }
    }

    #[test]
    fn the_artifact_tools_are_not_gated_behind_being_asked() {
        let prompt = minimal_prompt();

        // The old wording — "Use these tools when the task description asks you
        // to" — sat under the whole tool list and told the agent to wait. That is
        // why save_artifact was never called unless the user named it.
        assert!(
            prompt.contains("Use the task tools above when the task description asks you to"),
            "the gate must name the task tools, not all of them"
        );
        assert!(
            !prompt.contains("Use these tools when the task description asks you to"),
            "the un-scoped gate is what suppressed save_artifact"
        );
    }

    #[test]
    fn saving_a_document_is_an_instruction_not_just_a_capability() {
        let prompt = minimal_prompt();
        let instructions = prompt
            .split("## Instructions")
            .nth(1)
            .expect("the prompt has an Instructions section");

        // A capability inventory says what is possible; this has to say what to do,
        // in the section holding the other imperatives.
        assert!(
            instructions.contains("save_artifact"),
            "got {}",
            instructions
        );
        assert!(
            instructions.contains("without being asked") || instructions.contains("If you produce"),
            "the instruction has to trigger on the agent's own output"
        );
    }

    #[test]
    fn the_instruction_says_what_is_not_an_artifact() {
        let prompt = minimal_prompt();
        // Without this, a docs-site page or a source file gets saved as an
        // artifact — the false positive that killed the capture approach.
        assert!(
            prompt.contains("belong in the repository"),
            "got {}",
            prompt
        );
    }

    #[test]
    fn the_artifact_section_lists_paths_not_content() {
        let section = artifact_section(
            &[
                artifact_ref("Auth plan", "plan", "/store/auth-plan-1.md"),
                artifact_ref("Queue RFC", "rfc", "/store/queue-rfc-2.md"),
            ],
            0,
        );

        assert!(section.contains("/store/auth-plan-1.md"));
        assert!(section.contains("Auth plan"));
        assert!(section.contains("Queue RFC"));
        // Inlining content would blow up every prompt, and most of it would be
        // irrelevant to the task at hand.
        assert!(!section.contains("# Auth plan\n"));
    }

    #[test]
    fn the_section_tells_the_agent_to_update_the_given_path() {
        let section = artifact_section(&[artifact_ref("Plan", "plan", "/store/plan-1.md")], 0);
        // Without this, an agent edits a repository copy and the two versions
        // drift apart with nothing reconciling them.
        assert!(section.to_lowercase().contains("update"), "got {}", section);
        assert!(
            section.to_lowercase().contains("repositor"),
            "got {}",
            section
        );
    }

    #[test]
    fn nothing_referenced_produces_no_section() {
        assert_eq!(artifact_section(&[], 0), "");
    }

    #[test]
    fn the_section_says_how_many_it_omitted() {
        let section = artifact_section(&[artifact_ref("One", "doc", "/store/one.md")], 14);
        // Silent truncation reads as "this is everything", which invites the
        // agent to conclude a document does not exist.
        assert!(section.contains("14"), "got {}", section);
        assert!(section.contains("list_artifacts"), "got {}", section);
    }

    #[test]
    fn a_long_list_is_capped_and_the_remainder_counted() {
        let refs: Vec<ArtifactRef> = (0..15)
            .map(|i| artifact_ref(&format!("Doc {}", i), "doc", &format!("/store/{}.md", i)))
            .collect();

        let section = artifact_section(&refs, 0);

        assert!(section.contains("Doc 0"));
        assert!(!section.contains("Doc 14"), "past the cap");
        // 15 given, 10 listed, so 5 unlisted.
        assert!(section.contains('5'), "got {}", section);
    }

    #[test]
    fn omitted_and_capped_counts_add_up() {
        let refs: Vec<ArtifactRef> = (0..12)
            .map(|i| artifact_ref(&format!("Doc {}", i), "doc", &format!("/store/{}.md", i)))
            .collect();

        // 12 given (2 past the cap) plus 8 never passed in = 10 unlisted.
        let section = artifact_section(&refs, 8);

        assert!(section.contains("10"), "got {}", section);
    }
}
