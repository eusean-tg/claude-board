import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';

vi.mock('../../../i18n/I18nProvider', () => ({
  useTranslation: () => ({ t: (k) => k }),
}));
vi.mock('../../../lib/api', () => ({ api: { githubDetectRepo: vi.fn().mockResolvedValue(null) } }));
vi.mock('../../../lib/tauriEvents', () => ({ IS_TAURI: false }));
vi.mock('../../../lib/useGitRepoStatus', () => ({
  useGitRepoStatus: () => ({ status: { isRepo: true }, loading: false, refresh: vi.fn() }),
}));
vi.mock('../../../lib/useModels', () => ({
  useModels: () => ({
    models: [
      { value: 'sonnet', label: 'Sonnet', source: 'builtin' },
      { value: 'opus', label: 'Opus', source: 'builtin' },
    ],
  }),
}));
vi.mock('boring-avatars', () => ({ default: () => null }));

let ProjectModal;
beforeEach(async () => {
  vi.clearAllMocks();
  ProjectModal = (await import('../ProjectModal')).default;
});

const openEngine = (project, onSubmit) => {
  render(<ProjectModal project={project} onSubmit={onSubmit} onClose={vi.fn()} />);
  fireEvent.click(screen.getByText('Engine'));
};

const selectFor = (labelKey) => {
  const label = screen.getByText(labelKey);
  return label.parentElement.querySelector('select');
};

const project = { name: 'p', slug: 'p', working_dir: '/tmp/p' };

describe('ProjectModal — conflict resolution settings', () => {
  it('leaves both on the default until the user picks something', () => {
    // Empty is the "use the default" convention the backend resolves, so an
    // untouched project must submit empty rather than a guessed model name.
    openEngine(project, vi.fn());

    expect(selectFor('projectModal.resolveModel')).toHaveValue('');
    expect(selectFor('projectModal.resolveEffort')).toHaveValue('');
  });

  it('submits the model and effort the project chose', async () => {
    const onSubmit = vi.fn().mockResolvedValue(undefined);
    openEngine(project, onSubmit);

    fireEvent.change(selectFor('projectModal.resolveModel'), { target: { value: 'sonnet' } });
    fireEvent.change(selectFor('projectModal.resolveEffort'), { target: { value: 'medium' } });
    fireEvent.click(screen.getByText('common.update'));

    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith(
        expect.objectContaining({ resolveModel: 'sonnet', resolveEffort: 'medium' }),
      ),
    );
  });

  it('shows what the project already saved', () => {
    openEngine({ ...project, resolve_model: 'opus', resolve_effort: 'max' }, vi.fn());

    expect(selectFor('projectModal.resolveModel')).toHaveValue('opus');
    expect(selectFor('projectModal.resolveEffort')).toHaveValue('max');
  });
});
