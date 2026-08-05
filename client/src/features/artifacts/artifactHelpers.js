// Pure helpers for the artifacts browser. No React, no api.js — keep this unit-testable.
//
// An artifact is the shape returned by the `list_artifacts` command:
// { rel_path, name, dir, title, preview, kind, size_bytes, modified_at, tasks: [...] }

export const ARTIFACT_KINDS = ['all', 'plan', 'rfc', 'spec', 'readme', 'doc', 'other'];

export const KIND_LABEL_KEYS = ARTIFACT_KINDS.reduce((acc, kind) => {
  acc[kind] = `artifacts.kind.${kind}`;
  return acc;
}, {});

const MINUTE = 60 * 1000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const KB = 1024;
const MB = KB * 1024;

/** Case-insensitive substring match of `query` against name, rel_path and title. `kind: 'all'` matches everything. */
export function filterArtifacts(artifacts, { query = '', kind = 'all' } = {}) {
  if (!Array.isArray(artifacts)) return [];
  const needle = String(query ?? '')
    .trim()
    .toLowerCase();

  return artifacts.filter((artifact) => {
    if (!artifact) return false;
    if (kind !== 'all' && artifact.kind !== kind) return false;
    if (!needle) return true;
    return [artifact.name, artifact.rel_path, artifact.title].some(
      (field) => typeof field === 'string' && field.toLowerCase().includes(needle),
    );
  });
}

/** Groups artifacts into [{ dir, items }] — repo root first, then directories alphabetically. Item order is preserved. */
export function groupByDirectory(artifacts) {
  if (!Array.isArray(artifacts)) return [];

  const groups = new Map();
  for (const artifact of artifacts) {
    if (!artifact) continue;
    const dir = typeof artifact.dir === 'string' ? artifact.dir : '';
    if (!groups.has(dir)) groups.set(dir, []);
    groups.get(dir).push(artifact);
  }

  return [...groups.keys()]
    .sort((a, b) => {
      if (a === '') return -1;
      if (b === '') return 1;
      return a.localeCompare(b);
    })
    .map((dir) => ({ dir, items: groups.get(dir) }));
}

/** '820 B', '12.4 KB', '1.2 MB'. Returns '' for null, NaN or negative input. */
export function formatSize(bytes) {
  if (bytes === null || bytes === undefined) return '';
  const num = Number(bytes);
  if (!Number.isFinite(num) || num < 0) return '';
  if (num < KB) return `${Math.round(num)} B`;
  if (num < MB) return `${(num / KB).toFixed(1)} KB`;
  return `${(num / MB).toFixed(1)} MB`;
}

/** 'just now', '5m ago', '3h ago', '2d ago'; a locale date past 7 days. Returns '' for null or unparseable input. */
export function formatModified(iso, now = Date.now()) {
  if (!iso) return '';
  const then = new Date(iso).getTime();
  if (!Number.isFinite(then)) return '';

  const elapsed = now - then;
  if (elapsed < MINUTE) return 'just now';
  if (elapsed < HOUR) return `${Math.floor(elapsed / MINUTE)}m ago`;
  if (elapsed < DAY) return `${Math.floor(elapsed / HOUR)}h ago`;
  if (elapsed < 7 * DAY) return `${Math.floor(elapsed / DAY)}d ago`;
  return new Date(then).toLocaleDateString();
}
