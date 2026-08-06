import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';

const click = (el) => fireEvent.click(el);

vi.mock('../../../i18n/I18nProvider', () => ({
  useTranslation: () => ({ t: (k, vars) => (vars ? `${k} ${JSON.stringify(vars)}` : k) }),
}));

let PrerequisiteModal;
beforeEach(async () => {
  vi.clearAllMocks();
  PrerequisiteModal = (await import('../PrerequisiteModal')).default;
});

const chain = () => [
  [{ id: 1, title: 'discovery service' }],
  [{ id: 2, title: 'tauri commands' }],
  [{ id: 3, title: 'wire the tab' }],
];

const parallel = () => [
  [
    { id: 1, title: 'a' },
    { id: 2, title: 'b' },
  ],
  [{ id: 3, title: 'c' }],
];

describe('PrerequisiteModal', () => {
  it('lists every prerequisite wave in order and names the target last', () => {
    render(<PrerequisiteModal waves={chain()} onConfirm={() => {}} onClose={() => {}} />);

    const items = screen.getAllByRole('listitem').map((li) => li.textContent);
    expect(items[0]).toContain('discovery service');
    expect(items[2]).toContain('wire the tab');
  });

  it('states how many tasks will start, so confirming is an informed choice', () => {
    render(<PrerequisiteModal waves={parallel()} onConfirm={() => {}} onClose={() => {}} />);

    // Three tasks across two waves — the count is of tasks, not waves. It is on
    // the button as well as the blurb, because the button is what gets clicked.
    expect(screen.getByRole('button', { name: /"count":3/ })).toBeInTheDocument();
    expect(screen.getAllByText(/"count":3/).length).toBeGreaterThan(0);
  });

  it('groups tasks that can run together into one step', () => {
    render(<PrerequisiteModal waves={parallel()} onConfirm={() => {}} onClose={() => {}} />);

    // Two steps, not three: a and b are independent and run at the same time.
    expect(screen.getAllByRole('listitem')).toHaveLength(2);
  });

  it('marks the last wave as the task that was asked for', () => {
    render(<PrerequisiteModal waves={chain()} onConfirm={() => {}} onClose={() => {}} />);

    // Without this the user cannot tell which of three steps they clicked on.
    expect(screen.getByText(/prerequisites.target/)).toBeInTheDocument();
  });

  it('shows the trunk branch the group will build on', () => {
    render(
      <PrerequisiteModal
        waves={chain()}
        trunkBranch="trunk/feature/wire-the-tab"
        onConfirm={() => {}}
        onClose={() => {}}
      />,
    );

    expect(screen.getByText('trunk/feature/wire-the-tab')).toBeInTheDocument();
  });

  it('runs the chain on confirmation', () => {
    const onConfirm = vi.fn();
    render(<PrerequisiteModal waves={chain()} onConfirm={onConfirm} onClose={() => {}} />);

    click(screen.getByRole('button', { name: /prerequisites.confirm/i }));

    expect(onConfirm).toHaveBeenCalled();
  });

  it('closes without starting anything', () => {
    const onConfirm = vi.fn();
    const onClose = vi.fn();
    render(<PrerequisiteModal waves={chain()} onConfirm={onConfirm} onClose={onClose} />);

    click(screen.getByRole('button', { name: /prerequisites.cancel/i }));

    expect(onClose).toHaveBeenCalled();
    expect(onConfirm).not.toHaveBeenCalled();
  });

  it('blocks a second confirmation while the first is starting', async () => {
    let release;
    const onConfirm = vi.fn(() => new Promise((r) => (release = r)));
    render(<PrerequisiteModal waves={chain()} onConfirm={onConfirm} onClose={() => {}} />);

    click(screen.getByRole('button', { name: /prerequisites.confirm/i }));

    // Confirming twice would try to create two groups over the same tasks, and the
    // second is refused by the backend.
    expect(screen.getByRole('button', { name: /prerequisites.starting/i })).toBeDisabled();
    release();
  });

  it('renders nothing without a plan', () => {
    const { container } = render(<PrerequisiteModal waves={[]} onConfirm={() => {}} onClose={() => {}} />);

    expect(container).toBeEmptyDOMElement();
  });
});
