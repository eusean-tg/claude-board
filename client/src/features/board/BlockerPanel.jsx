import { useMemo, useState } from 'react';
import { HelpCircle, Send, X } from 'lucide-react';
import { useTranslation } from '../../i18n/I18nProvider';

// The value the escape option carries, kept out of the numeric option-id space.
const ESCAPE = 'escape';

/**
 * The question an agent stopped to ask, and the controls to answer it.
 *
 * Three shapes, one per kind: radios for a single choice, checkboxes with a
 * per-option note for a multiple choice, a textarea for free text. Every kind also
 * offers a free-text escape, because the options are the agent's guess at the
 * answer space and it is often wrong.
 */
export default function BlockerPanel({ blocker, onAnswer, onCancel }) {
  const { t } = useTranslation();
  const [chosen, setChosen] = useState([]);
  const [notes, setNotes] = useState({});
  const [text, setText] = useState('');
  const [sending, setSending] = useState(false);

  const kind = blocker?.kind ?? 'free_text';
  const options = blocker?.options ?? [];
  const multi = kind === 'multi_choice';
  const escaping = chosen.includes(ESCAPE);
  // A free-text question is all escape, so it needs no separate escape control.
  const textIsTheAnswer = kind === 'free_text' || escaping;

  const responses = useMemo(() => {
    const trimmed = text.trim();
    if (kind === 'free_text') {
      return trimmed ? [{ optionId: null, note: null, freeText: trimmed }] : [];
    }
    const out = chosen
      .filter((c) => c !== ESCAPE)
      .map((id) => ({
        optionId: id,
        note: notes[id]?.trim() ? notes[id].trim() : null,
        freeText: null,
      }));
    if (escaping && trimmed) {
      out.push({ optionId: null, note: null, freeText: trimmed });
    }
    return out;
  }, [kind, chosen, notes, text, escaping]);

  if (!blocker) return null;

  const toggle = (value) => {
    setChosen((prev) => {
      if (multi) {
        return prev.includes(value) ? prev.filter((v) => v !== value) : [...prev, value];
      }
      // A single select replaces rather than accumulates, or the agent is left to
      // guess which of two answers won.
      return [value];
    });
  };

  const send = async () => {
    if (!responses.length || sending) return;
    setSending(true);
    try {
      await onAnswer(responses);
    } finally {
      setSending(false);
    }
  };

  const inputType = multi ? 'checkbox' : 'radio';

  return (
    <div className="rounded-xl border border-orange-500/30 bg-orange-500/5 p-4">
      <div className="flex items-start gap-2.5">
        <HelpCircle size={16} className="text-orange-400 mt-0.5 flex-shrink-0" />
        <div className="min-w-0 flex-1">
          {blocker.header ? (
            <span className="inline-block text-[10px] font-medium px-1.5 py-0.5 rounded bg-orange-500/15 text-orange-400 mb-1.5">
              {blocker.header}
            </span>
          ) : null}
          <p className="text-sm text-surface-100 font-medium">{blocker.question}</p>
          {blocker.context ? (
            <p className="mt-1.5 text-xs text-surface-400 whitespace-pre-wrap">{blocker.context}</p>
          ) : null}
        </div>
        <button
          onClick={onCancel}
          title={t('blocker.dismiss')}
          aria-label={t('blocker.dismiss')}
          className="p-1 rounded text-surface-500 hover:text-surface-300 hover:bg-surface-700/50 flex-shrink-0"
        >
          <X size={14} />
        </button>
      </div>

      <div className="mt-3 space-y-1.5">
        {options.map((opt) => {
          const picked = chosen.includes(opt.id);
          return (
            <div key={opt.id}>
              <label className="flex items-start gap-2 px-2 py-1.5 rounded-lg hover:bg-surface-800/50 cursor-pointer">
                {/* aria-label rather than relying on the wrapping label: the
                    label's text includes the description, which would make the
                    accessible name the option and its explanation run together. */}
                <input
                  type={inputType}
                  name={`blocker-${blocker.id}`}
                  aria-label={opt.label}
                  aria-describedby={opt.description ? `blocker-opt-${opt.id}-desc` : undefined}
                  checked={picked}
                  onChange={() => toggle(opt.id)}
                  disabled={sending}
                  className="mt-0.5 accent-orange-400"
                />
                <span className="min-w-0">
                  <span className="text-sm text-surface-200">{opt.label}</span>
                  {opt.description ? (
                    <span id={`blocker-opt-${opt.id}-desc`} className="block text-xs text-surface-500">
                      {opt.description}
                    </span>
                  ) : null}
                </span>
              </label>
              {/* Only for the options actually chosen: a note box against every
                  option is noise on a question offering ten of them. */}
              {multi && picked ? (
                <input
                  type="text"
                  value={notes[opt.id] ?? ''}
                  onChange={(e) => setNotes((prev) => ({ ...prev, [opt.id]: e.target.value }))}
                  placeholder={t('blocker.addContext')}
                  disabled={sending}
                  className="ml-7 mt-1 w-[calc(100%-1.75rem)] px-2 py-1 text-xs bg-surface-800 border border-surface-700 rounded-md text-surface-200 placeholder-surface-600 focus:outline-none focus:border-orange-500/50"
                />
              ) : null}
            </div>
          );
        })}

        {options.length > 0 ? (
          <label className="flex items-center gap-2 px-2 py-1.5 rounded-lg hover:bg-surface-800/50 cursor-pointer">
            <input
              type={inputType}
              name={`blocker-${blocker.id}`}
              checked={escaping}
              onChange={() => toggle(ESCAPE)}
              disabled={sending}
              className="accent-orange-400"
            />
            <span className="text-sm text-surface-400">{t('blocker.somethingElse')}</span>
          </label>
        ) : null}

        {textIsTheAnswer ? (
          <textarea
            value={text}
            onChange={(e) => setText(e.target.value)}
            placeholder={kind === 'free_text' ? t('blocker.answerPlaceholder') : t('blocker.escapePlaceholder')}
            rows={3}
            disabled={sending}
            className="w-full px-2.5 py-2 text-sm bg-surface-800 border border-surface-700 rounded-lg text-surface-200 placeholder-surface-600 focus:outline-none focus:border-orange-500/50 resize-y"
          />
        ) : null}
      </div>

      <div className="mt-3 flex justify-end">
        <button
          onClick={send}
          disabled={!responses.length || sending}
          className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-lg bg-orange-500/20 text-orange-300 hover:bg-orange-500/30 disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
        >
          <Send size={12} />
          {sending ? t('blocker.sending') : t('blocker.send')}
        </button>
      </div>
    </div>
  );
}
