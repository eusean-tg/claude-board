import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';

vi.mock('../../../i18n/I18nProvider', () => ({
  useTranslation: () => ({ t: (k, vars) => (vars ? `${k} ${JSON.stringify(vars)}` : k) }),
}));
vi.mock('../StatusTransitionEffect', () => ({ default: () => null }));

let TaskCard;
beforeEach(async () => {
  vi.clearAllMocks();
  TaskCard = (await import('../TaskCard')).default;
});

const card = (overrides) => ({
  id: 1,
  title: 't',
  status: 'backlog',
  task_type: 'feature',
  ...overrides,
});

describe('TaskCard dependency state', () => {
  it('says what a not-yet-startable task is waiting for', () => {
    render(<TaskCard task={card({ waiting_on: 2 })} />);

    // The count is what makes the refused Start legible rather than broken.
    expect(screen.getByText(/"count":2/)).toBeInTheDocument();
  });

  it('does not call it blocked', () => {
    render(<TaskCard task={card({ waiting_on: 2 })} />);

    // `blocked` is taken: it means an agent asked a question and needs an answer.
    // A task waiting on another task needs no answer, and sharing the word would
    // teach the user that neither marker means anything specific.
    expect(screen.queryByText(/blocker\./)).not.toBeInTheDocument();
    expect(screen.queryByText(/status\.blocked/)).not.toBeInTheDocument();
  });

  it('says nothing when a task is ready to start', () => {
    render(<TaskCard task={card({ waiting_on: 0 })} />);

    expect(screen.queryByText(/waiting/i)).not.toBeInTheDocument();
  });

  it('says nothing when the field is absent', () => {
    // Tasks read through paths that do not compute it must not render a stray "0".
    render(<TaskCard task={card()} />);

    expect(screen.queryByText(/waiting/i)).not.toBeInTheDocument();
  });

  it('shows the shared branch for a task running in a group', () => {
    render(<TaskCard task={card({ trunk_branch: 'trunk/feature/x' })} />);

    expect(screen.getByText('trunk/feature/x')).toBeInTheDocument();
  });

  it('says a run stopped instead of naming its branch', () => {
    render(<TaskCard task={card({ trunk_branch: 'trunk/feature/x', run_stopped: true })} />);

    // A stopped run needs a person, and every task in it still reports a status
    // that looks fine — so the card has to say so rather than show a branch name
    // indistinguishable from a healthy run's.
    expect(screen.getByText('card.runStoppedShort')).toBeInTheDocument();
    expect(screen.queryByText('trunk/feature/x')).not.toBeInTheDocument();
  });

  it('names the branch to merge in the marker title', () => {
    render(<TaskCard task={card({ trunk_branch: 'trunk/feature/x', run_stopped: true })} />);

    // The instruction has to reach the user somewhere: the label is too small for it.
    expect(screen.getByTitle(/"trunk":"trunk\/feature\/x"/)).toBeInTheDocument();
  });

  it('opens the resolution panel from the marker', () => {
    const onRunStopped = vi.fn();
    render(
      <TaskCard task={card({ trunk_branch: 'trunk/feature/x', run_stopped: true })} onRunStopped={onRunStopped} />,
    );

    fireEvent.click(screen.getByText('card.runStoppedShort'));

    expect(onRunStopped).toHaveBeenCalled();
  });

  it('leaves a healthy run inert', () => {
    const onRunStopped = vi.fn();
    render(<TaskCard task={card({ trunk_branch: 'trunk/feature/x' })} onRunStopped={onRunStopped} />);

    fireEvent.click(screen.getByText('trunk/feature/x'));

    // Nothing to resolve, so the marker must not offer to resolve it.
    expect(onRunStopped).not.toHaveBeenCalled();
  });

  it('shows nothing about branches for an ungrouped task', () => {
    render(<TaskCard task={card()} />);

    expect(screen.queryByText(/^trunk\//)).not.toBeInTheDocument();
  });

  it('keeps the waiting marker off a task that has already started', () => {
    // A running task's dependencies were met when it started. A stale count would
    // contradict the fact that it is visibly running.
    render(<TaskCard task={card({ status: 'in_progress', is_running: true, waiting_on: 1 })} />);

    expect(screen.queryByText(/"count":1/)).not.toBeInTheDocument();
  });
});
