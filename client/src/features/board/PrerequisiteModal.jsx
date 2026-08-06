import { useState } from 'react';
import { GitBranch, Layers, Play } from 'lucide-react';
import ModalShell from '../../components/ModalShell';
import { useTranslation } from '../../i18n/I18nProvider';

/**
 * Confirms running a task's prerequisites before starting a chain.
 *
 * Shown instead of the ordinary start confirmation when a task has unfinished
 * dependencies, because the two decisions are not the same size: one starts an
 * agent, this starts several. The wave order is on display so confirming is an
 * informed choice rather than a leap.
 */
export default function PrerequisiteModal({ waves = [], trunkBranch, onConfirm, onClose }) {
  const { t } = useTranslation();
  const [starting, setStarting] = useState(false);

  if (!waves.length) return null;

  const total = waves.reduce((n, wave) => n + wave.length, 0);
  const lastIndex = waves.length - 1;

  const confirm = async () => {
    if (starting) return;
    setStarting(true);
    try {
      await onConfirm();
    } finally {
      setStarting(false);
    }
  };

  return (
    <ModalShell
      title={t('prerequisites.title')}
      subtitle={t('prerequisites.body', { count: total })}
      icon={Layers}
      onClose={onClose}
    >
      {/* ModalShell pads its header and leaves the body to its consumer, so the
          padding here is what keeps this modal flush with every other one. */}
      <div className="px-5 py-4">
        <ol className="space-y-2">
          {waves.map((wave, i) => (
            <li
              key={i}
              className="flex items-start gap-3 px-3 py-2.5 rounded-lg bg-surface-800/50 border border-surface-700/50"
            >
              <span className="flex-shrink-0 w-5 h-5 rounded-full bg-surface-700 text-surface-300 text-[10px] font-medium flex items-center justify-center mt-0.5">
                {i + 1}
              </span>
              <span className="min-w-0 flex-1">
                {wave.map((task) => (
                  <span key={task.id} className="block text-sm text-surface-200 truncate">
                    {task.title}
                  </span>
                ))}
                {/* Which of these steps is the one they clicked is not otherwise
                    obvious, and it is the only one they asked for directly. */}
                {i === lastIndex ? (
                  <span className="block text-[10px] text-claude mt-1">{t('prerequisites.target')}</span>
                ) : wave.length > 1 ? (
                  <span className="block text-[10px] text-surface-500 mt-1">{t('prerequisites.together')}</span>
                ) : null}
              </span>
            </li>
          ))}
        </ol>

        {trunkBranch ? (
          <div className="mt-3 flex items-center gap-2 text-xs text-surface-500">
            <GitBranch size={12} />
            <span>{t('prerequisites.trunk')}</span>
            <code className="text-surface-400">{trunkBranch}</code>
          </div>
        ) : null}

        <div className="mt-4 flex justify-end gap-2 pt-1">
          <button
            onClick={onClose}
            disabled={starting}
            className="px-3 py-1.5 text-xs text-surface-400 hover:text-surface-200 disabled:opacity-40 transition-colors"
          >
            {t('prerequisites.cancel')}
          </button>
          <button
            onClick={confirm}
            disabled={starting}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-lg bg-claude hover:bg-claude-light text-white disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
          >
            <Play size={12} />
            {starting ? t('prerequisites.starting') : t('prerequisites.confirm', { count: total })}
          </button>
        </div>
      </div>
    </ModalShell>
  );
}
