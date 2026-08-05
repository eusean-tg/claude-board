import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { FileText, RefreshCw, Search, Pencil, Eye, FolderOpen, Copy, Save, Check, Trash2 } from 'lucide-react';
import MDEditor from '@uiw/react-md-editor';
import { api, notifyError } from '../../lib/api';
import { IS_TAURI } from '../../lib/tauriEvents';
import { useTranslation } from '../../i18n/I18nProvider';
import Spinner from '../../components/Spinner';
import EmptyState from '../../components/EmptyState';
import {
  ARTIFACT_KINDS,
  KIND_LABEL_KEYS,
  filterArtifacts,
  groupByKind,
  formatSize,
  formatModified,
} from './artifactHelpers';

export default function ArtifactsView({ projectId, project, tasks, onViewDetail }) {
  const { t } = useTranslation();

  const [artifacts, setArtifacts] = useState([]);
  const [loading, setLoading] = useState(IS_TAURI);

  const [query, setQuery] = useState('');
  const [kind, setKind] = useState('all');

  const [selectedId, setSelectedId] = useState(null);
  const [content, setContent] = useState('');
  const [originalContent, setOriginalContent] = useState('');
  const [contentLoading, setContentLoading] = useState(false);

  const [editing, setEditing] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [copied, setCopied] = useState(false);
  const [deleting, setDeleting] = useState(false);

  // `t` gets a new identity whenever the language changes. Reading it through a ref keeps it
  // out of the fetch callbacks' deps, so switching language can't refetch and drop the selection.
  const tRef = useRef(t);
  tRef.current = t;

  const savedTimer = useRef(null);
  const copiedTimer = useRef(null);

  useEffect(
    () => () => {
      clearTimeout(savedTimer.current);
      clearTimeout(copiedTimer.current);
    },
    [],
  );

  const loadArtifacts = useCallback(async () => {
    if (!IS_TAURI) return;
    setLoading(true);
    try {
      const list = await api.listArtifacts(projectId);
      setArtifacts(Array.isArray(list) ? list : []);
    } catch (err) {
      notifyError(err?.message || tRef.current('artifacts.loadFailed'));
      setArtifacts([]);
    } finally {
      setLoading(false);
    }
  }, [projectId]);

  // Switching projects invalidates the current selection along with the list.
  useEffect(() => {
    setSelectedId(null);
    setContent('');
    setOriginalContent('');
    setEditing(false);
    loadArtifacts();
  }, [loadArtifacts]);

  const selectArtifact = useCallback(async (artifact) => {
    setSelectedId(artifact.id);
    setEditing(false);
    setContentLoading(true);
    try {
      const data = await api.getArtifact(artifact.id);
      const text = data?.content ?? '';
      setContent(text);
      setOriginalContent(text);
    } catch (err) {
      notifyError(err?.message || tRef.current('artifacts.loadFailed'));
      setContent('');
      setOriginalContent('');
    } finally {
      setContentLoading(false);
    }
  }, []);

  const handleSave = useCallback(async () => {
    if (!selectedId) return;
    setSaving(true);
    try {
      const updated = await api.updateArtifact(selectedId, content);
      setOriginalContent(content);
      // The title, preview, kind and size are re-derived from the new content, so
      // the row has to be refreshed or the list keeps showing the old heading.
      if (updated) {
        setArtifacts((prev) => prev.map((a) => (a.id === updated.id ? updated : a)));
      }
      setSaved(true);
      clearTimeout(savedTimer.current);
      savedTimer.current = setTimeout(() => setSaved(false), 2000);
    } catch (err) {
      notifyError(err?.message || tRef.current('artifacts.saveFailed'));
    } finally {
      setSaving(false);
    }
  }, [selectedId, content]);

  useEffect(() => {
    if (!editing) return;
    const handler = (e) => {
      if ((e.ctrlKey || e.metaKey) && e.key === 's') {
        e.preventDefault();
        handleSave();
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [editing, handleSave]);

  const handleReveal = useCallback(async () => {
    if (!selectedId) return;
    try {
      await api.revealArtifact(selectedId);
    } catch (err) {
      notifyError(err?.message || tRef.current('artifacts.loadFailed'));
    }
  }, [selectedId]);

  // The absolute store path, not the source path: this is the form to hand an
  // agent so it can read the document itself.
  const handleCopyPath = useCallback(async () => {
    if (!selectedId) return;
    try {
      const ref = await api.artifactReference(selectedId);
      await navigator.clipboard.writeText(ref?.path ?? '');
      setCopied(true);
      clearTimeout(copiedTimer.current);
      copiedTimer.current = setTimeout(() => setCopied(false), 2000);
    } catch (err) {
      notifyError(err?.message || tRef.current('artifacts.loadFailed'));
    }
  }, [selectedId]);

  const handleDelete = useCallback(async () => {
    if (!selectedId) return;
    if (!window.confirm(tRef.current('artifacts.deleteConfirm'))) return;
    setDeleting(true);
    try {
      await api.deleteArtifact(selectedId);
      setArtifacts((prev) => prev.filter((a) => a.id !== selectedId));
      setSelectedId(null);
      setContent('');
      setOriginalContent('');
      setEditing(false);
    } catch (err) {
      notifyError(err?.message || tRef.current('artifacts.deleteFailed'));
    } finally {
      setDeleting(false);
    }
  }, [selectedId]);

  // Task chips carry only ids; prefer the already-loaded task so the detail view opens instantly.
  const handleTaskChip = useCallback(
    async (taskId) => {
      if (!onViewDetail) return;
      const local = tasks?.find((task) => task.id === taskId);
      if (local) {
        onViewDetail(local);
        return;
      }
      try {
        const fetched = await api.getTask(taskId);
        if (fetched) onViewDetail(fetched);
      } catch {
        // A task that no longer exists just doesn't open — nothing to recover from.
      }
    },
    [tasks, onViewDetail],
  );

  const filtered = useMemo(() => filterArtifacts(artifacts, { query, kind }), [artifacts, query, kind]);
  const groups = useMemo(() => groupByKind(filtered), [filtered]);
  const selected = useMemo(
    () => artifacts.find((artifact) => artifact.id === selectedId) ?? null,
    [artifacts, selectedId],
  );

  // An artifact records who first wrote it and who last wrote it; show each once.
  const authoringTaskIds = useMemo(() => {
    if (!selected) return [];
    return [...new Set([selected.origin_task_id, selected.last_task_id].filter(Boolean))];
  }, [selected]);

  const hasChanges = content !== originalContent;

  if (!IS_TAURI) {
    return (
      <div className="h-full flex flex-col items-center justify-center">
        <EmptyState icon={FileText} title={t('artifacts.title')} description={t('artifacts.desktopOnly')} />
      </div>
    );
  }

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Toolbar */}
      <div className="flex items-center gap-2 px-4 py-2 border-b border-surface-800 flex-shrink-0">
        <div className="relative">
          <Search size={13} className="absolute left-2.5 top-1/2 -translate-y-1/2 text-surface-500" />
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t('artifacts.search')}
            className="w-40 lg:w-56 pl-8 pr-3 py-1.5 bg-surface-800 border border-surface-700 rounded-lg text-xs focus:outline-none focus:ring-1 focus:ring-claude focus:border-claude placeholder-surface-600"
          />
        </div>

        <div className="flex items-center gap-1 overflow-x-auto">
          {ARTIFACT_KINDS.map((k) => (
            <button
              key={k}
              onClick={() => setKind(k)}
              className={`px-2 py-1 rounded-lg text-[10px] font-medium whitespace-nowrap transition-colors ${
                kind === k
                  ? 'bg-claude/15 text-claude'
                  : 'bg-surface-800 text-surface-400 hover:bg-surface-700 hover:text-surface-200'
              }`}
            >
              {t(KIND_LABEL_KEYS[k])}
            </button>
          ))}
        </div>

        <span className="ml-auto text-[10px] text-surface-500">
          {t('artifacts.fileCount', { count: filtered.length })}
        </span>

        <button
          onClick={loadArtifacts}
          disabled={loading}
          title={t('artifacts.refresh')}
          className="p-1.5 rounded-lg hover:bg-surface-800 text-surface-400 hover:text-claude disabled:opacity-40 transition-colors"
        >
          <RefreshCw size={13} className={loading ? 'animate-spin' : ''} />
        </button>
      </div>

      {/* Panes */}
      <div className="flex-1 flex min-h-0">
        <div className="w-[320px] flex-shrink-0 border-r border-surface-800 overflow-y-auto">
          {loading ? (
            <Spinner />
          ) : groups.length === 0 ? (
            <EmptyState icon={FileText} title={t('artifacts.empty')} description={t('artifacts.emptyDesc')} />
          ) : (
            groups.map((group) => (
              <div key={group.kind}>
                <div className="sticky top-0 z-10 bg-surface-900 px-3 py-1.5 text-[10px] font-medium text-surface-500 truncate">
                  {t(KIND_LABEL_KEYS[group.kind] ?? KIND_LABEL_KEYS.other)}
                </div>
                {group.artifacts.map((artifact) => {
                  const isSelected = artifact.id === selectedId;
                  return (
                    <button
                      key={artifact.id}
                      onClick={() => selectArtifact(artifact)}
                      className={`w-full text-left px-3 py-2 border-b border-surface-800/50 transition-colors ${
                        isSelected ? 'bg-claude/15 text-claude' : 'hover:bg-surface-800/40'
                      }`}
                    >
                      <div className="flex items-center gap-2">
                        <span className={`text-xs truncate ${isSelected ? '' : 'text-surface-200'}`}>
                          {artifact.title || artifact.stored_name}
                        </span>
                        <span className="ml-auto flex-shrink-0 text-[9px] font-medium px-1 py-0.5 rounded bg-surface-800 text-surface-400">
                          {t(KIND_LABEL_KEYS[artifact.kind] ?? KIND_LABEL_KEYS.other)}
                        </span>
                      </div>
                      <div className="text-[10px] text-surface-600 truncate mt-0.5">{artifact.source_rel_path}</div>
                      <div className="flex items-center gap-2 text-[10px] text-surface-600 mt-0.5">
                        <span>{formatModified(artifact.updated_at)}</span>
                        <span>{formatSize(artifact.size)}</span>
                      </div>
                    </button>
                  );
                })}
              </div>
            ))
          )}
        </div>

        <div className="flex-1 min-h-0 flex flex-col">
          {!selected ? (
            <div className="flex-1 flex items-center justify-center text-surface-500 text-sm">
              {t('artifacts.selectPrompt')}
            </div>
          ) : (
            <>
              <div className="flex items-center gap-2 px-4 py-2 border-b border-surface-800 flex-shrink-0">
                <FileText size={14} className="text-claude flex-shrink-0" />
                <span className="text-xs text-surface-200 truncate">{selected.title || selected.source_rel_path}</span>
                {editing && hasChanges && (
                  <span className="text-[10px] px-1.5 py-0.5 rounded bg-blue-500/15 text-blue-400 flex-shrink-0">
                    {t('artifacts.unsaved')}
                  </span>
                )}
                {saved && (
                  <span className="flex items-center gap-1 text-[10px] text-emerald-400 flex-shrink-0">
                    <Check size={11} />
                    {t('artifacts.saved')}
                  </span>
                )}

                <div className="ml-auto flex items-center gap-1 flex-shrink-0">
                  {editing && (
                    <button
                      onClick={handleSave}
                      disabled={saving || !hasChanges}
                      className="flex items-center gap-1.5 px-2.5 py-1 rounded-lg bg-claude hover:bg-claude-light disabled:opacity-40 disabled:cursor-not-allowed text-[10px] font-medium transition-colors"
                    >
                      <Save size={11} />
                      {t('artifacts.save')}
                    </button>
                  )}
                  <button
                    onClick={() => setEditing((prev) => !prev)}
                    className="flex items-center gap-1.5 px-2.5 py-1 rounded-lg bg-surface-800 text-surface-300 hover:bg-surface-700 hover:text-surface-100 text-[10px] font-medium transition-colors"
                  >
                    {editing ? <Eye size={11} /> : <Pencil size={11} />}
                    {editing ? t('artifacts.preview') : t('artifacts.edit')}
                  </button>
                  <button
                    onClick={handleReveal}
                    className="flex items-center gap-1.5 px-2.5 py-1 rounded-lg bg-surface-800 text-surface-300 hover:bg-surface-700 hover:text-surface-100 text-[10px] font-medium transition-colors"
                  >
                    <FolderOpen size={11} />
                    {t('artifacts.reveal')}
                  </button>
                  <button
                    onClick={handleCopyPath}
                    className="flex items-center gap-1.5 px-2.5 py-1 rounded-lg bg-surface-800 text-surface-300 hover:bg-surface-700 hover:text-surface-100 text-[10px] font-medium transition-colors"
                  >
                    <Copy size={11} />
                    {copied ? t('artifacts.copied') : t('artifacts.copyPath')}
                  </button>
                  <button
                    onClick={handleDelete}
                    disabled={deleting}
                    title={t('artifacts.delete')}
                    className="flex items-center gap-1.5 px-2.5 py-1 rounded-lg bg-surface-800 text-surface-300 hover:bg-red-500/15 hover:text-red-400 disabled:opacity-40 text-[10px] font-medium transition-colors"
                  >
                    <Trash2 size={11} />
                    {t('artifacts.delete')}
                  </button>
                </div>
              </div>

              {authoringTaskIds.length > 0 && (
                <div className="flex items-center gap-1.5 flex-wrap px-4 py-1.5 border-b border-surface-800 flex-shrink-0">
                  <span className="text-[10px] text-surface-500">{t('artifacts.createdBy')}</span>
                  {authoringTaskIds.map((taskId) => {
                    // A task that has since been deleted leaves the artifact intact
                    // with its attribution nulled, so this only renders live ids.
                    const task = tasks?.find((candidate) => candidate.id === taskId);
                    return (
                      // Stacked rather than side by side: a task key and a title
                      // on one line overflow the chip and wrap. `max-w-full` on
                      // the title is what lets `truncate` engage — in a flex
                      // column the span is content-sized otherwise.
                      <button
                        key={taskId}
                        onClick={() => handleTaskChip(taskId)}
                        className="flex flex-col items-start px-1.5 py-0.5 rounded bg-surface-800 hover:bg-surface-700 text-[10px] text-left text-surface-300 hover:text-claude transition-colors max-w-[180px]"
                      >
                        <span className="font-mono text-[9px] leading-tight text-surface-500">
                          {task?.task_key || `#${taskId}`}
                        </span>
                        {task?.title && <span className="max-w-full truncate leading-tight">{task.title}</span>}
                      </button>
                    );
                  })}
                </div>
              )}

              <div className="flex-1 min-h-0 overflow-auto" data-color-mode="dark">
                {contentLoading ? (
                  <Spinner />
                ) : editing ? (
                  <MDEditor
                    value={content}
                    onChange={(val) => setContent(val || '')}
                    height="100%"
                    visibleDragbar={false}
                    preview="live"
                    style={{ backgroundColor: 'transparent', height: '100%' }}
                  />
                ) : (
                  <div className="px-4 py-3">
                    <MDEditor.Markdown
                      source={content}
                      style={{
                        backgroundColor: 'transparent',
                        color: '#a8a29e',
                        fontSize: '12px',
                        lineHeight: '1.6',
                      }}
                    />
                  </div>
                )}
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
