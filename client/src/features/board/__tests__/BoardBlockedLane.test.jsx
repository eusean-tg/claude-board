import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen } from '@testing-library/react';

vi.mock('../../../lib/api', () => ({
  api: { getGitHubIssues: vi.fn().mockResolvedValue([]) },
  notifyError: vi.fn(),
}));
vi.mock('../../../lib/tauriEvents', () => ({ IS_TAURI: true }));
vi.mock('../../../i18n/I18nProvider', () => ({ useTranslation: () => ({ t: (k) => k }) }));
vi.mock('../ListView', () => ({ default: () => <div /> }));
vi.mock('../GitHubIssuesPanel', () => ({ default: () => <div /> }));
vi.mock('../../artifacts/ArtifactsView', () => ({ default: () => <div /> }));

// Column is stubbed to report the lane it was asked to render.
vi.mock('../Column', () => ({
  default: ({ column }) => <div data-testid={`lane-${column.id}`} />,
}));

let Board;
beforeEach(async () => {
  vi.clearAllMocks();
  Board = (await import('../Board')).default;
});

const props = (tasks, project = {}) => ({
  tasks,
  project: { id: 1, ...project },
  onStatusChange: () => {},
  onViewLogs: () => {},
  onEditTask: () => {},
  onDeleteTask: () => {},
});

describe('the Blocked lane', () => {
  it('is on the board even when nothing is blocked', () => {
    render(<Board {...props([{ id: 1, status: 'backlog' }])} />);

    // A lane that appears only once it has something in it is a lane nobody knows
    // exists until it surprises them.
    expect(screen.getByTestId('lane-blocked')).toBeInTheDocument();
  });

  it('holds a blocked task rather than dropping it', () => {
    render(<Board {...props([{ id: 1, status: 'blocked' }])} />);

    expect(screen.getByTestId('lane-blocked')).toBeInTheDocument();
  });

  it('still hides the approval lane unless the project opts in', () => {
    render(<Board {...props([{ id: 1, status: 'backlog' }])} />);
    expect(screen.queryByTestId('lane-awaiting_approval')).not.toBeInTheDocument();

    render(<Board {...props([{ id: 1, status: 'backlog' }], { require_approval: 1 })} />);
    expect(screen.getByTestId('lane-awaiting_approval')).toBeInTheDocument();
  });
});
