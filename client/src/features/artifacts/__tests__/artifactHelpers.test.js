import { describe, it, expect } from 'vitest';

import {
  ARTIFACT_KINDS,
  KIND_LABEL_KEYS,
  filterArtifacts,
  groupByKind,
  formatSize,
  formatModified,
} from '../artifactHelpers';

const artifact = (overrides = {}) => ({
  id: 1,
  project_id: 1,
  stored_name: 'readme-1754400000.md',
  origin: 'README.md',
  title: 'Readme',
  preview: 'hello',
  kind: 'readme',
  size: 820,
  origin_task_id: null,
  last_task_id: null,
  created_at: '2026-08-05T12:00:00Z',
  updated_at: '2026-08-05T12:00:00Z',
  ...overrides,
});

describe('ARTIFACT_KINDS / KIND_LABEL_KEYS', () => {
  it('starts with the catch-all kind and maps every kind to an i18n key', () => {
    expect(ARTIFACT_KINDS[0]).toBe('all');
    expect(Object.keys(KIND_LABEL_KEYS)).toEqual(ARTIFACT_KINDS);
    expect(KIND_LABEL_KEYS.plan).toBe('artifacts.kind.plan');
    expect(KIND_LABEL_KEYS.other).toBe('artifacts.kind.other');
  });
});

describe('filterArtifacts', () => {
  const artifacts = [
    artifact({ id: 1, stored_name: 'readme-1.md', origin: 'README.md', title: 'Readme', kind: 'readme' }),
    artifact({
      id: 2,
      stored_name: '0001-queue-2.md',
      origin: 'docs/rfcs/0001-queue.md',
      title: 'Queue RFC',
      kind: 'rfc',
    }),
    artifact({
      id: 3,
      stored_name: 'plan-3.md',
      origin: '.planning/PLAN.md',
      title: 'Phase 3 plan',
      kind: 'plan',
    }),
  ];

  it('returns everything with no options', () => {
    expect(filterArtifacts(artifacts)).toHaveLength(3);
    expect(filterArtifacts(artifacts, {})).toHaveLength(3);
  });

  it('matches the query case-insensitively against the stored name, source path and title', () => {
    expect(filterArtifacts(artifacts, { query: 'QUEUE' }).map((a) => a.id)).toEqual([2]);
    expect(filterArtifacts(artifacts, { query: '.planning/' }).map((a) => a.id)).toEqual([3]);
    expect(filterArtifacts(artifacts, { query: 'phase 3' }).map((a) => a.id)).toEqual([3]);
    expect(filterArtifacts(artifacts, { query: '  readme  ' })).toHaveLength(1);
    expect(filterArtifacts(artifacts, { query: 'nothing here' })).toEqual([]);
  });

  it('filters by kind, with "all" matching everything', () => {
    expect(filterArtifacts(artifacts, { kind: 'all' })).toHaveLength(3);
    expect(filterArtifacts(artifacts, { kind: 'rfc' }).map((a) => a.id)).toEqual([2]);
    expect(filterArtifacts(artifacts, { kind: 'spec' })).toEqual([]);
  });

  it('combines query and kind', () => {
    expect(filterArtifacts(artifacts, { query: 'md', kind: 'plan' }).map((a) => a.id)).toEqual([3]);
    expect(filterArtifacts(artifacts, { query: 'queue', kind: 'plan' })).toEqual([]);
  });

  it('tolerates missing fields, null entries and non-array input', () => {
    const ragged = [artifact({ stored_name: null, title: null, origin: 'docs/notes.md' }), null, undefined];
    expect(filterArtifacts(ragged, { query: 'notes' })).toHaveLength(1);
    expect(filterArtifacts(ragged, { query: 'readme' })).toEqual([]);
    expect(filterArtifacts(null)).toEqual([]);
    expect(filterArtifacts(undefined, { query: 'x' })).toEqual([]);
    expect(filterArtifacts('README.md')).toEqual([]);
  });
});

