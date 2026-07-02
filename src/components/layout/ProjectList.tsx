import { ProjectListItem } from './ProjectListItem';
import { CreateProjectModal, EditProjectModal, DeleteProjectModal } from './ProjectModals';
import { useProjectList } from '../../hooks/useProjectList';
import { invoke } from '@tauri-apps/api/core';
import { showNotification } from '../ui/Toast';

interface Props {
  selectedId: string | null;
  onSelect: (id: string | null) => void;
  onProjectName?: (name: string) => void;
  onProjectPath?: (path: string) => void;
}

export function ProjectList({ selectedId, onSelect, onProjectName, onProjectPath }: Props) {
  const {
    projects, search, setSearch,
    expanded, expandedSvc, svcCache, setSvcCache,
    actingId,
    showNewModal, setShowNewModal,
    ctxMenu, setCtxMenu,
    deleteTarget, setDeleteTarget,
    editTarget, setEditTarget,
    filtered, isProjectRunning, load,
    handleDuplicate, handleTogglePin,
    handleStart, handleStop,
    toggleSvcExpand, toggleExpand,
  } = useProjectList();

  return (
    <div className="h-full bg-nexus-surface flex flex-col select-none">
      {/* 搜索 + 新建 */}
      <SearchBar search={search} setSearch={setSearch} onNew={() => setShowNewModal(true)} />

      {/* 项目列表 */}
      <div className="flex-1 overflow-auto py-0.5">
        {filtered.length === 0 && (
          <EmptyState search={search} onNew={() => setShowNewModal(true)} />
        )}
        {filtered.map(p => (
          <ProjectListItem
            key={p.id}
            project={p}
            selected={selectedId === p.id}
            isExpanded={expanded.has(p.id)}
            services={svcCache[p.id] || []}
            isRunning={isProjectRunning(p.id)}
            actingId={actingId}
            expandedSvc={expandedSvc}
            onSelect={() => { onSelect(p.id); onProjectName?.(p.name); onProjectPath?.(p.path); }}
            onDoubleClick={e => toggleExpand(e, p.id)}
            onContextMenu={(e) => { e.preventDefault(); e.stopPropagation(); setCtxMenu({ id: p.id, name: p.name, path: p.path, x: e.clientX, y: e.clientY }); }}
            onStart={e => handleStart(e, p.id, p.name)}
            onStop={e => handleStop(e, p.id, p.name)}
            onTogglePin={e => handleTogglePin(e, p.id)}
            onToggleSvcExpand={serviceId => toggleSvcExpand({ stopPropagation: () => {} } as React.MouseEvent, p.id, serviceId)}
          />
        ))}
      </div>

      {/* 右键菜单 */}
      {ctxMenu && (
        <ContextMenu
          ctx={ctxMenu}
          onOpenInExplorer={async () => {
            try {
              await invoke('open_in_explorer', { path: ctxMenu.path });
            } catch (err) {
              console.error('打开资源管理器失败:', err);
              showNotification({ variant: 'error', title: '打开资源管理器失败' });
            }
            setCtxMenu(null);
          }}
          onDuplicate={() => { handleDuplicate(ctxMenu.id); setCtxMenu(null); }}
          onEdit={() => {
            const p = projects.find(pr => pr.id === ctxMenu.id);
            if (p) setEditTarget(p);
            setCtxMenu(null);
          }}
          onDelete={() => { setDeleteTarget({ id: ctxMenu.id, name: ctxMenu.name }); setCtxMenu(null); }}
        />
      )}

      {/* Modals */}
      <CreateProjectModal
        open={showNewModal}
        onClose={() => setShowNewModal(false)}
        onCreated={(p) => { load(); onSelect(p.id); onProjectName?.(p.name); }}
      />

      <EditProjectModal
        project={editTarget}
        onClose={() => setEditTarget(null)}
        onUpdated={load}
        onProjectName={onProjectName}
      />

      <DeleteProjectModal
        target={deleteTarget}
        onClose={() => setDeleteTarget(null)}
        onDeleted={() => {
          load();
          setSvcCache(prev => {
            const next = { ...prev };
            if (deleteTarget) delete next[deleteTarget.id];
            return next;
          });
        }}
        onDeselectIfSelected={() => {
          if (selectedId && deleteTarget && selectedId === deleteTarget.id) {
            onSelect(null);
          }
        }}
      />
    </div>
  );
}

// ── 子组件 ────────────────────────────────────────────────

function SearchBar({ search, setSearch, onNew }: {
  search: string;
  setSearch: (s: string) => void;
  onNew: () => void;
}) {
  return (
    <div className="flex items-center gap-2 px-3 py-2 flex-shrink-0">
      <div className="flex-1 relative">
        <svg className="absolute left-2.5 top-1/2 -translate-y-1/2 text-nexus-muted" width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.5"><circle cx="5" cy="5" r="3.5"/><line x1="8.5" y1="8.5" x2="11" y2="11"/></svg>
        <input className="w-full pl-7 pr-2 py-1 text-[12px] bg-nexus-bg border border-nexus-border rounded text-nexus-text placeholder:text-nexus-muted focus:outline-none focus:border-nexus-accent"
          placeholder="搜索项目..." value={search} onChange={e => setSearch(e.target.value)} />
      </div>
      <button
        className="flex-shrink-0 flex items-center gap-1 px-2.5 py-1 text-[12px] bg-nexus-accent text-white rounded hover:bg-nexus-accent-hover transition-colors"
        onClick={onNew}
      >
        <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.5"><line x1="6" y1="1" x2="6" y2="11"/><line x1="1" y1="6" x2="11" y2="6"/></svg>
        新建
      </button>
    </div>
  );
}

