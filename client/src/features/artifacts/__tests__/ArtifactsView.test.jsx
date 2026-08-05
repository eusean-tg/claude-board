import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, waitFor, fireEvent } from '@testing-library/react';

const listArtifacts = vi.fn();
const getArtifact = vi.fn();
const updateArtifact = vi.fn();
const deleteArtifact = vi.fn();
const artifactReference = vi.fn();
const revealArtifact = vi.fn();
const getTask = vi.fn();
const notifyError = vi.fn();

vi.mock('../../../lib/api', () => ({
  api: {
    listArtifacts,
    getArtifact,
    updateArtifact,
    deleteArtifact,
    artifactReference,
    revealArtifact,
    getTask,
  },
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
    id: 11,
    project_id: 1,
    stored_name: 'plan-1754400000.md',
    source_rel_path: 'PLAN.md',
    title: 'The Plan',
    preview: 'a plan',
    kind: 'plan',
    size: 2048,
    origin_task_id: 7,
    last_task_id: 7,
    created_at: new Date().toISOString(),
    updated_at: new Date().toISOString(),
  },
  {
    id: 12,
    project_id: 1,
    stored_name: 'spec-1754400001.md',
    source_rel_path: 'docs/spec.md',
    title: null,
    preview: '',
    kind: 'spec',
    size: 500,
    origin_task_id: null,
    last_task_id: null,
    created_at: null,
    updated_at: null,
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

    expect(container.querySelector('.animate-spin')).toBeTruthy();

    resolve(ARTIFACTS);

    await waitFor(() => expect(screen.getByText('The Plan')).toBeTruthy());
    // With no title, the row falls back to the stored filename.
    expect(screen.getByText('spec-1754400001.md')).toBeTruthy();
    // The source path is shown beneath the title.
    expect(screen.getByText('docs/spec.md')).toBeTruthy();
    expect(screen.getByText('artifacts.fileCount:{"count":2}')).toBeTruthy();
    expect(screen.getByText('artifacts.selectPrompt')).toBeTruthy();

    fireEvent.click(screen.getByText('The Plan'));
    await waitFor(() => expect(screen.getByText('Hello')).toBeTruthy());
    // Keyed on the artifact id, not a path.
    expect(getArtifact).toHaveBeenCalledWith(11);
  });

  it('groups rows by kind rather than by directory', async () => {
    listArtifacts.mockResolvedValue(ARTIFACTS);
    render(<ArtifactsView projectId={1} project={{ name: 'repo' }} tasks={[]} />);
    await waitFor(() => expect(screen.getByText('The Plan')).toBeTruthy());

    // A flat store has no directories; the group headers are kind labels. Each
    // label also appears on the toolbar chip and each row badge, so all that can
    // be asserted is that it appears more than once.
    expect(screen.getAllByText('artifacts.kind.plan').length).toBeGreaterThan(1);
    expect(screen.getAllByText('artifacts.kind.spec').length).toBeGreaterThan(1);
    // The old directory grouping showed the project name for the repo root.
    expect(screen.queryByText('repo')).toBeNull();
  });

  it('filters as you type and by kind', async () => {
    listArtifacts.mockResolvedValue(ARTIFACTS);
    render(<ArtifactsView projectId={1} project={{ name: 'repo' }} tasks={[]} />);
    await waitFor(() => expect(screen.getByText('The Plan')).toBeTruthy());

    fireEvent.change(screen.getByPlaceholderText('artifacts.search'), { target: { value: 'spec' } });
    await waitFor(() => expect(screen.queryByText('The Plan')).toBeNull());
    expect(screen.getByText('spec-1754400001.md')).toBeTruthy();

    fireEvent.change(screen.getByPlaceholderText('artifacts.search'), { target: { value: '' } });
    fireEvent.click(screen.getAllByText('artifacts.kind.plan')[0]);
    await waitFor(() => expect(screen.queryByText('spec-1754400001.md')).toBeNull());
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

  it('edits, saves via Cmd+S, and refreshes the row from the response', async () => {
    listArtifacts.mockResolvedValue(ARTIFACTS);
    getArtifact.mockResolvedValue({ content: 'original' });
    updateArtifact.mockResolvedValue({ ...ARTIFACTS[0], title: 'Renamed by the edit' });

    render(<ArtifactsView projectId={1} project={{ name: 'repo' }} tasks={[]} />);
    await waitFor(() => expect(screen.getByText('The Plan')).toBeTruthy());
    fireEvent.click(screen.getByText('The Plan'));
    await waitFor(() => expect(screen.getByText('artifacts.edit')).toBeTruthy());

    fireEvent.click(screen.getByText('artifacts.edit'));
    const textarea = await waitFor(() => document.querySelector('textarea'));
    fireEvent.change(textarea, { target: { value: 'changed' } });
    await waitFor(() => expect(screen.getByText('artifacts.unsaved')).toBeTruthy());

    fireEvent.keyDown(window, { key: 's', metaKey: true });
    await waitFor(() => expect(updateArtifact).toHaveBeenCalledWith(11, 'changed'));
    await waitFor(() => expect(screen.getByText('artifacts.saved')).toBeTruthy());
    // Title, preview, kind and size are re-derived from the new content, so the
    // list has to pick the refreshed row up. The selected artifact's title shows
    // in both the row and the pane header, hence getAllByText.
    await waitFor(() => expect(screen.getAllByText('Renamed by the edit').length).toBeGreaterThan(0));
  });

  it('copies the absolute store path, not the source path', async () => {
    listArtifacts.mockResolvedValue(ARTIFACTS);
    getArtifact.mockResolvedValue({ content: 'x' });
    artifactReference.mockResolvedValue({
      path: '/home/u/.claudeboard/artifacts/spec-1754400001.md',
    });
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });

    render(<ArtifactsView projectId={1} project={{ name: 'repo' }} tasks={[]} />);
    await waitFor(() => expect(screen.getByText('spec-1754400001.md')).toBeTruthy());
    fireEvent.click(screen.getByText('spec-1754400001.md'));
    await waitFor(() => expect(screen.getByText('artifacts.copyPath')).toBeTruthy());

    fireEvent.click(screen.getByText('artifacts.copyPath'));
    // The path an agent can actually read, which the source path is not.
    await waitFor(() => expect(writeText).toHaveBeenCalledWith('/home/u/.claudeboard/artifacts/spec-1754400001.md'));
    await waitFor(() => expect(screen.getByText('artifacts.copied')).toBeTruthy());
  });

  it('reveals the selected artifact by id', async () => {
    listArtifacts.mockResolvedValue(ARTIFACTS);
    getArtifact.mockResolvedValue({ content: 'x' });
    revealArtifact.mockResolvedValue(undefined);

    render(<ArtifactsView projectId={1} project={{ name: 'repo' }} tasks={[]} />);
    await waitFor(() => expect(screen.getByText('spec-1754400001.md')).toBeTruthy());
    fireEvent.click(screen.getByText('spec-1754400001.md'));
    await waitFor(() => expect(screen.getByText('artifacts.reveal')).toBeTruthy());

    fireEvent.click(screen.getByText('artifacts.reveal'));
    await waitFor(() => expect(revealArtifact).toHaveBeenCalledWith(12));
  });

  it('deletes after confirmation and drops the row from the list', async () => {
    listArtifacts.mockResolvedValue(ARTIFACTS);
    getArtifact.mockResolvedValue({ content: 'x' });
    deleteArtifact.mockResolvedValue(undefined);
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(true);

    render(<ArtifactsView projectId={1} project={{ name: 'repo' }} tasks={[]} />);
    await waitFor(() => expect(screen.getByText('The Plan')).toBeTruthy());
    fireEvent.click(screen.getByText('The Plan'));
    await waitFor(() => expect(screen.getByText('artifacts.delete')).toBeTruthy());

    fireEvent.click(screen.getByText('artifacts.delete'));
    await waitFor(() => expect(deleteArtifact).toHaveBeenCalledWith(11));
    // Row gone and the selection cleared, without a refetch.
    await waitFor(() => expect(screen.queryByText('The Plan')).toBeNull());
    expect(screen.getByText('artifacts.selectPrompt')).toBeTruthy();
    confirm.mockRestore();
  });

  it('does not delete when the confirmation is declined', async () => {
    listArtifacts.mockResolvedValue(ARTIFACTS);
    getArtifact.mockResolvedValue({ content: 'x' });
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);

    render(<ArtifactsView projectId={1} project={{ name: 'repo' }} tasks={[]} />);
    await waitFor(() => expect(screen.getByText('The Plan')).toBeTruthy());
    fireEvent.click(screen.getByText('The Plan'));
    await waitFor(() => expect(screen.getByText('artifacts.delete')).toBeTruthy());

    fireEvent.click(screen.getByText('artifacts.delete'));

    expect(deleteArtifact).not.toHaveBeenCalled();
    // Still selected, so the title appears in both the row and the pane header.
    expect(screen.getAllByText('The Plan').length).toBeGreaterThan(0);
    confirm.mockRestore();
  });

  it('shows one attribution chip per authoring task', async () => {
    listArtifacts.mockResolvedValue([{ ...ARTIFACTS[0], origin_task_id: 7, last_task_id: 9 }]);
    getArtifact.mockResolvedValue({ content: 'x' });

    render(
      <ArtifactsView
        projectId={1}
        project={{ name: 'repo' }}
        tasks={[
          { id: 7, task_key: 'CB-7', title: 'wrote it' },
          { id: 9, task_key: 'CB-9', title: 'edited it' },
        ]}
      />,
    );
    await waitFor(() => expect(screen.getByText('The Plan')).toBeTruthy());
    fireEvent.click(screen.getByText('The Plan'));

    await waitFor(() => expect(screen.getByText('CB-7')).toBeTruthy());
    expect(screen.getByText('CB-9')).toBeTruthy();
  });

  it('shows a single chip when one task both created and last wrote the document', async () => {
    listArtifacts.mockResolvedValue(ARTIFACTS);
    getArtifact.mockResolvedValue({ content: 'x' });

    render(
      <ArtifactsView
        projectId={1}
        project={{ name: 'repo' }}
        tasks={[{ id: 7, task_key: 'CB-7', title: 'wrote it' }]}
      />,
    );
    await waitFor(() => expect(screen.getByText('The Plan')).toBeTruthy());
    fireEvent.click(screen.getByText('The Plan'));

    // origin and last are the same task; it must not be listed twice.
    await waitFor(() => expect(screen.getAllByText('CB-7')).toHaveLength(1));
  });

  it('falls back to api.getTask for a chip missing from the tasks prop', async () => {
    listArtifacts.mockResolvedValue(ARTIFACTS);
    getArtifact.mockResolvedValue({ content: 'x' });
    getTask.mockResolvedValue({ id: 7, title: 'fetched' });
    const onViewDetail = vi.fn();

    render(<ArtifactsView projectId={1} project={{ name: 'repo' }} tasks={[]} onViewDetail={onViewDetail} />);
    await waitFor(() => expect(screen.getByText('The Plan')).toBeTruthy());
    fireEvent.click(screen.getByText('The Plan'));
    // No matching task in the prop, so the chip falls back to the raw id.
    await waitFor(() => expect(screen.getByText('#7')).toBeTruthy());

    fireEvent.click(screen.getByText('#7'));
    await waitFor(() => expect(getTask).toHaveBeenCalledWith(7));
    expect(onViewDetail).toHaveBeenCalledWith({ id: 7, title: 'fetched' });
  });

  it('prefers the local task and swallows getTask failures', async () => {
    listArtifacts.mockResolvedValue(ARTIFACTS);
    getArtifact.mockResolvedValue({ content: 'x' });
    getTask.mockRejectedValue(new Error('gone'));
    const onViewDetail = vi.fn();
    const local = { id: 7, task_key: 'CB-7', title: 'local' };

    render(<ArtifactsView projectId={1} project={{ name: 'repo' }} tasks={[local]} onViewDetail={onViewDetail} />);
    await waitFor(() => expect(screen.getByText('The Plan')).toBeTruthy());
    fireEvent.click(screen.getByText('The Plan'));
    await waitFor(() => expect(screen.getByText('CB-7')).toBeTruthy());

    fireEvent.click(screen.getByText('CB-7'));
    await waitFor(() => expect(onViewDetail).toHaveBeenCalledWith(local));
    expect(getTask).not.toHaveBeenCalled();
  });
});
