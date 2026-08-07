import { describe, it, expect, vi, beforeEach } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';

vi.mock('../../lib/api', () => ({
  api: {
    updateStatus: vi.fn(),
    planPrerequisites: vi.fn(),
    startTaskWithPrerequisites: vi.fn(),
    getTasks: vi.fn(),
  },
}));
vi.mock('../../features/board/StatusTransitionContext', () => ({
  emitStatusTransition: vi.fn(),
}));

let useTaskHandlers;
let api;
beforeEach(async () => {
  vi.clearAllMocks();
  useTaskHandlers = (await import('../useTaskHandlers')).useTaskHandlers;
  api = (await import('../../lib/api')).api;
  api.planPrerequisites.mockResolvedValue([]);
  api.updateStatus.mockResolvedValue({ id: 1, status: 'in_progress' });
  api.getTasks.mockResolvedValue([]);
});

/** The handlers, plus the setters we assert on. */
function setup(tasks) {
  const spies = {
    setConfirm: vi.fn(),
    setPrerequisites: vi.fn(),
    setRunStopped: vi.fn(),
    setTasks: vi.fn(),
    addToast: vi.fn(),
  };
  const { result } = renderHook(() =>
    useTaskHandlers({
      tasks,
      t: (k) => k,
      terminal: { openTab: vi.fn() },
      setSelectedTask: vi.fn(),
      setActivePanel: vi.fn(),
      openModal: vi.fn(),
      closeModal: vi.fn(),
      currentProject: { id: 1 },
      ...spies,
    }),
  );
  return { handlers: result.current, ...spies };
}

const task = (over = {}) => ({ id: 1, project_id: 1, title: 'dep d', status: 'backlog', ...over });

describe('onStatusChange with a stopped run', () => {
  it('opens the resolution panel instead of starting the task', async () => {
    const { handlers, setRunStopped, setConfirm } = setup([task({ run_stopped: true })]);

    await act(async () => handlers.onStatusChange(1, 'in_progress'));

    // The start must not reach the backend: the trunk is missing work this task
    // depends on, and the panel is where that decision belongs.
    expect(api.updateStatus).not.toHaveBeenCalled();
    expect(setConfirm).not.toHaveBeenCalled();
    expect(setRunStopped).toHaveBeenCalledWith(expect.objectContaining({ startable: true }));
  });

  it('does not ask what would run first', async () => {
    const { handlers } = setup([task({ run_stopped: true })]);

    await act(async () => handlers.onStatusChange(1, 'in_progress'));

    // The prerequisite plan would name a sibling that is itself only waiting on the
    // same stopped run, which explains nothing and buries the actual cause.
    expect(api.planPrerequisites).not.toHaveBeenCalled();
  });

  it('leaves a resume alone', async () => {
    const { handlers, setRunStopped, setConfirm } = setup([task({ run_stopped: true, status: 'blocked' })]);

    await act(async () => handlers.onStatusChange(1, 'in_progress'));

    // A member still in flight when the run stopped keeps going; answering its
    // blocker passes back through In Progress and must not be intercepted.
    expect(setRunStopped).not.toHaveBeenCalled();
    expect(setConfirm).toHaveBeenCalled();
  });

  it('leaves a move to any other column alone', async () => {
    const { handlers, setRunStopped } = setup([task({ run_stopped: true })]);

    await act(async () => handlers.onStatusChange(1, 'failed'));

    expect(setRunStopped).not.toHaveBeenCalled();
    await waitFor(() => expect(api.updateStatus).toHaveBeenCalledWith(1, 'failed'));
  });

  it('confirms a normal start as before', async () => {
    const { handlers, setRunStopped, setConfirm } = setup([task()]);

    await act(async () => handlers.onStatusChange(1, 'in_progress'));

    expect(setRunStopped).not.toHaveBeenCalled();
    expect(setConfirm).toHaveBeenCalled();
  });
});

describe('onRunStopped', () => {
  it('opens the panel without the start option when called from a card', () => {
    const { handlers, setRunStopped } = setup([task({ run_stopped: true })]);

    act(() => handlers.onRunStopped(task({ run_stopped: true })));

    expect(setRunStopped).toHaveBeenCalledWith(expect.objectContaining({ startable: undefined }));
  });

  it('refreshes the board once the run is resolved', async () => {
    const { handlers, setRunStopped, setTasks } = setup([task({ run_stopped: true })]);
    act(() => handlers.onRunStopped(task({ run_stopped: true }), { startable: true }));
    const { onResolved } = setRunStopped.mock.calls[0][0];

    await act(async () => onResolved({ kind: 'startedAnyway', result: {} }));

    // Every card in the run carries the marker, so only a refetch clears them all.
    await waitFor(() => expect(setTasks).toHaveBeenCalled());
  });
});
