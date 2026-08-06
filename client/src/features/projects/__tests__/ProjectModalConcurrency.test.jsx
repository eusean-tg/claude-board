import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';

vi.mock('../../../i18n/I18nProvider', () => ({
  useTranslation: () => ({ t: (k) => k }),
}));
vi.mock('../../../lib/api', () => ({ api: { githubDetectRepo: vi.fn().mockResolvedValue(null) } }));
vi.mock('../../../lib/tauriEvents', () => ({ IS_TAURI: false }));
vi.mock('../../../lib/useGitRepoStatus', () => ({
  useGitRepoStatus: () => ({ status: { isRepo: true }, loading: false, refresh: vi.fn() }),
}));
vi.mock('../../../lib/useModels', () => ({ useModels: () => ({ models: [] }) }));
vi.mock('boring-avatars', () => ({ default: () => null }));

let ProjectModal;
beforeEach(async () => {
  vi.clearAllMocks();
  ProjectModal = (await import('../ProjectModal')).default;
});

const openAutomation = (project) => {
  render(<ProjectModal project={project} onSubmit={vi.fn()} onClose={vi.fn()} />);
  fireEvent.click(screen.getByText('Automation'));
};

// The Automation tab has several number inputs, so this reads the one belonging to
// the Max Concurrent field rather than the first on the page.
const maxConcurrentInput = () => {
  const label = screen.getByText('projectModal.maxConcurrent');
  return label.parentElement.querySelector('input[type="number"]');
};

describe('ProjectModal — Max Concurrent', () => {
  it('is reachable with auto-queue off', () => {
    // The limit also caps a dependency chain, and a chain runs whether or not
    // auto-queue is on. Hiding the control behind that toggle left a project
    // throttled to a stored value with no way to see or change it.
    openAutomation({ name: 'p', slug: 'p', working_dir: '/tmp/p', auto_queue: 0, max_concurrent: 2 });

    expect(screen.getByText('projectModal.maxConcurrent')).toBeInTheDocument();
    expect(maxConcurrentInput()).toHaveValue(2);
  });

  it('still shows with auto-queue on', () => {
    openAutomation({ name: 'p', slug: 'p', working_dir: '/tmp/p', auto_queue: 1, max_concurrent: 3 });

    expect(screen.getByText('projectModal.maxConcurrent')).toBeInTheDocument();
    expect(maxConcurrentInput()).toHaveValue(3);
  });
});
