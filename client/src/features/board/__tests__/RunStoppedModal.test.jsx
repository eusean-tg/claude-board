import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';

const click = (el) => fireEvent.click(el);

vi.mock('../../../i18n/I18nProvider', () => ({
  useTranslation: () => ({ t: (k, vars) => (vars ? `${k} ${JSON.stringify(vars)}` : k) }),
}));
vi.mock('../../../lib/api', () => ({
  api: {
    resumeStoppedRun: vi.fn(),
    abandonRun: vi.fn(),
    startDespiteStoppedRun: vi.fn(),
    resolveStoppedRun: vi.fn(),
  },
}));

let RunStoppedModal;
let api;
beforeEach(async () => {
  vi.clearAllMocks();
  RunStoppedModal = (await import('../RunStoppedModal')).default;
  api = (await import('../../../lib/api')).api;
});

const task = () => ({ id: 7, title: 'wire the tab', trunk_branch: 'trunk/feature/x' });
const render_ = (props = {}) =>
  render(<RunStoppedModal task={task()} onClose={vi.fn()} onResolved={vi.fn()} {...props} />);

describe('RunStoppedModal', () => {
  it('names the branch that has to be merged', () => {
    render_();

    // The whole resolution is "merge this". Without the name it is not actionable.
    expect(screen.getByText('trunk/feature/x')).toBeInTheDocument();
  });

  it('carries the run on', async () => {
    api.resumeStoppedRun.mockResolvedValue({ resumed: true, merged: [], started: [2] });
    const onResolved = vi.fn();
    render_({ onResolved });

    click(screen.getByRole('button', { name: /runStopped.resume/i }));

    await waitFor(() => expect(api.resumeStoppedRun).toHaveBeenCalledWith(7));
    await waitFor(() => expect(onResolved).toHaveBeenCalled());
  });

  it('keeps the panel open and says why when the merge still fails', async () => {
    api.resumeStoppedRun.mockRejectedValue(new Error('feature/x still cannot be merged'));
    const onResolved = vi.fn();
    render_({ onResolved });

    click(screen.getByRole('button', { name: /runStopped.resume/i }));

    // Closing here would leave no sign the run did not move.
    await waitFor(() => expect(screen.getByText(/still cannot be merged/)).toBeInTheDocument());
    expect(onResolved).not.toHaveBeenCalled();
  });

  it('asks twice before abandoning', () => {
    render_();

    click(screen.getByRole('button', { name: /runStopped.abandon$/i }));

    // Abandoning releases the tasks and gives up the run, and its button sits next
    // to the one people mean to press.
    expect(screen.getByRole('button', { name: /runStopped.abandonConfirm/i })).toBeInTheDocument();
    expect(api.abandonRun).not.toHaveBeenCalled();
  });

  it('abandons on the second click', async () => {
    api.abandonRun.mockResolvedValue({ abandoned: true, trunk: 'trunk/feature/x' });
    const onResolved = vi.fn();
    render_({ onResolved });

    click(screen.getByRole('button', { name: /runStopped.abandon$/i }));
    click(screen.getByRole('button', { name: /runStopped.abandonConfirm/i }));

    await waitFor(() => expect(api.abandonRun).toHaveBeenCalledWith(7));
    await waitFor(() => expect(onResolved).toHaveBeenCalledWith(expect.objectContaining({ kind: 'abandoned' })));
  });

  it('renders nothing without a task', () => {
    const { container } = render(<RunStoppedModal task={null} onClose={vi.fn()} onResolved={vi.fn()} />);

    expect(container).toBeEmptyDOMElement();
  });

  it('offers no way to start the task when opened from the card marker', () => {
    render_();

    // Nothing is being started here, so the option would only invite making the
    // trunk worse than the stop left it.
    expect(screen.queryByRole('button', { name: /runStopped.startAnyway/i })).toBeNull();
  });

  it('warns before starting on a trunk that is missing work', () => {
    render_({ startable: true });

    // The warning is the point — this panel *is* the confirmation for that start.
    expect(screen.getByText(/runStopped.startAnywayWarning/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /runStopped.startAnyway/i })).toBeInTheDocument();
  });

  it('starts the task anyway on request', async () => {
    api.startDespiteStoppedRun.mockResolvedValue({ id: 7, status: 'in_progress' });
    const onResolved = vi.fn();
    render_({ startable: true, onResolved });

    click(screen.getByRole('button', { name: /runStopped.startAnyway/i }));

    await waitFor(() => expect(api.startDespiteStoppedRun).toHaveBeenCalledWith(7));
    await waitFor(() => expect(onResolved).toHaveBeenCalledWith(expect.objectContaining({ kind: 'startedAnyway' })));
  });

  it('keeps the panel open and says why when starting anyway fails', async () => {
    api.startDespiteStoppedRun.mockRejectedValue(new Error('Blocked by 1 unfinished task(s): dep a'));
    const onResolved = vi.fn();
    render_({ startable: true, onResolved });

    click(screen.getByRole('button', { name: /runStopped.startAnyway/i }));

    // Overriding the run does not clear an unmet prerequisite, and the refusal for
    // that is a different one the user still has to read.
    await waitFor(() => expect(screen.getByText(/dep a/)).toBeInTheDocument());
    expect(onResolved).not.toHaveBeenCalled();
  });
  it('offers to resolve the conflict with a task', async () => {
    api.resolveStoppedRun.mockResolvedValue({ resolveTaskId: 9, created: true });
    const onResolved = vi.fn();
    render_({ onResolved });

    click(screen.getByRole('button', { name: /runStopped.resolve$/i }));

    await waitFor(() => expect(api.resolveStoppedRun).toHaveBeenCalledWith(7));
    await waitFor(() => expect(onResolved).toHaveBeenCalledWith(expect.objectContaining({ kind: 'resolveStarted' })));
  });

  it('shows who is already resolving instead of offering it twice', () => {
    // The backend refuses a duplicate too, but the button existing at all would
    // read as "the first click did nothing".
    render_({ resolveTask: { id: 9, title: 'Resolve merge conflict', status: 'in_progress' } });

    expect(screen.queryByRole('button', { name: /runStopped.resolve$/i })).toBeNull();
    expect(screen.getByText(/runStopped.resolving/)).toBeInTheDocument();
  });

  it('treats a resolution waiting for review as in flight', () => {
    // Awaiting approval is not spent and not restartable — the user's next move is
    // reading the resolution, and the hint says so.
    render_({ resolveTask: { id: 9, title: 'Resolve merge conflict', status: 'awaiting_approval' } });

    expect(screen.queryByRole('button', { name: /runStopped.resolve$/i })).toBeNull();
    expect(screen.getByText(/runStopped.resolving/)).toBeInTheDocument();
  });

  it('offers to run a crashed resolve task again', () => {
    // Its attempt produced no resolution to judge, so this is not a second attempt.
    render_({ resolveTask: { id: 9, title: 'Resolve merge conflict', status: 'failed' } });

    expect(screen.getByRole('button', { name: /runStopped.resolve$/i })).toBeInTheDocument();
  });

  it('says when the resolve attempt is spent', () => {
    render_({ resolveTask: { id: 9, title: 'Resolve merge conflict', status: 'done' } });

    expect(screen.queryByRole('button', { name: /runStopped.resolve$/i })).toBeNull();
    expect(screen.getByText(/runStopped.resolveSpent/)).toBeInTheDocument();
  });

  it('keeps the modal open and shows why when resolving is refused', async () => {
    api.resolveStoppedRun.mockRejectedValue(new Error('nothing left to resolve'));
    const onResolved = vi.fn();
    render_({ onResolved });

    click(screen.getByRole('button', { name: /runStopped.resolve$/i }));

    await waitFor(() => expect(screen.getByText(/nothing left to resolve/)).toBeInTheDocument());
    expect(onResolved).not.toHaveBeenCalled();
  });
});
