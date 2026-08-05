import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, waitFor, fireEvent } from '@testing-library/react';

// Board pulls in the whole board surface; stub the leaves so this stays a wiring test.
vi.mock('../../../lib/api', () => ({
  api: { getGitHubIssues: vi.fn().mockResolvedValue([]) },
  notifyError: vi.fn(),
}));

vi.mock('../../../lib/tauriEvents', () => ({ IS_TAURI: true }));

vi.mock('../../../i18n/I18nProvider', () => ({
  useTranslation: () => ({ t: (k) => k }),
}));

vi.mock('../Column', () => ({ default: () => <div /> }));
vi.mock('../ListView', () => ({ default: () => <div /> }));
vi.mock('../GitHubIssuesPanel', () => ({ default: () => <div /> }));

// Records the props Board hands the real view, so the unfiltered-`tasks` contract is observable.
const artifactsProps = vi.fn();
vi.mock('../../artifacts/ArtifactsView', () => ({
  default: (props) => {
    artifactsProps(props);
    return <div data-testid="artifacts-view">artifacts</div>;
  },
}));

const TASKS = [
  { id: 1, status: 'done', model_used: 'sonnet', tags: '["api"]' },
  { id: 2, status: 'backlog', model_used: 'opus', tags: '["ui"]' },
];

let Board;

beforeEach(async () => {
  vi.clearAllMocks();
  ({ default: Board } = await import('../Board'));
});

afterEach(() => {
  vi.resetModules();
});

function renderBoard() {
  return render(<Board tasks={TASKS} projectId={42} project={{ name: 'repo' }} onViewDetail={vi.fn()} />);
}

describe('Board artifacts tab wiring', () => {
  it('shows an Artifacts tab in the view strip', () => {
    renderBoard();
    expect(screen.getByText('board.artifacts')).toBeInTheDocument();
  });

  it('lazily renders ArtifactsView when the tab is selected', async () => {
    renderBoard();
    expect(screen.queryByTestId('artifacts-view')).not.toBeInTheDocument();

    fireEvent.click(screen.getByText('board.artifacts'));

    await waitFor(() => expect(screen.getByTestId('artifacts-view')).toBeInTheDocument());
  });

  it('passes projectId, project and onViewDetail through', async () => {
    renderBoard();
    fireEvent.click(screen.getByText('board.artifacts'));
    await waitFor(() => expect(artifactsProps).toHaveBeenCalled());

    const props = artifactsProps.mock.calls.at(-1)[0];
    expect(props.projectId).toBe(42);
    expect(props.project).toEqual({ name: 'repo' });
    expect(typeof props.onViewDetail).toBe('function');
  });

  it('passes the unfiltered task list so attribution chips resolve under an active filter', async () => {
    renderBoard();

    // Narrow the board to a single model, then open the artifacts tab.
    fireEvent.click(screen.getByText('board.artifacts'));
    await waitFor(() => expect(artifactsProps).toHaveBeenCalled());
    fireEvent.click(screen.getByText('opus'));

    await waitFor(() => {
      const props = artifactsProps.mock.calls.at(-1)[0];
      expect(props.tasks).toHaveLength(TASKS.length);
    });
  });
});
