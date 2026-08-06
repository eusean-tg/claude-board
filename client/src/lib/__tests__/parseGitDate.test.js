import { describe, it, expect } from 'vitest';
import { parseGitDate, formatGitDate, gitDateToIso } from '../formatters';

describe('parseGitDate', () => {
  it("converts git's %ai shape to ISO regardless of what the engine would accept", () => {
    // The assertion that actually pins the fix. Parsing alone cannot: Node accepts
    // the space-separated form, so a parse-then-fallback design tests green here
    // and still renders "Invalid Date" in the app's WebKit engine.
    expect(gitDateToIso('2026-08-06 12:36:47 +0800')).toBe('2026-08-06T12:36:47+08:00');
    expect(gitDateToIso('2026-08-06 12:36:47 -0500')).toBe('2026-08-06T12:36:47-05:00');
    // Already ISO, left alone.
    expect(gitDateToIso('2026-08-06T12:36:47+08:00')).toBe('2026-08-06T12:36:47+08:00');
    // Not a git date, passed through for the Date constructor to reject.
    expect(gitDateToIso('whenever')).toBe('whenever');
  });

  it("reads git's %ai format, which WebKit rejects outright", () => {
    // This is the shape that rendered every commit as "Invalid Date" in the app:
    // Node parses it leniently, so it looked fine in tests and broke in the window.
    const d = parseGitDate('2026-08-06 12:36:47 +0800');

    expect(d).not.toBeNull();
    expect(d.getTime()).toBe(Date.parse('2026-08-06T12:36:47+08:00'));
  });

  it("reads git's %aI format, which is what new commits are recorded in", () => {
    const d = parseGitDate('2026-08-06T12:36:47+08:00');

    expect(d.getTime()).toBe(Date.parse('2026-08-06T12:36:47+08:00'));
  });

  it('handles a negative offset', () => {
    const d = parseGitDate('2026-08-06 12:36:47 -0500');

    expect(d.getTime()).toBe(Date.parse('2026-08-06T12:36:47-05:00'));
  });

  it('returns null rather than an Invalid Date object', () => {
    // Callers render the result directly, so "Invalid Date" must never reach them.
    for (const bad of [null, undefined, '', 'not a date', 'whenever']) {
      expect(parseGitDate(bad)).toBeNull();
    }
  });

  it('formats to an empty string when it cannot be read', () => {
    expect(formatGitDate('nonsense')).toBe('');
    expect(formatGitDate(null)).toBe('');
    expect(formatGitDate('2026-08-06 12:36:47 +0800')).not.toBe('');
  });
});