describe('groupByKind', () => {
  it('orders groups by the canonical kind order and drops empty ones', () => {
    const groups = groupByKind([
      artifact({ id: 1, kind: 'doc' }),
      artifact({ id: 2, kind: 'plan' }),
      artifact({ id: 3, kind: 'plan' }),
    ]);

    expect(groups.map((g) => g.kind)).toEqual(['plan', 'doc']);
    expect(groups[0].artifacts.map((a) => a.id)).toEqual([2, 3]);
  });

  it('never lists the catch-all "all" pseudo-kind as a group', () => {
    const groups = groupByKind([artifact({ kind: 'readme' })]);
    expect(groups.map((g) => g.kind)).not.toContain('all');
  });

  it('treats a missing kind as other', () => {
    const groups = groupByKind([artifact({ id: 9, kind: undefined })]);
    expect(groups).toEqual([{ kind: 'other', artifacts: [expect.objectContaining({ id: 9 })] }]);
  });

  it('keeps a kind the frontend does not know about rather than dropping it', () => {
    // A kind added backend-side must still be visible, at the end of the list.
    const groups = groupByKind([artifact({ id: 1, kind: 'plan' }), artifact({ id: 2, kind: 'runbook' })]);
    expect(groups.map((g) => g.kind)).toEqual(['plan', 'runbook']);
  });

  it('returns [] for empty, null and non-array input', () => {
    expect(groupByKind([])).toEqual([]);
    expect(groupByKind(null)).toEqual([]);
    expect(groupByKind('nope')).toEqual([]);
  });
});

describe('formatSize', () => {
  it('formats bytes, kilobytes and megabytes', () => {
    expect(formatSize(0)).toBe('0 B');
    expect(formatSize(820)).toBe('820 B');
    expect(formatSize(1023)).toBe('1023 B');
    expect(formatSize(1024)).toBe('1.0 KB');
    expect(formatSize(12_698)).toBe('12.4 KB');
    expect(formatSize(1.2 * 1024 * 1024)).toBe('1.2 MB');
    expect(formatSize(5 * 1024 * 1024)).toBe('5.0 MB');
  });

  it('returns "" for null, undefined, NaN and negative sizes', () => {
    expect(formatSize(null)).toBe('');
    expect(formatSize(undefined)).toBe('');
    expect(formatSize(NaN)).toBe('');
    expect(formatSize('nope')).toBe('');
    expect(formatSize(-1)).toBe('');
  });
});

describe('formatModified', () => {
  const now = new Date('2026-08-05T12:00:00Z').getTime();
  const ago = (ms) => new Date(now - ms).toISOString();

  it('formats recent timestamps relatively', () => {
    expect(formatModified(ago(0), now)).toBe('just now');
    expect(formatModified(ago(45 * 1000), now)).toBe('just now');
    expect(formatModified(ago(5 * 60 * 1000), now)).toBe('5m ago');
    expect(formatModified(ago(59 * 60 * 1000), now)).toBe('59m ago');
    expect(formatModified(ago(3 * 60 * 60 * 1000), now)).toBe('3h ago');
    expect(formatModified(ago(2 * 24 * 60 * 60 * 1000), now)).toBe('2d ago');
    expect(formatModified(ago(6 * 24 * 60 * 60 * 1000), now)).toBe('6d ago');
  });

  it('falls back to a locale date past 7 days', () => {
    const old = ago(30 * 24 * 60 * 60 * 1000);
    expect(formatModified(old, now)).toBe(new Date(old).toLocaleDateString());
    expect(formatModified(ago(7 * 24 * 60 * 60 * 1000), now)).not.toMatch(/ago$/);
  });

  it('treats future timestamps as just now', () => {
    expect(formatModified(new Date(now + 60_000).toISOString(), now)).toBe('just now');
  });

  it('returns "" for null and unparseable input', () => {
    expect(formatModified(null, now)).toBe('');
    expect(formatModified(undefined, now)).toBe('');
    expect(formatModified('', now)).toBe('');
    expect(formatModified('not a date', now)).toBe('');
  });
});
