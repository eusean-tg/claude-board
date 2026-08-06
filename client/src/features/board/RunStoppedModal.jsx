import { useState } from 'react';
import { AlertTriangle, GitBranch, Play, Trash2 } from 'lucide-react';
import ModalShell from '../../components/ModalShell';
import { api } from '../../lib/api';
import { useTranslation } from '../../i18n/I18nProvider';

/**
 * Resolves a dependency run that stopped because a member's work could not reach
 * the trunk.
 *
 * Two ways out, and the panel does not assume which one the user took. Carrying on
 * re-attempts the merge, so it works whether they merged the branch by hand or
 * fixed whatever caused the conflict. Abandoning releases the tasks and keeps the
 * trunk, because that trunk holds every member's work that did merge.
 */
export default function RunStoppedModal({ task, onClose, onResolved }) {
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(null);
  const [confirmingAbandon, setConfirmingAbandon] = useState(false);

  if (!task) return null;

  const resume = async () => {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      const result = await api.resumeStoppedRun(task.id);
      onResolved?.({ kind: 'resumed', result });
    } catch (e) {
      // Kept open on failure: closing would leave no sign the run did not move.
      setError(e?.message || String(e));
    } finally {
      setBusy(false);
    }
  };

  const abandon = async () => {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      const result = await api.abandonRun(task.id);
      onResolved?.({ kind: 'abandoned', result });
    } catch (e) {
      setError(e?.message || String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <ModalShell
      title={t('runStopped.title')}
      subtitle={t('runStopped.body')}
      icon={AlertTriangle}
      iconClass="text-red-400"
      onClose={onClose}
    >
      <div className="px-5 py-4">
        {task.trunk_branch ? (
          <div className="flex items-center gap-2 text-xs text-surface-500">
            <GitBranch size={12} />
            <span>{t('runStopped.trunk')}</span>
            <code className="text-surface-300">{task.trunk_branch}</code>
          </div>
        ) : null}

        {error ? (
          <p className="mt-3 text-xs text-red-400 bg-red-500/10 border border-red-500/20 rounded-lg px-3 py-2">
            {error}
          </p>
        ) : null}

        <div className="mt-4 flex justify-end gap-2 pt-1">
          <button
            onClick={onClose}
            disabled={busy}
            className="px-3 py-1.5 text-xs text-surface-400 hover:text-surface-200 disabled:opacity-40 transition-colors"
          >
            {t('runStopped.cancel')}
          </button>
          {/* Two clicks: abandoning frees the tasks and gives up the run, and the
              button sits next to the one people mean to press. */}
          {confirmingAbandon ? (
            <button
              onClick={abandon}
              disabled={busy}
              className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-lg bg-red-600 hover:bg-red-500 text-white disabled:opacity-50 transition-colors"
            >
              <Trash2 size={12} />
              {t('runStopped.abandonConfirm')}
            </button>
          ) : (
            <button
              onClick={() => setConfirmingAbandon(true)}
              disabled={busy}
              className="px-3 py-1.5 text-xs font-medium rounded-lg text-red-400 hover:bg-red-500/10 disabled:opacity-40 transition-colors"
            >
              {t('runStopped.abandon')}
            </button>
          )}
          <button
            onClick={resume}
            disabled={busy}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-lg bg-claude hover:bg-claude-light text-white disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
          >
            <Play size={12} />
            {busy ? t('runStopped.resuming') : t('runStopped.resume')}
          </button>
        </div>
      </div>
    </ModalShell>
  );
}
