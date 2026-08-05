import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, waitFor, fireEvent } from '@testing-library/react';

const listArtifacts = vi.fn();
const getArtifact = vi.fn();
const revealArtifact = vi.fn();
const saveArtifact = vi.fn();
const getTask = vi.fn();
const notifyError = vi.fn();

vi.mock('../../../lib/api', () => ({
  api: { listArtifacts, getArtifact, saveArtifact, revealArtifact, getTask },
  notifyError,
}));

let IS_TAURI = true;
vi.mock('../../../lib/tauriEvents', () => ({
  get IS_TAURI() {
    return IS_TAURI;
  },
}));

vi.mock('../../../i18n/I18nProvider', () => ({
  useTranslation: () => ({ t: (k, p) => (p ? `${k}:${JSON.stringify(p)}` : k) }),
}));

const ARTIFACTS = [
  {
    rel_path: 'PLAN.md',
    name: 'PLAN.md',
    dir: '',
    title: 'The Plan',
    kind: 'plan',
    size_bytes: 2048,
    modified_at: new Date().toISOString(),
    tasks: [{ task_id: 7, task_key: 'CB-7', title: 'Do the thing' }],
  },
  {
    rel_path: 'docs/spec.md',
    name: 'spec.md',
    dir: 'docs',
    title: null,
    kind: 'spec',
    size_bytes: 500,
    modified_at: null,
    tasks: [],
  },
];

let ArtifactsView;

