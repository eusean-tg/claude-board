import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, waitFor, fireEvent } from '@testing-library/react';

const listArtifacts = vi.fn();
const taskArtifacts = vi.fn();
const addArtifactRef = vi.fn();
const removeArtifactRef = vi.fn();
const notifyError = vi.fn();

vi.mock('../../../lib/api', () => ({
  api: { listArtifacts, taskArtifacts, addArtifactRef, removeArtifactRef },
  notifyError,
}));

let IS_TAURI = true;
vi.mock('../../../lib/tauriEvents', () => ({
  get IS_TAURI() {
    return IS_TAURI;
  },
}));

vi.mock('../../../i18n/I18nProvider', () => ({
  useTranslation: () => ({ t: (k) => k }),
}));

const artifact = (overrides = {}) => ({
  id: 1,
  project_id: 1,
  stored_name: 'plan-1.md',
  origin: 'docs/plan.md',
  title: 'Auth plan',
  preview: '',
  kind: 'plan',
  size: 10,
  ...overrides,
});

let ArtifactPicker;

describe('ArtifactPicker', () => {
  beforeEach(async () => {
    vi.clearAllMocks();
    IS_TAURI = true;
    taskArtifacts.mockResolvedValue([]);
    ({ default: ArtifactPicker } = await import('../ArtifactPicker'));
  });

  afterEach(() => {
    vi.resetModules();
  });

  it('records a reference rather than editing any text field', async () => {
    listArtifacts.mockResolvedValue([artifact()]);
    addArtifactRef.mockResolvedValue([artifact()]);
    const onChange = vi.fn();

    render(<ArtifactPicker projectId={1} taskId={5} onChange={onChange} />);
    fireEvent.click(await screen.findByText('Auth plan'));

    // A relation, because the prompt builder, the detail view and blockers all
    // query it — a path pasted into a description answers none of them.
    await waitFor(() => expect(addArtifactRef).toHaveBeenCalledWith(5, 1, 'reference'));
    expect(onChange).toHaveBeenCalled();
  });

  it('lists what the task already references, and excludes it from the candidates', async () => {
    listArtifacts.mockResolvedValue([artifact({ id: 1 }), artifact({ id: 2, title: 'Queue RFC' })]);
    taskArtifacts.mockResolvedValue([artifact({ id: 1 })]);

    render(<ArtifactPicker projectId={1} taskId={5} />);

    await waitFor(() => expect(screen.getByText('artifacts.alreadyReferenced')).toBeTruthy());
    // Auth plan shows once, under the referenced heading, not again as a candidate.
    expect(screen.getAllByText('Auth plan')).toHaveLength(1);
    expect(screen.getByText('Queue RFC')).toBeTruthy();
  });

  it('removes a reference', async () => {
    listArtifacts.mockResolvedValue([artifact()]);
    taskArtifacts.mockResolvedValue([artifact()]);
    removeArtifactRef.mockResolvedValue([]);

    render(<ArtifactPicker projectId={1} taskId={5} />);
    await waitFor(() => expect(screen.getByTitle('artifacts.refRemove')).toBeTruthy());

    fireEvent.click(screen.getByTitle('artifacts.refRemove'));

    await waitFor(() => expect(removeArtifactRef).toHaveBeenCalledWith(5, 1));
  });

  it('filters candidates by the search box', async () => {
    listArtifacts.mockResolvedValue([artifact({ id: 1 }), artifact({ id: 2, title: 'Queue RFC' })]);

    render(<ArtifactPicker projectId={1} taskId={5} />);
    await waitFor(() => expect(screen.getByText('Auth plan')).toBeTruthy());

    fireEvent.change(screen.getByPlaceholderText('artifacts.refSearch'), {
      target: { value: 'queue' },
    });

    await waitFor(() => expect(screen.queryByText('Auth plan')).toBeNull());
    expect(screen.getByText('Queue RFC')).toBeTruthy();
  });

  it('says so when there is nothing left to reference', async () => {
    listArtifacts.mockResolvedValue([]);
    render(<ArtifactPicker projectId={1} taskId={5} />);
    await waitFor(() => expect(screen.getByText('artifacts.refNone')).toBeTruthy());
  });

  it('renders nothing outside the desktop app', async () => {
    vi.resetModules();
    IS_TAURI = false;
    ({ default: ArtifactPicker } = await import('../ArtifactPicker'));

    const { container } = render(<ArtifactPicker projectId={1} taskId={5} />);

    expect(container.firstChild).toBeNull();
    expect(listArtifacts).not.toHaveBeenCalled();
  });
});
