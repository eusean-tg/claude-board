import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen } from '@testing-library/react';

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
