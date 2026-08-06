import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';

// fireEvent rather than user-event: the house convention, and it keeps the
// dependency list as it is.
const click = (el) => fireEvent.click(el);
const type = (el, value) => fireEvent.change(el, { target: { value } });

vi.mock('../../../i18n/I18nProvider', () => ({
  useTranslation: () => ({ t: (k) => k }),
}));

let BlockerPanel;
beforeEach(async () => {
  vi.clearAllMocks();
  BlockerPanel = (await import('../BlockerPanel')).default;
});

const singleChoice = () => ({
  id: 5,
  task_id: 1,
  kind: 'single_choice',
  header: 'Auth flow',
  question: 'Which auth flow should the client use?',
  context: 'The router already assumes a redirect.',
  status: 'open',
  options: [
    { id: 1, label: 'PKCE', description: 'recommended for public clients' },
    { id: 2, label: 'Implicit', description: '' },
  ],
});

const multiChoice = () => ({
  ...singleChoice(),
  kind: 'multi_choice',
  question: 'Which paths should be cached?',
  options: [
    { id: 1, label: 'Read path', description: '' },
    { id: 2, label: 'Write path', description: '' },
  ],
});

const freeText = () => ({
  ...singleChoice(),
  kind: 'free_text',
  question: 'How should the token be stored?',
  options: [],
});

