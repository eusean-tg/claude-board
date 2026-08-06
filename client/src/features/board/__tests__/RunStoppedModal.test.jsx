import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';

const click = (el) => fireEvent.click(el);

vi.mock('../../../i18n/I18nProvider', () => ({
  useTranslation: () => ({ t: (k, vars) => (vars ? `${k} ${JSON.stringify(vars)}` : k) }),
}));
vi.mock('../../../lib/api', () => ({
  api: { resumeStoppedRun: vi.fn(), abandonRun: vi.fn() },
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
});
