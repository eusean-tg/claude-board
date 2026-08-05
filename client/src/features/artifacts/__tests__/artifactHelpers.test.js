import { describe, it, expect } from 'vitest';

import {
  ARTIFACT_KINDS,
  KIND_LABEL_KEYS,
  filterArtifacts,
  groupByDirectory,
  formatSize,
  formatModified,
} from '../artifactHelpers';

const artifact = (overrides = {}) => ({
  rel_path: 'README.md',
  name: 'README.md',
  dir: '',
  title: 'Readme',
  preview: 'hello',
  kind: 'readme',
  size_bytes: 820,
  modified_at: '2026-08-05T12:00:00Z',
  tasks: [],
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
    artifact({ rel_path: 'README.md', name: 'README.md', title: 'Readme', kind: 'readme' }),
    artifact({
      rel_path: 'docs/rfcs/0001-queue.md',
      name: '0001-queue.md',
      title: 'Queue RFC',
      dir: 'docs/rfcs',
      kind: 'rfc',
    }),
    artifact({ rel_path: '.planning/PLAN.md', name: 'PLAN.md', title: 'Phase 3 plan', dir: '.planning', kind: 'plan' }),
  ];

  it('returns everything with no options', () => {
    expect(filterArtifacts(artifacts)).toHaveLength(3);
    expect(filterArtifacts(artifacts, {})).toHaveLength(3);
  });

  it('matches the query case-insensitively against name, rel_path and title', () => {
    expect(filterArtifacts(artifacts, { query: 'QUEUE' }).map((a) => a.name)).toEqual(['0001-queue.md']);
    expect(filterArtifacts(artifacts, { query: '.planning/' }).map((a) => a.name)).toEqual(['PLAN.md']);
    expect(filterArtifacts(artifacts, { query: 'phase 3' }).map((a) => a.name)).toEqual(['PLAN.md']);
    expect(filterArtifacts(artifacts, { query: '  readme  ' })).toHaveLength(1);
    expect(filterArtifacts(artifacts, { query: 'nothing here' })).toEqual([]);
  });

  it('filters by kind, with "all" matching everything', () => {
    expect(filterArtifacts(artifacts, { kind: 'all' })).toHaveLength(3);
    expect(filterArtifacts(artifacts, { kind: 'rfc' }).map((a) => a.name)).toEqual(['0001-queue.md']);
    expect(filterArtifacts(artifacts, { kind: 'spec' })).toEqual([]);
  });

  it('combines query and kind', () => {
    expect(filterArtifacts(artifacts, { query: 'md', kind: 'plan' }).map((a) => a.name)).toEqual(['PLAN.md']);
    expect(filterArtifacts(artifacts, { query: 'queue', kind: 'plan' })).toEqual([]);
  });

  it('tolerates missing fields, null entries and non-array input', () => {
    const ragged = [artifact({ name: null, title: null, rel_path: 'docs/notes.md' }), null, undefined];
    expect(filterArtifacts(ragged, { query: 'notes' })).toHaveLength(1);
    expect(filterArtifacts(ragged, { query: 'readme' })).toEqual([]);
    expect(filterArtifacts(null)).toEqual([]);
    expect(filterArtifacts(undefined, { query: 'x' })).toEqual([]);
    expect(filterArtifacts('README.md')).toEqual([]);
  });
});

describe('groupByDirectory', () => {
  it('puts the repo root first and sorts the rest alphabetically', () => {
    const grouped = groupByDirectory([
      artifact({ name: 'b.md', dir: 'docs' }),
      artifact({ name: 'root.md', dir: '' }),
      artifact({ name: 'a.md', dir: '.planning' }),
      artifact({ name: 'c.md', dir: 'docs' }),
    ]);

    expect(grouped.map((g) => g.dir)).toEqual(['', '.planning', 'docs']);
    expect(grouped[2].items.map((a) => a.name)).toEqual(['b.md', 'c.md']);
  });

  it('treats a missing dir as the repo root', () => {
    const grouped = groupByDirectory([
      artifact({ name: 'x.md', dir: undefined }),
      artifact({ name: 'y.md', dir: 'docs' }),
    ]);
    expect(grouped[0]).toEqual({ dir: '', items: [expect.objectContaining({ name: 'x.md' })] });
  });

  it('returns [] for empty, null and non-array input', () => {
    expect(groupByDirectory([])).toEqual([]);
    expect(groupByDirectory(null)).toEqual([]);
    expect(groupByDirectory({ dir: 'docs' })).toEqual([]);
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