describe('BlockerPanel', () => {
  it('renders a single select as radios and submits one option', async () => {
    const onAnswer = vi.fn();
    render(<BlockerPanel blocker={singleChoice()} onAnswer={onAnswer} onCancel={() => {}} />);

    click(screen.getByLabelText('PKCE'));
    click(screen.getByRole('button', { name: /send/i }));

    expect(onAnswer).toHaveBeenCalledWith([{ optionId: 1, note: null, freeText: null }]);
  });

  it('only lets one option be chosen for a single select', async () => {
    const onAnswer = vi.fn();
    render(<BlockerPanel blocker={singleChoice()} onAnswer={onAnswer} onCancel={() => {}} />);

    click(screen.getByLabelText('PKCE'));
    click(screen.getByLabelText('Implicit'));
    click(screen.getByRole('button', { name: /send/i }));

    // Two answers to a single select leaves the agent to guess which one won.
    expect(onAnswer).toHaveBeenCalledWith([{ optionId: 2, note: null, freeText: null }]);
  });

  it('lets each checked option carry its own context', async () => {
    const onAnswer = vi.fn();
    render(<BlockerPanel blocker={multiChoice()} onAnswer={onAnswer} onCancel={() => {}} />);

    click(screen.getByLabelText('Read path'));
    type(screen.getByPlaceholderText(/addContext/i), 'only the cached reads');
    click(screen.getByRole('button', { name: /send/i }));

    expect(onAnswer).toHaveBeenCalledWith([{ optionId: 1, note: 'only the cached reads', freeText: null }]);
  });

  it('reveals a note field only for the options that are checked', async () => {
    render(<BlockerPanel blocker={multiChoice()} onAnswer={() => {}} onCancel={() => {}} />);

    // A note box per option, always visible, is noise on a question with ten of them.
    expect(screen.queryByPlaceholderText(/addContext/i)).not.toBeInTheDocument();
    click(screen.getByLabelText('Write path'));
    expect(screen.getAllByPlaceholderText(/addContext/i)).toHaveLength(1);
  });

  it('submits several checked options together', async () => {
    const onAnswer = vi.fn();
    render(<BlockerPanel blocker={multiChoice()} onAnswer={onAnswer} onCancel={() => {}} />);

    click(screen.getByLabelText('Read path'));
    click(screen.getByLabelText('Write path'));
    click(screen.getByRole('button', { name: /send/i }));

    expect(onAnswer).toHaveBeenCalledWith([
      { optionId: 1, note: null, freeText: null },
      { optionId: 2, note: null, freeText: null },
    ]);
  });

  it('offers a free-text escape even when options are given', async () => {
    render(<BlockerPanel blocker={singleChoice()} onAnswer={() => {}} onCancel={() => {}} />);

    // The options are the agent's guess at the answer space, and it is often wrong.
    expect(screen.getByLabelText(/somethingElse/i)).toBeInTheDocument();
  });

  it('submits the escape text as a free-text answer', async () => {
    const onAnswer = vi.fn();
    render(<BlockerPanel blocker={singleChoice()} onAnswer={onAnswer} onCancel={() => {}} />);

    click(screen.getByLabelText(/somethingElse/i));
    type(screen.getByPlaceholderText(/escapePlaceholder/i), 'Use device flow');
    click(screen.getByRole('button', { name: /send/i }));

    expect(onAnswer).toHaveBeenCalledWith([{ optionId: null, note: null, freeText: 'Use device flow' }]);
  });

  it('submits a free-text question as typed', async () => {
    const onAnswer = vi.fn();
    render(<BlockerPanel blocker={freeText()} onAnswer={onAnswer} onCancel={() => {}} />);

    type(screen.getByRole('textbox'), 'In the keychain');
    click(screen.getByRole('button', { name: /send/i }));

    expect(onAnswer).toHaveBeenCalledWith([{ optionId: null, note: null, freeText: 'In the keychain' }]);
  });

  it('cannot submit an empty answer', () => {
    render(<BlockerPanel blocker={freeText()} onAnswer={() => {}} onCancel={() => {}} />);

    // The backend refuses an empty answer, so the button would only produce an error.
    expect(screen.getByRole('button', { name: /send/i })).toBeDisabled();
  });

  it('cannot submit whitespace as a free-text answer', async () => {
    render(<BlockerPanel blocker={freeText()} onAnswer={() => {}} onCancel={() => {}} />);

    type(screen.getByRole('textbox'), '   ');

    expect(screen.getByRole('button', { name: /send/i })).toBeDisabled();
  });

  it('cannot submit the escape with nothing typed in it', async () => {
    render(<BlockerPanel blocker={singleChoice()} onAnswer={() => {}} onCancel={() => {}} />);

    click(screen.getByLabelText(/somethingElse/i));

    expect(screen.getByRole('button', { name: /send/i })).toBeDisabled();
  });

  it('shows the question, its header and the context the agent gathered', () => {
    render(<BlockerPanel blocker={singleChoice()} onAnswer={() => {}} onCancel={() => {}} />);

    expect(screen.getByText('Which auth flow should the client use?')).toBeInTheDocument();
    expect(screen.getByText('Auth flow')).toBeInTheDocument();
    // The context is why the user can answer without re-deriving the situation.
    expect(screen.getByText('The router already assumes a redirect.')).toBeInTheDocument();
    expect(screen.getByText(/recommended for public clients/)).toBeInTheDocument();
  });

  it('cancels without sending an answer', async () => {
    const onCancel = vi.fn();
    const onAnswer = vi.fn();
    render(<BlockerPanel blocker={freeText()} onAnswer={onAnswer} onCancel={onCancel} />);

    click(screen.getByRole('button', { name: /dismiss/i }));

    expect(onCancel).toHaveBeenCalled();
    expect(onAnswer).not.toHaveBeenCalled();
  });

  it('renders nothing without a blocker', () => {
    const { container } = render(<BlockerPanel blocker={null} onAnswer={() => {}} onCancel={() => {}} />);

    expect(container).toBeEmptyDOMElement();
  });

  it('stops accepting input while the answer is being sent', async () => {
    let release;
    const onAnswer = vi.fn(() => new Promise((r) => (release = r)));
    render(<BlockerPanel blocker={freeText()} onAnswer={onAnswer} onCancel={() => {}} />);
    type(screen.getByRole('textbox'), 'In the keychain');

    click(screen.getByRole('button', { name: /send/i }));

    // Two clicks would answer twice, and the second is refused by the backend.
    expect(screen.getByRole('button', { name: /sending/i })).toBeDisabled();
    release();
  });
});