describe('ArtifactsView render states', () => {
  beforeEach(async () => {
    vi.clearAllMocks();
    IS_TAURI = true;
    ({ default: ArtifactsView } = await import('../ArtifactsView'));
  });

  afterEach(() => {
    vi.resetModules();
  });

  it('renders the loading state then the populated two-pane view', async () => {
    let resolve;
    listArtifacts.mockReturnValue(new Promise((r) => (resolve = r)));
    getArtifact.mockResolvedValue({ content: '# Hello\n\nbody' });

    const { container } = render(<ArtifactsView projectId={1} project={{ name: 'repo' }} tasks={[]} />);

    // loading: spinner present, selectPrompt in the right pane
    expect(container.querySelector('.animate-spin')).toBeTruthy();

    resolve(ARTIFACTS);

    await waitFor(() => expect(screen.getByText('The Plan')).toBeTruthy());
    expect(screen.getByText('spec.md')).toBeTruthy();
    expect(screen.getByText('docs')).toBeTruthy();
    expect(screen.getByText('repo')).toBeTruthy(); // repo-root group header
    expect(screen.getByText('artifacts.fileCount:{"count":2}')).toBeTruthy();
    expect(screen.getByText('artifacts.selectPrompt')).toBeTruthy();

    // select a row -> fetches + renders markdown
    fireEvent.click(screen.getByText('The Plan'));
    await waitFor(() => expect(screen.getByText('Hello')).toBeTruthy());
    expect(getArtifact).toHaveBeenCalledWith(1, 'PLAN.md');

    // task attribution chip resolves from the tasks prop
    expect(screen.getByText('CB-7')).toBeTruthy();
  });

  it('filters as you type and by kind', async () => {
    listArtifacts.mockResolvedValue(ARTIFACTS);
    render(<ArtifactsView projectId={1} project={{ name: 'repo' }} tasks={[]} />);
    await waitFor(() => expect(screen.getByText('The Plan')).toBeTruthy());

    fireEvent.change(screen.getByPlaceholderText('artifacts.search'), { target: { value: 'spec' } });
    await waitFor(() => expect(screen.queryByText('The Plan')).toBeNull());
    expect(screen.getByText('spec.md')).toBeTruthy();

    fireEvent.change(screen.getByPlaceholderText('artifacts.search'), { target: { value: '' } });
    // the label also appears on each row's kind badge; [0] is the toolbar chip
    fireEvent.click(screen.getAllByText('artifacts.kind.plan')[0]);
    await waitFor(() => expect(screen.queryByText('spec.md')).toBeNull());
    expect(screen.getByText('The Plan')).toBeTruthy();
  });

  it('renders the empty state', async () => {
    listArtifacts.mockResolvedValue([]);
    render(<ArtifactsView projectId={1} project={{ name: 'repo' }} tasks={[]} />);
    await waitFor(() => expect(screen.getByText('artifacts.empty')).toBeTruthy());
    expect(screen.getByText('artifacts.emptyDesc')).toBeTruthy();
  });

  it('renders the desktop-only state and never fetches in web mode', async () => {
    vi.resetModules();
    IS_TAURI = false;
    ({ default: ArtifactsView } = await import('../ArtifactsView'));

    render(<ArtifactsView projectId={1} project={{ name: 'repo' }} tasks={[]} />);
    expect(screen.getByText('artifacts.desktopOnly')).toBeTruthy();
    expect(listArtifacts).not.toHaveBeenCalled();
  });

  it('surfaces a load failure without getting stuck on the spinner', async () => {
    listArtifacts.mockRejectedValue(new Error('boom'));
    const { container } = render(<ArtifactsView projectId={1} project={{ name: 'repo' }} tasks={[]} />);
    await waitFor(() => expect(notifyError).toHaveBeenCalledWith('boom'));
    expect(container.querySelector('.animate-spin')).toBeNull();
    expect(screen.getByText('artifacts.empty')).toBeTruthy();
  });

  it('edits, saves via Cmd+S, and shows the transient confirmation', async () => {
    listArtifacts.mockResolvedValue(ARTIFACTS);
    getArtifact.mockResolvedValue({ content: 'original' });
    saveArtifact.mockResolvedValue(undefined);

    render(<ArtifactsView projectId={1} project={{ name: 'repo' }} tasks={[]} />);
    await waitFor(() => expect(screen.getByText('The Plan')).toBeTruthy());
    fireEvent.click(screen.getByText('The Plan'));
    await waitFor(() => expect(screen.getByText('artifacts.edit')).toBeTruthy());

    fireEvent.click(screen.getByText('artifacts.edit'));
    const textarea = await waitFor(() => document.querySelector('textarea'));
    fireEvent.change(textarea, { target: { value: 'changed' } });
    await waitFor(() => expect(screen.getByText('artifacts.unsaved')).toBeTruthy());

    fireEvent.keyDown(window, { key: 's', metaKey: true });
    await waitFor(() => expect(saveArtifact).toHaveBeenCalledWith(1, 'PLAN.md', 'changed'));
    await waitFor(() => expect(screen.getByText('artifacts.saved')).toBeTruthy());
  });

  it('reveals and copies the path', async () => {
    listArtifacts.mockResolvedValue(ARTIFACTS);
    getArtifact.mockResolvedValue({ content: 'x' });
    revealArtifact.mockResolvedValue(undefined);
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });

    render(<ArtifactsView projectId={1} project={{ name: 'repo' }} tasks={[]} />);
    await waitFor(() => expect(screen.getByText('spec.md')).toBeTruthy());
    fireEvent.click(screen.getByText('spec.md'));
    await waitFor(() => expect(screen.getByText('artifacts.reveal')).toBeTruthy());

    fireEvent.click(screen.getByText('artifacts.reveal'));
    await waitFor(() => expect(revealArtifact).toHaveBeenCalledWith(1, 'docs/spec.md'));

    fireEvent.click(screen.getByText('artifacts.copyPath'));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith('docs/spec.md'));
    await waitFor(() => expect(screen.getByText('artifacts.copied')).toBeTruthy());
  });

  it('falls back to api.getTask for a chip missing from the tasks prop', async () => {
    listArtifacts.mockResolvedValue(ARTIFACTS);
    getArtifact.mockResolvedValue({ content: 'x' });
    getTask.mockResolvedValue({ id: 7, title: 'fetched' });
    const onViewDetail = vi.fn();

    render(<ArtifactsView projectId={1} project={{ name: 'repo' }} tasks={[]} onViewDetail={onViewDetail} />);
    await waitFor(() => expect(screen.getByText('The Plan')).toBeTruthy());
    fireEvent.click(screen.getByText('The Plan'));
    await waitFor(() => expect(screen.getByText('CB-7')).toBeTruthy());

    fireEvent.click(screen.getByText('CB-7'));
    await waitFor(() => expect(getTask).toHaveBeenCalledWith(7));
    expect(onViewDetail).toHaveBeenCalledWith({ id: 7, title: 'fetched' });
  });

  it('prefers the local task and swallows getTask failures', async () => {
    listArtifacts.mockResolvedValue(ARTIFACTS);
    getArtifact.mockResolvedValue({ content: 'x' });
    getTask.mockRejectedValue(new Error('gone'));
    const onViewDetail = vi.fn();
    const local = { id: 7, title: 'local' };

    render(<ArtifactsView projectId={1} project={{ name: 'repo' }} tasks={[local]} onViewDetail={onViewDetail} />);
    await waitFor(() => expect(screen.getByText('The Plan')).toBeTruthy());
    fireEvent.click(screen.getByText('The Plan'));
    await waitFor(() => expect(screen.getByText('CB-7')).toBeTruthy());

    fireEvent.click(screen.getByText('CB-7'));
    await waitFor(() => expect(onViewDetail).toHaveBeenCalledWith(local));
    expect(getTask).not.toHaveBeenCalled();
  });
});
