import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { FileText, RefreshCw, Search, Pencil, Eye, FolderOpen, Copy, Save, Check } from 'lucide-react';
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
  groupByDirectory,
  formatSize,
  formatModified,
} from './artifactHelpers';

export default function ArtifactsView({ projectId, project, tasks, onViewDetail }) {
  const { t } = useTranslation();

  const [artifacts, setArtifacts] = useState([]);
  const [loading, setLoading] = useState(IS_TAURI);

  const [query, setQuery] = useState('');
  const [kind, setKind] = useState('all');

  const [selectedPath, setSelectedPath] = useState(null);
  const [content, setContent] = useState('');
  const [originalContent, setOriginalContent] = useState('');
  const [contentLoading, setContentLoading] = useState(false);

  const [editing, setEditing] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [copied, setCopied] = useState(false);

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
    setSelectedPath(null);
    setContent('');
    setOriginalContent('');
    setEditing(false);
    loadArtifacts();
  }, [loadArtifacts]);

  const selectArtifact = useCallback(
    async (artifact) => {
      setSelectedPath(artifact.rel_path);
      setEditing(false);
      setContentLoading(true);
      try {
        const data = await api.getArtifact(projectId, artifact.rel_path);
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
    },
    [projectId],
  );

  const handleSave = useCallback(async () => {
    if (!selectedPath) return;
    setSaving(true);
    try {
      await api.saveArtifact(projectId, selectedPath, content);
      setOriginalContent(content);
      setSaved(true);
      clearTimeout(savedTimer.current);
      savedTimer.current = setTimeout(() => setSaved(false), 2000);
    } catch (err) {
      notifyError(err?.message || tRef.current('artifacts.loadFailed'));
    } finally {
      setSaving(false);
    }
  }, [projectId, selectedPath, content]);

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
    if (!selectedPath) return;
    try {
      await api.revealArtifact(projectId, selectedPath);
    } catch (err) {
      notifyError(err?.message || tRef.current('artifacts.loadFailed'));
    }
  }, [projectId, selectedPath]);

  const handleCopyPath = useCallback(async () => {
    if (!selectedPath) return;
    try {
      await navigator.clipboard.writeText(selectedPath);
      setCopied(true);
      clearTimeout(copiedTimer.current);
      copiedTimer.current = setTimeout(() => setCopied(false), 2000);
    } catch (err) {
      notifyError(err?.message || tRef.current('artifacts.loadFailed'));
    }
  }, [selectedPath]);

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
  const groups = useMemo(() => groupByDirectory(filtered), [filtered]);
  const selected = useMemo(
    () => artifacts.find((artifact) => artifact.rel_path === selectedPath) ?? null,
    [artifacts, selectedPath],
  );

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
              <div key={group.dir}>
                <div className="sticky top-0 z-10 bg-surface-900 px-3 py-1.5 text-[10px] font-medium text-surface-500 truncate">
                  {group.dir || project?.name || '/'}
                </div>
                {group.items.map((artifact) => {
                  const isSelected = artifact.rel_path === selectedPath;
                  return (
                    <button
                      key={artifact.rel_path}
                      onClick={() => selectArtifact(artifact)}
                      className={`w-full text-left px-3 py-2 border-b border-surface-800/50 transition-colors ${
                        isSelected ? 'bg-claude/15 text-claude' : 'hover:bg-surface-800/40'
                      }`}
                    >
                      <div className="flex items-center gap-2">
                        <span className={`text-xs truncate ${isSelected ? '' : 'text-surface-200'}`}>
                          {artifact.title || artifact.name}
                        </span>
                        <span className="ml-auto flex-shrink-0 text-[9px] font-medium px-1 py-0.5 rounded bg-surface-800 text-surface-400">
                          {t(KIND_LABEL_KEYS[artifact.kind] ?? KIND_LABEL_KEYS.other)}
                        </span>
                      </div>
                      <div className="text-[10px] text-surface-600 truncate mt-0.5">{artifact.rel_path}</div>
                      <div className="flex items-center gap-2 text-[10px] text-surface-600 mt-0.5">
                        <span>{formatModified(artifact.modified_at)}</span>
                        <span>{formatSize(artifact.size_bytes)}</span>
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
                <span className="text-xs text-surface-200 truncate">{selected.name}</span>
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
                </div>
              </div>

              {selected.tasks?.length > 0 && (
                <div className="flex items-center gap-1.5 flex-wrap px-4 py-1.5 border-b border-surface-800 flex-shrink-0">
                  <span className="text-[10px] text-surface-500">{t('artifacts.createdBy')}</span>
                  {selected.tasks.map((task) => (
                    <button
                      key={task.task_id}
                      onClick={() => handleTaskChip(task.task_id)}
                      className="flex items-center gap-1 px-1.5 py-0.5 rounded bg-surface-800 hover:bg-surface-700 text-[10px] text-surface-300 hover:text-claude transition-colors max-w-[240px]"
                    >
                      <span className="font-mono text-surface-500">{task.task_key || `#${task.task_id}`}</span>
                      <span className="truncate">{task.title}</span>
                    </button>
                  ))}
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