function EmptyState({ search, onNew }: { search: string; onNew: () => void }) {
  return (
    <div className="flex flex-col items-center justify-center py-16 px-4 text-center">
      {search.trim() ? (
        <span className="text-[12px] text-nexus-muted">未找到匹配的项目</span>
      ) : (
        <>
          <span className="text-[40px] opacity-[0.08] select-none font-extralight mb-2">N</span>
          <span className="text-[12px] text-nexus-muted mb-3">暂无项目</span>
          <button
            className="px-4 py-1.5 text-[12px] bg-nexus-accent text-white rounded hover:bg-nexus-accent-hover"
            onClick={onNew}
          >创建第一个项目</button>
        </>
      )}
    </div>
  );
}

function ContextMenu({ ctx, onOpenInExplorer, onDuplicate, onEdit, onDelete }: {
  ctx: { id: string; name: string; path: string; x: number; y: number };
  onOpenInExplorer: () => void;
  onDuplicate: () => void;
  onEdit: () => void;
  onDelete: () => void;
}) {
  // 计算菜单位置，确保不超出屏幕
  const menuStyle = {
    left: Math.min(ctx.x, window.innerWidth - 188),
    top: Math.min(ctx.y, window.innerHeight - 200),
  };

  return (
    <div
      className="fixed z-[70] w-[180px] bg-nexus-surface border border-nexus-border/60 rounded-lg shadow-2xl overflow-hidden"
      style={menuStyle}
    >
      {/* 操作项 */}
      <div className="py-1.5 px-1.5">
        <button
          className="w-full flex items-center gap-2 px-2.5 py-1.5 rounded-md hover:bg-nexus-accent/10 transition-colors group text-left"
          onClick={onOpenInExplorer}
        >
          <div className="w-5 h-5 rounded bg-nexus-bg border border-nexus-border/30 flex items-center justify-center flex-shrink-0 group-hover:border-nexus-accent/30">
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" className="text-nexus-muted group-hover:text-nexus-accent">
              <path d="M1.5 3h2l1-1.5h4a1 1 0 011 1v5.5a1 1 0 01-1 1h-7a1 1 0 01-1-1V3z"/>
            </svg>
          </div>
          <span className="text-[12px] text-nexus-text">在资源管理器中打开</span>
        </button>

        <button
          className="w-full flex items-center gap-2 px-2.5 py-1.5 rounded-md hover:bg-nexus-accent/10 transition-colors group text-left"
          onClick={onDuplicate}
        >
          <div className="w-5 h-5 rounded bg-nexus-bg border border-nexus-border/30 flex items-center justify-center flex-shrink-0 group-hover:border-nexus-accent/30">
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" className="text-nexus-muted group-hover:text-nexus-accent">
              <rect x="2.5" y="3" width="5" height="5.5" rx=".8"/>
              <path d="M2 2.5v4.5h.5V3.5h4V2.5H3a.5.5 0 00-.5.5z"/>
            </svg>
          </div>
          <span className="text-[12px] text-nexus-text">复制项目</span>
        </button>

        <button
          className="w-full flex items-center gap-2 px-2.5 py-1.5 rounded-md hover:bg-nexus-accent/10 transition-colors group text-left"
          onClick={onEdit}
        >
          <div className="w-5 h-5 rounded bg-nexus-bg border border-nexus-border/30 flex items-center justify-center flex-shrink-0 group-hover:border-nexus-accent/30">
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" className="text-nexus-muted group-hover:text-nexus-accent">
              <path d="M7 2l1 1-5.5 5.5H1.5V7.5L7 2z"/>
            </svg>
          </div>
          <span className="text-[12px] text-nexus-text">编辑项目</span>
        </button>
      </div>

      {/* 分隔线和删除 */}
      <div className="border-t border-nexus-border/30 py-1.5 px-1.5">
        <button
          className="w-full flex items-center gap-2 px-2.5 py-1.5 rounded-md hover:bg-nexus-error/10 transition-colors group text-left"
          onClick={onDelete}
        >
          <div className="w-5 h-5 rounded bg-nexus-bg border border-nexus-border/30 flex items-center justify-center flex-shrink-0 group-hover:border-nexus-error/30">
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" className="text-nexus-muted group-hover:text-nexus-error">
              <path d="M2.5 3h5M3.5 3V2a.5.5 0 01.5-.5h2a.5.5 0 01.5.5v1M4 4.5v3M6 4.5v3M3 3l.5 6a1 1 0 001 .5h3a1 1 0 001-.5L9 3"/>
            </svg>
          </div>
          <span className="text-[12px] text-nexus-muted group-hover:text-nexus-error">删除项目</span>
        </button>
      </div>
    </div>
  );
}
