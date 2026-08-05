import { useState, useEffect, useCallback, useMemo } from 'react';
import { FileText, Search, Plus, X } from 'lucide-react';
import { api, notifyError } from '../../lib/api';
import { IS_TAURI } from '../../lib/tauriEvents';
import { useTranslation } from '../../i18n/I18nProvider';
import { KIND_LABEL_KEYS, filterArtifacts } from './artifactHelpers';

/**
 * Pick stored documents for a task to read and update.
 *
 * A reference is a relation, not text inserted into the description: the prompt
 * builder, the task detail view and blockers all query it. Picking therefore
 * calls addArtifactRef rather than editing any field.
 */
export default function ArtifactPicker({ projectId, taskId, onChange }) {
  const { t } = useTranslation();

  const [available, setAvailable] = useState([]);
  const [referenced, setReferenced] = useState([]);
  const [query, setQuery] = useState('');
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    if (!IS_TAURI || !projectId) return;
    try {
      const [all, mine] = await Promise.all([
        api.listArtifacts(projectId),
        taskId ? api.taskArtifacts(taskId) : Promise.resolve([]),
      ]);
      setAvailable(Array.isArray(all) ? all : []);
      setReferenced(Array.isArray(mine) ? mine : []);
    } catch (err) {
      notifyError(err?.message || t('artifacts.loadFailed'));
    }
    // `t` is intentionally omitted: it changes identity on every language switch
    // and would refetch the list for no reason.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectId, taskId]);

  useEffect(() => {
    load();
  }, [load]);

  const referencedIds = useMemo(() => new Set(referenced.map((a) => a.id)), [referenced]);

  const candidates = useMemo(
    () => filterArtifacts(available, { query }).filter((a) => !referencedIds.has(a.id)),
    [available, query, referencedIds],
  );

  const add = useCallback(
    async (artifact) => {
      if (!taskId || busy) return;
      setBusy(true);
      try {
        const next = await api.addArtifactRef(taskId, artifact.id, 'reference');
        setReferenced(Array.isArray(next) ? next : []);
        onChange?.(next);
      } catch (err) {
        notifyError(err?.message || t('artifacts.refAddFailed'));
      } finally {
        setBusy(false);
      }
    },
    [taskId, busy, onChange, t],
  );

  const remove = useCallback(
    async (artifact) => {
      if (!taskId || busy) return;
      setBusy(true);
      try {
        const next = await api.removeArtifactRef(taskId, artifact.id);
        setReferenced(Array.isArray(next) ? next : []);
        onChange?.(next);
      } catch (err) {
        notifyError(err?.message || t('artifacts.refRemoveFailed'));
      } finally {
        setBusy(false);
      }
    },
    [taskId, busy, onChange, t],
  );

  if (!IS_TAURI) return null;

  return (
    <div className="flex flex-col gap-2">
      {referenced.length > 0 && (
        <div className="flex flex-col gap-1">
          <div className="text-[10px] text-surface-500">{t('artifacts.alreadyReferenced')}</div>
          {referenced.map((artifact) => (
            <div key={artifact.id} className="flex items-center gap-2 px-2 py-1 rounded bg-surface-800 text-[11px]">
              <FileText size={11} className="text-claude flex-shrink-0" />
              <span className="truncate text-surface-200">{artifact.title || artifact.stored_name}</span>
              <span className="ml-auto flex-shrink-0 text-[9px] text-surface-500">
                {t(KIND_LABEL_KEYS[artifact.kind] ?? KIND_LABEL_KEYS.other)}
              </span>
              <button
                onClick={() => remove(artifact)}
                disabled={busy}
                title={t('artifacts.refRemove')}
                className="flex-shrink-0 p-0.5 rounded text-surface-500 hover:text-red-400 disabled:opacity-40 transition-colors"
              >
                <X size={11} />
              </button>
            </div>
          ))}
        </div>
      )}

      <div className="relative">
        <Search size={12} className="absolute left-2 top-1/2 -translate-y-1/2 text-surface-500" />
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={t('artifacts.refSearch')}
          className="w-full pl-7 pr-2 py-1.5 bg-surface-800 border border-surface-700 rounded-lg text-[11px] focus:outline-none focus:ring-1 focus:ring-claude focus:border-claude placeholder-surface-600"
        />
      </div>

      {candidates.length === 0 ? (
        <div className="text-[10px] text-surface-600 px-2 py-1">{t('artifacts.refNone')}</div>
      ) : (
        <div className="flex flex-col gap-1 max-h-40 overflow-y-auto">
          {candidates.map((artifact) => (
            <button
              key={artifact.id}
              onClick={() => add(artifact)}
              disabled={busy}
              className="flex items-center gap-2 px-2 py-1 rounded text-left text-[11px] hover:bg-surface-800 disabled:opacity-40 transition-colors"
            >
              <Plus size={11} className="text-surface-500 flex-shrink-0" />
              <span className="truncate text-surface-300">{artifact.title || artifact.stored_name}</span>
              <span className="ml-auto flex-shrink-0 text-[9px] text-surface-500">
                {t(KIND_LABEL_KEYS[artifact.kind] ?? KIND_LABEL_KEYS.other)}
              </span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
