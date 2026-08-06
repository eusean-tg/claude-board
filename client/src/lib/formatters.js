export function formatTokens(n) {
  if (!n || n === 0) return '0';
  if (n >= 1e6) return `${(n / 1e6).toFixed(1)}M`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(1)}K`;
  return String(n);
}

export function formatDuration(startedAt, completedAt, workDurationMs = 0, lastResumedAt = null) {
  if (!startedAt) return null;
  let diffMs;
  if (workDurationMs > 0 || lastResumedAt) {
    // Use accumulated work duration + current active segment
    diffMs = workDurationMs || 0;
    if (lastResumedAt) {
      diffMs += Date.now() - new Date(lastResumedAt).getTime();
    }
  } else {
    // Fallback: old behavior for tasks without timer tracking
    const start = new Date(startedAt);
    const end = completedAt ? new Date(completedAt) : new Date();
    diffMs = end - start;
  }
  const mins = Math.floor(diffMs / 60000);
  const hours = Math.floor(mins / 60);
  const days = Math.floor(hours / 24);
  if (days > 0) return `${days}d ${hours % 24}h`;
  if (hours > 0) return `${hours}h ${mins % 60}m`;
  if (mins > 0) return `${mins}m`;
  return '<1m';
}

export function formatMs(ms) {
  if (!ms) return '';
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  return `${Math.floor(ms / 60000)}m${Math.floor((ms % 60000) / 1000)}s`;
}

export function formatTime(dateStr) {
  if (!dateStr) return '';
  return new Date(dateStr).toLocaleTimeString('en-US', {
    hour12: false,
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });
}

export function formatTimeAgo(dateStr) {
  const diff = Date.now() - new Date(dateStr).getTime();
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return 'just now';
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

export function basename(path) {
  if (!path) return null;
  return path.replace(/\\/g, '/').split('/').pop();
}

export function shortenPath(path) {
  if (!path) return '';
  const parts = path.replace(/\\/g, '/').split('/');
  if (parts.length <= 3) return parts.join('/');
  return '\u2026/' + parts.slice(-3).join('/');
}

// "2026-08-06 12:36:47 +0800" (git's %ai) and "2026-08-06T12:36:47+08:00" (%aI).
const GIT_DATE = /^(\d{4}-\d{2}-\d{2})[ T](\d{2}:\d{2}:\d{2})\s*([+-]\d{2}):?(\d{2})$/;

/**
 * A git date as strict ISO 8601, or the input unchanged when it is not a git date.
 *
 * Normalisation happens before parsing rather than after a failed attempt,
 * because whether an engine accepts the space-separated `%ai` form differs
 * between them: Node takes it, the WebKit engine Tauri uses on macOS does not.
 * Leaning on that leniency is what let "Invalid Date" reach the commit list while
 * every test passed.
 */
export function gitDateToIso(value) {
  const raw = String(value ?? '').trim();
  const m = raw.match(GIT_DATE);
  return m ? `${m[1]}T${m[2]}${m[3]}:${m[4]}` : raw;
}

/**
 * Parse a git author/commit date, in either format git has been asked for.
 *
 * Returns null for anything unparseable, so callers render nothing rather than
 * the words "Invalid Date".
 */
export function parseGitDate(value) {
  if (!value) return null;
  const parsed = new Date(gitDateToIso(value));
  return Number.isNaN(parsed.getTime()) ? null : parsed;
}

/** A commit date as a short local date, or '' when it cannot be read. */
export function formatGitDate(value) {
  const parsed = parseGitDate(value);
  return parsed ? parsed.toLocaleDateString() : '';
}
