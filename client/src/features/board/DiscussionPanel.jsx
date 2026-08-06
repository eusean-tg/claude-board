import { useState } from 'react';
import { MessageSquare, Send, Play } from 'lucide-react';
import { useTranslation } from '../../i18n/I18nProvider';

/**
 * A conversation about how a task should be approached.
 *
 * Posting and resuming are two separate acts. Writing a message changes nothing
 * about the work — no worktree, no branch, no process — so reconsidering costs
 * nothing. Sending the agent back in is the deliberate second step, and it needs
 * something in the thread to send.
 */
export default function DiscussionPanel({ messages = [], onPost, onResume, canResume = true }) {
  const { t } = useTranslation();
  const [body, setBody] = useState('');
  const [posting, setPosting] = useState(false);
  const [resuming, setResuming] = useState(false);

  const busy = posting || resuming;
  const canPost = body.trim().length > 0 && !busy;

  const post = async () => {
    if (!canPost) return;
    setPosting(true);
    try {
      await onPost(body.trim());
      setBody('');
    } finally {
      setPosting(false);
    }
  };

  const resume = async () => {
    if (!messages.length || busy) return;
    setResuming(true);
    try {
      await onResume();
    } finally {
      setResuming(false);
    }
  };

  return (
    <div className="rounded-xl border border-surface-700 bg-surface-800/40 p-4">
      <div className="flex items-center gap-2 mb-3">
        <MessageSquare size={14} className="text-claude" />
        <span className="text-xs font-medium text-surface-300">{t('discussion.title')}</span>
      </div>

      {messages.length === 0 ? (
        <p className="text-xs text-surface-500">{t('discussion.empty')}</p>
      ) : (
        <div className="space-y-2 mb-3 max-h-64 overflow-y-auto">
          {messages.map((m) => (
            <div
              key={m.id}
              className={`rounded-lg px-3 py-2 text-xs whitespace-pre-wrap ${
                m.role === 'agent'
                  ? 'bg-surface-800 text-surface-300 border border-surface-700'
                  : 'bg-claude/10 text-surface-200 border border-claude/20'
              }`}
            >
              <span className="block text-[10px] font-medium text-surface-500 mb-0.5">
                {m.role === 'agent' ? t('discussion.agent') : t('discussion.you')}
              </span>
              {m.body}
            </div>
          ))}
        </div>
      )}

      <textarea
        value={body}
        onChange={(e) => setBody(e.target.value)}
        placeholder={t('discussion.placeholder')}
        rows={3}
        disabled={busy}
        className="w-full px-2.5 py-2 text-sm bg-surface-800 border border-surface-700 rounded-lg text-surface-200 placeholder-surface-600 focus:outline-none focus:border-claude/50 resize-y"
      />

      <div className="mt-2 flex items-center justify-between gap-2">
        {/* Only offered once there is something to send. Resuming with an empty
            thread would restart the agent with nothing new to go on. */}
        <button
          onClick={resume}
          disabled={!messages.length || busy || !canResume}
          title={t('discussion.resumeHint')}
          className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-lg bg-surface-700/60 text-surface-300 hover:bg-surface-700 disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
        >
          <Play size={12} />
          {resuming ? t('discussion.resuming') : t('discussion.resume')}
        </button>
        <button
          onClick={post}
          disabled={!canPost}
          className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-lg bg-claude/20 text-claude hover:bg-claude/30 disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
        >
          <Send size={12} />
          {posting ? t('discussion.posting') : t('discussion.post')}
        </button>
      </div>
    </div>
  );
}
