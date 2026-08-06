import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';

const click = (el) => fireEvent.click(el);
const type = (el, value) => fireEvent.change(el, { target: { value } });

vi.mock('../../../i18n/I18nProvider', () => ({
  useTranslation: () => ({ t: (k) => k }),
}));

let DiscussionPanel;
beforeEach(async () => {
  vi.clearAllMocks();
  DiscussionPanel = (await import('../DiscussionPanel')).default;
});

const thread = () => [
  { id: 1, role: 'user', body: 'why the join table?' },
  { id: 2, role: 'agent', body: 'for the many-to-many' },
];

describe('DiscussionPanel', () => {
  it('shows the thread oldest first with both speakers', () => {
    render(<DiscussionPanel messages={thread()} onPost={() => {}} onResume={() => {}} />);

    expect(screen.getByText('why the join table?')).toBeInTheDocument();
    expect(screen.getByText('for the many-to-many')).toBeInTheDocument();
    // A conversation read in the wrong order means something else.
    const bodies = screen.getAllByText(/join table|many-to-many/).map((n) => n.textContent);
    expect(bodies[0]).toContain('join table');
  });

  it('posts a message and clears the box', async () => {
    const onPost = vi.fn().mockResolvedValue(undefined);
    render(<DiscussionPanel messages={[]} onPost={onPost} onResume={() => {}} />);

    type(screen.getByRole('textbox'), "let's reconsider the schema");
    click(screen.getByRole('button', { name: /discussion.post/i }));

    expect(onPost).toHaveBeenCalledWith("let's reconsider the schema");
  });

  it('cannot post an empty message', () => {
    render(<DiscussionPanel messages={[]} onPost={() => {}} onResume={() => {}} />);

    // The backend refuses it, so the button would only produce an error.
    expect(screen.getByRole('button', { name: /discussion.post/i })).toBeDisabled();
  });

  it('cannot post whitespace', () => {
    render(<DiscussionPanel messages={[]} onPost={() => {}} onResume={() => {}} />);

    type(screen.getByRole('textbox'), '   ');

    expect(screen.getByRole('button', { name: /discussion.post/i })).toBeDisabled();
  });

  it('cannot resume with an empty thread', () => {
    render(<DiscussionPanel messages={[]} onPost={() => {}} onResume={() => {}} />);

    // Restarting the agent with nothing new to go on wastes a run.
    expect(screen.getByRole('button', { name: /discussion.resume/i })).toBeDisabled();
  });

  it('resumes once there is something to send', async () => {
    const onResume = vi.fn().mockResolvedValue(undefined);
    render(<DiscussionPanel messages={thread()} onPost={() => {}} onResume={onResume} />);

    click(screen.getByRole('button', { name: /discussion.resume/i }));

    expect(onResume).toHaveBeenCalled();
  });

  it('does not offer to resume a task that cannot be started', () => {
    render(<DiscussionPanel messages={thread()} onPost={() => {}} onResume={() => {}} canResume={false} />);

    // A done task can be discussed as a record without offering to restart it.
    expect(screen.getByRole('button', { name: /discussion.resume/i })).toBeDisabled();
  });

  it('says so when there is nothing yet', () => {
    render(<DiscussionPanel messages={[]} onPost={() => {}} onResume={() => {}} />);

    expect(screen.getByText('discussion.empty')).toBeInTheDocument();
  });

  it('blocks both actions while a message is being posted', async () => {
    let release;
    const onPost = vi.fn(() => new Promise((r) => (release = r)));
    render(<DiscussionPanel messages={thread()} onPost={onPost} onResume={() => {}} />);
    type(screen.getByRole('textbox'), 'one moment');

    click(screen.getByRole('button', { name: /discussion.post/i }));

    // Resuming mid-post would send a thread missing the message being written.
    expect(screen.getByRole('button', { name: /discussion.resume/i })).toBeDisabled();
    expect(screen.getByRole('textbox')).toBeDisabled();
    release();
  });
});
