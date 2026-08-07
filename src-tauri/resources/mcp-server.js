#!/usr/bin/env node

/**
 * Claude Board MCP Server
 *
 * Exposes task management tools to Claude via the Model Context Protocol.
 * Runs as a stdio server — Claude Code spawns it as a subprocess.
 *
 * Tools: list_projects, list_tasks, create_task, update_task, change_task_status,
 *        get_task_detail, delete_task, list_task_summary, list_artifacts, save_artifact,
 *        update_artifact, raise_blocker
 */

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import { z } from 'zod';

const BASE_URL = process.env.CLAUDE_BOARD_URL || 'http://localhost:4000';

/// A field the board stores as a JSON string but serves parsed, read as a list either
/// way. `JSON.parse` on an array coerces it to a string first, so parsing an
/// already-parsed `[]` asks it to parse `''` — "Unexpected end of JSON input", on every
/// task that has no commits.
function asList(value) {
  if (Array.isArray(value)) return value;
  if (typeof value !== 'string' || value.trim() === '') return [];
  try {
    const parsed = JSON.parse(value);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

async function api(path, options = {}) {
  // eslint-disable-next-line no-undef
  const res = await fetch(`${BASE_URL}${path}`, {
    headers: { 'Content-Type': 'application/json' },
    ...options,
  });
  if (!res.ok) {
    const err = await res.json().catch(() => ({ error: res.statusText }));
    throw new Error(err.error || `API error: ${res.status}`);
  }
  return res.json();
}

const server = new McpServer({
  name: 'claude-board',
  version: '4.0.0',
});

// ─── list_projects ───
server.tool('list_projects', 'List all projects with task counts and stats', {}, async () => {
  const projects = await api('/api/projects/summary');
  const text = projects
    .map(
      (p) =>
        `[${p.id}] ${p.name} (${p.slug}) — ${p.total_tasks} tasks (${p.active_tasks} active, ${p.done_tasks} done, ${p.backlog_tasks} backlog)`,
    )
    .join('\n');
  return { content: [{ type: 'text', text: text || 'No projects found.' }] };
});

// ─── list_tasks ───
server.tool(
  'list_tasks',
  'List all tasks for a project. Returns task keys, titles, status, type, and model.',
  { project_id: z.number().describe('Project ID') },
  async ({ project_id }) => {
    const tasks = await api(`/api/projects/${project_id}/tasks`);
    if (tasks.length === 0) return { content: [{ type: 'text', text: 'No tasks in this project.' }] };

    const lines = tasks.map(
      (t) =>
        `[${t.task_key || '#' + t.id}] ${t.title} — status: ${t.status}, type: ${t.task_type}, model: ${t.model || 'sonnet'}${t.is_running ? ' (RUNNING)' : ''}`,
    );
    return { content: [{ type: 'text', text: lines.join('\n') }] };
  },
);

// ─── create_task ───
server.tool(
  'create_task',
  'Create a new task in a project. Use parent_task_id to create sub-tasks that are linked to a parent — the parent will automatically wait for all sub-tasks to complete.',
  {
    project_id: z.number().describe('Project ID to create the task in'),
    title: z.string().describe('Task title — clear and concise'),
    description: z.string().optional().describe('Detailed description or prompt for Claude'),
    task_type: z
      .enum(['feature', 'bugfix', 'refactor', 'docs', 'test', 'chore'])
      .optional()
      .default('feature')
      .describe('Task type'),
    priority: z.number().min(0).max(3).optional().default(0).describe('Priority: 0=none, 1=low, 2=medium, 3=high'),
    model: z.enum(['haiku', 'sonnet', 'opus']).optional().default('sonnet').describe('Claude model to use'),
    acceptance_criteria: z.string().optional().describe('Definition of done — what must be true when task completes'),
    parent_task_id: z
      .number()
      .optional()
      .describe(
        'Parent task ID — creates a sub-task linked to the parent. The parent will wait for all sub-tasks to complete before finishing.',
      ),
    tags: z.array(z.string()).optional().describe('Tags/labels for the task (e.g. ["backend", "security"])'),
  },
  async ({ project_id, title, description, task_type, priority, model, acceptance_criteria, parent_task_id, tags }) => {
    const task = await api(`/api/projects/${project_id}/tasks`, {
      method: 'POST',
      body: JSON.stringify({
        title,
        description: description || '',
        task_type: task_type || 'feature',
        priority: priority || 0,
        model: model || 'sonnet',
        acceptance_criteria: acceptance_criteria || '',
        parent_task_id: parent_task_id || null,
        tags: tags ? JSON.stringify(tags) : '[]',
      }),
    });
    const parentInfo = parent_task_id ? ` (sub-task of #${parent_task_id})` : '';
    return {
      content: [
        {
          type: 'text',
          text: `Task created: ${task.task_key || '#' + task.id} — "${task.title}" (${task.task_type}, ${task.model}, priority: ${task.priority})${parentInfo}`,
        },
      ],
    };
  },
);

// ─── update_task ───
server.tool(
  'update_task',
  'Update an existing task (title, description, type, priority, model).',
  {
    task_id: z.number().describe('Task ID to update'),
    title: z.string().optional().describe('New title'),
    description: z.string().optional().describe('New description'),
    task_type: z.enum(['feature', 'bugfix', 'refactor', 'docs', 'test', 'chore']).optional().describe('New type'),
    priority: z.number().min(0).max(3).optional().describe('New priority'),
    model: z.enum(['haiku', 'sonnet', 'opus']).optional().describe('New model'),
    acceptance_criteria: z.string().optional().describe('New acceptance criteria'),
  },
  async ({ task_id, ...updates }) => {
    // Get current task to merge with updates
    const current = await api(`/api/tasks/${task_id}`);
    const data = {
      title: updates.title || current.title,
      description: updates.description !== undefined ? updates.description : current.description,
      task_type: updates.task_type || current.task_type,
      priority: updates.priority !== undefined ? updates.priority : current.priority,
      model: updates.model || current.model,
      acceptance_criteria:
        updates.acceptance_criteria !== undefined ? updates.acceptance_criteria : current.acceptance_criteria,
    };
    await api(`/api/tasks/${task_id}`, { method: 'PUT', body: JSON.stringify(data) });
    return { content: [{ type: 'text', text: `Task #${task_id} updated.` }] };
  },
);

// ─── change_task_status ───
server.tool(
  'change_task_status',
  'Move a task to a different status column (backlog, in_progress, testing, done). ' +
    'Not every move is allowed: the board refuses illegal transitions, a task whose ' +
    'dependencies have not finished, and a task belonging to a dependency run that ' +
    'stopped. A refusal comes back as an error naming what stands in the way — read it ' +
    'rather than retrying, since none of them clear on their own.',
  {
    task_id: z.number().describe('Task ID'),
    status: z
      .enum(['backlog', 'in_progress', 'testing', 'done'])
      .describe('New status. WARNING: moving to in_progress starts a Claude agent on the task.'),
  },
  async ({ task_id, status }) => {
    await api(`/api/tasks/${task_id}/status`, {
      method: 'PATCH',
      body: JSON.stringify({ status }),
    });
    const labels = { backlog: 'Backlog', in_progress: 'In Progress', testing: 'Testing', done: 'Done' };
    return { content: [{ type: 'text', text: `Task #${task_id} moved to ${labels[status]}.` }] };
  },
);

// ─── get_task_detail ───
server.tool(
  'get_task_detail',
  'Get full details of a task including commits, revisions, attachments, and usage stats.',
  { task_id: z.number().describe('Task ID') },
  async ({ task_id }) => {
    const d = await api(`/api/tasks/${task_id}/detail`);
    const commits = asList(d.commits);
    const lines = [
      `# ${d.task_key || '#' + d.id} — ${d.title}`,
      `Status: ${d.status} | Type: ${d.task_type} | Model: ${d.model} | Priority: ${d.priority}`,
      d.description ? `\nDescription:\n${d.description}` : '',
      d.acceptance_criteria ? `\nAcceptance Criteria:\n${d.acceptance_criteria}` : '',
      d.branch_name ? `Branch: ${d.branch_name}` : '',
      d.is_running ? '⚡ Currently running' : '',
      `\nTokens: ${(d.input_tokens || 0).toLocaleString()} in / ${(d.output_tokens || 0).toLocaleString()} out`,
      d.total_cost > 0 ? `Cost: $${d.total_cost.toFixed(4)}` : '',
      // Each commit is an object — short/message/author/date — so joining the list
      // renders "[object Object]" per commit. The short hash and subject are what a
      // reader wants; the UI's Git tab shows the same two.
      commits.length > 0
        ? `\nCommits (${commits.length}):\n${commits
            .map((c) => `  ${c.short || c.hash || ''} ${c.message || ''}`.trimEnd())
            .join('\n')}`
        : '',
      d.revisions?.length > 0
        ? `\nRevisions (${d.revisions.length}):\n${d.revisions.map((r) => `  #${r.revision_number}: ${r.feedback}`).join('\n')}`
        : '',
    ].filter(Boolean);
    return { content: [{ type: 'text', text: lines.join('\n') }] };
  },
);

// ─── delete_task ───
server.tool(
  'delete_task',
  'Permanently delete a task. This cannot be undone.',
  { task_id: z.number().describe('Task ID to delete') },
  async ({ task_id }) => {
    await api(`/api/tasks/${task_id}`, { method: 'DELETE' });
    return { content: [{ type: 'text', text: `Task #${task_id} deleted.` }] };
  },
);

// ─── list_task_summary ───
server.tool(
  'list_task_summary',
  'Get a summary of tasks grouped by status for a project.',
  { project_id: z.number().describe('Project ID') },
  async ({ project_id }) => {
    const tasks = await api(`/api/projects/${project_id}/tasks`);
    const groups = { backlog: [], in_progress: [], testing: [], done: [] };
    tasks.forEach((t) => {
      if (groups[t.status]) groups[t.status].push(t);
    });

    const lines = [];
    for (const [status, items] of Object.entries(groups)) {
      const label = { backlog: 'Backlog', in_progress: 'In Progress', testing: 'Testing', done: 'Done' }[status];
      lines.push(`\n## ${label} (${items.length})`);
      items.forEach((t) => {
        lines.push(`  - [${t.task_key || '#' + t.id}] ${t.title} (${t.task_type}, ${t.model})`);
      });
    }
    return { content: [{ type: 'text', text: `# Project Tasks\nTotal: ${tasks.length}${lines.join('\n')}` }] };
  },
);

// ─── Artifacts ───
//
// The store is for documents about the work — plans, RFCs, specs, notes. Files
// that belong to the codebase go in the repository as normal. Getting that
// distinction wrong in either direction is the failure mode: a docs-site page
// saved as an artifact is noise, and a plan written only into the repo is lost
// when its branch is.

server.tool(
  'list_artifacts',
  'List markdown documents stored for a project — plans, RFCs, specs and notes saved ' +
    'by earlier tasks. Each entry gives an id, title, kind, tags, and the absolute ' +
    'path of the live copy on the second line. Read or edit a document with your own ' +
    'file tools at that path. Never search the filesystem for the store: the path here ' +
    'is the answer, and a copy you find elsewhere is not the one the board tracks.',
  {
    projectId: z.number().describe('Project ID'),
    tag: z.string().optional().describe('Only documents carrying this tag'),
  },
  async ({ projectId, tag }) => {
    const artifacts = await api(`/api/projects/${projectId}/artifacts`);
    const wanted = tag ? artifacts.filter((a) => asList(a.tags).includes(tag)) : artifacts;
    // The path is the point of the listing: it is how the document gets read. Leaving
    // it out sent agents hunting through the home directory for the store, which the
    // tool's own description said they would not have to do.
    const text = wanted
      .map((a) =>
        [`[${a.id}] ${a.title || a.stored_name} (${a.kind}) ${a.tags || '[]'}`, a.path ? `\n    ${a.path}` : '']
          .join('')
          .trimEnd(),
      )
      .join('\n');
    return {
      content: [{ type: 'text', text: text || 'No documents stored for this project.' }],
    };
  },
);

server.tool(
  'save_artifact',
  'Save a markdown document to Claude Board so the user can browse it and later tasks ' +
    'can reference it. Use this for documents about the work — a plan, an RFC, a spec, ' +
    'research notes, a progress log. Do NOT use it for files that belong in the ' +
    'codebase; write those to the repository as usual. Give a real title and tag the ' +
    'document so it can be found later.',
  {
    projectId: z.number().describe('Project ID'),
    title: z.string().describe('Human-readable title, e.g. "Auth rollout plan"'),
    kind: z.enum(['plan', 'rfc', 'spec', 'readme', 'doc', 'other']).describe('What kind of document this is'),
    content: z.string().describe('The full markdown body'),
    tags: z
      .array(z.string())
      .optional()
      .describe('Tags for finding this later, e.g. ["context"] for project-wide context'),
    taskId: z.number().optional().describe('The task saving this, for attribution'),
  },
  async ({ projectId, title, kind, content, tags, taskId }) => {
    const saved = await api(`/api/projects/${projectId}/artifacts`, {
      method: 'POST',
      body: JSON.stringify({ title, kind, content, tags, task_id: taskId }),
    });
    return {
      content: [
        {
          type: 'text',
          text: `Saved artifact ${saved.id} — "${title}" (${kind}) at ${saved.path}`,
        },
      ],
    };
  },
);

server.tool(
  'update_artifact',
  'Revise a stored document. Pass only what changes: sending just `content` rewrites ' +
    'the body and leaves the title, kind and tags as they are. Use this rather than ' +
    'saving a second copy when a document already exists.',
  {
    id: z.number().describe('Artifact ID, from list_artifacts'),
    content: z.string().optional().describe('Replacement markdown body'),
    title: z.string().optional(),
    kind: z.enum(['plan', 'rfc', 'spec', 'readme', 'doc', 'other']).optional(),
    tags: z.array(z.string()).optional().describe('Replaces the existing tags entirely'),
    taskId: z.number().optional().describe('The task making this change, for attribution'),
  },
  async ({ id, content, title, kind, tags, taskId }) => {
    const saved = await api(`/api/artifacts/${id}`, {
      method: 'PATCH',
      body: JSON.stringify({ content, title, kind, tags, task_id: taskId }),
    });
    return {
      content: [{ type: 'text', text: `Updated artifact ${saved.id} at ${saved.path}` }],
    };
  },
);

// ─── raise_blocker ───
server.tool(
  'raise_blocker',
  'Ask the user a question and wait for their answer. Use this when you cannot make ' +
    'progress without a decision only they can make — an ambiguous requirement, a ' +
    'choice between approaches with real trade-offs, a missing credential, work that ' +
    'would touch something outside the task. Prefer offering options over free text: ' +
    'name the choices you would pick between and say what each one means. Do NOT use ' +
    'this for decisions you can reasonably make yourself, or to confirm work you have ' +
    'already done. One question at a time — a task can have only one open blocker.',
  {
    taskId: z.number().describe('The task you are working on'),
    kind: z
      .enum(['single_choice', 'multi_choice', 'free_text'])
      .describe(
        'single_choice for one of several options, multi_choice when several can ' +
          'apply together, free_text when you cannot enumerate the answers',
      ),
    question: z.string().describe('The question, in one or two sentences'),
    header: z.string().optional().describe('Two or three words naming the decision, e.g. "Auth flow"'),
    context: z
      .string()
      .optional()
      .describe('What you established before getting stuck, so the user need not re-derive it'),
    options: z
      .array(
        z.object({
          label: z.string().describe('The choice, short enough to read at a glance'),
          description: z.string().optional().describe('What picking this one means'),
        }),
      )
      .optional()
      .describe('Required for single_choice and multi_choice'),
    artifactId: z.number().optional().describe('The document the question is about, if there is one'),
    waitSeconds: z
      .number()
      .optional()
      .describe('How long to wait. Defaults to 5 minutes; leave unset unless you have a reason'),
  },
  async ({ taskId, kind, question, header, context, options, artifactId, waitSeconds }) => {
    const result = await api(`/api/tasks/${taskId}/blockers`, {
      method: 'POST',
      body: JSON.stringify({ kind, question, header, context, options, artifactId, waitSeconds }),
    });
    if (result.answered) {
      return {
        content: [
          {
            type: 'text',
            text: `The user answered: ${result.summary}\n\nCarry on with the task using that answer.`,
          },
        ],
      };
    }
    // Not an error: nobody was there to answer. Stopping cleanly is the right
    // outcome — the task stays blocked and is resumed with the answer later, in
    // this worktree, so uncommitted work is kept.
    return {
      content: [
        {
          type: 'text',
          text:
            'Nobody answered in time. Stop working on this task now. Leave your ' +
            'changes exactly as they are — do not revert, clean up, or guess at the ' +
            'answer. Summarise what you completed and what you were waiting on, then ' +
            'end your turn. The task stays blocked and will be resumed with the ' +
            'answer.',
        },
      ],
    };
  },
);

// ─── Start server ───
async function main() {
  const transport = new StdioServerTransport();
  await server.connect(transport);
}

main().catch((err) => {
  console.error('MCP server error:', err);
  process.exit(1);
});
