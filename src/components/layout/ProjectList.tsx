import { Fragment, useEffect, useRef } from 'react';
import { ProjectListItem } from './ProjectListItem';
import { CreateProjectModal, EditProjectModal, DeleteProjectModal, DuplicateProjectModal } from './ProjectModals';
import { useProjectList } from '../../hooks/useProjectList';
import { useSvcCacheStore } from '../../stores/svcCacheStore';
import { openInExplorer, openTerminal } from '../../services/system';
import { PanelToggleIcon } from '../ui/PanelToggleIcon';
import { ContextMenu, ContextMenuItem } from '../ui/ContextMenu';
import { needsFavoriteDivider } from '../../utils/projectList';

interface Props {
  selectedId: string | null;
  onSelect: (id: string | null) => void;
  onProjectName?: (name: string) => void;
  onProjectPath?: (path: string) => void;
  /** 收起项目列（由 MainLayout 提供） */
  onCollapse?: () => void;
}

/**
 * 展开/收起服务分组时需要的"事件"其实是空的：子组件（`ProjectListItem`）只回传
 * serviceId，没有真实 DOM 事件。这里给一个名实相符的 no-op，而不是伪造一个
 * `as React.MouseEvent` 的假事件（CQ-21，见 `useProjectList.toggleSvcExpand` 的说明）。
 */
const NO_DOM_EVENT = { stopPropagation: () => {} };

export function ProjectList({ selectedId, onSelect, onProjectName, onProjectPath, onCollapse }: Props) {
  const {
    projects, search, setSearch,
    expanded, expandedSvc, svcCache, expandingId,
    actingId,
    showNewModal, setShowNewModal,
    ctxMenu, setCtxMenu,
    deleteTarget, setDeleteTarget,
    editTarget, setEditTarget,
    duplicateTarget, setDuplicateTarget,
    filtered, isProjectRunning, load,
    handleTogglePin,
    handleStart, handleStop,
    toggleSvcExpand, toggleExpand,
  } = useProjectList();

  /**
   * 选中项滚进视野。
   *
   * 此前列表不做任何滚动：新建项目（`onCreated` 里会选中它）或从别处选中时，
   * 只要它在视野之外，用户就"选中了却看不见"——收藏多的时候尤其明显。
   * `block: 'nearest'`：只滚必要的那点距离，不打断用户当前的浏览位置。
   */
  const listRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (!selectedId) return;
    const items = listRef.current?.querySelectorAll<HTMLElement>('[data-project-id]');
    const el = items && Array.from(items).find(e => e.dataset.projectId === selectedId);
    el?.scrollIntoView({ block: 'nearest' });
  }, [selectedId]);

  return (
    <div className="h-full bg-nexus-surface flex flex-col select-none">
      {/* 搜索 + 新建 + 收起 */}
      <SearchBar search={search} setSearch={setSearch} onNew={() => setShowNewModal(true)} onCollapse={onCollapse} />

      {/* 项目列表。收藏的项目由后端排在前面（`ORDER BY pinned DESC, sort_index`），
          行上那个 ★ 显示自己是不是收藏；两者之间再画一条线，与收起态的窄轨是**同一条规则**
          （`needsFavoriteDivider`，边界条件在那里有测试）——区分更直接，也免去了分组标题 */}
      <div ref={listRef} className="flex-1 overflow-auto py-0.5">
        {filtered.length === 0 && (
          <EmptyState search={search} onNew={() => setShowNewModal(true)} />
        )}
        {filtered.map((p, i) => (
          <Fragment key={p.id}>
            {/* 收藏与其余之间的分隔线。上下的间距由"上一项的 mb-1.5"与"本线自己的 mb-1.5"
                各出一半，所以视觉上是等距的 */}
            {needsFavoriteDivider(filtered, i) && (
              <div className="mx-3 mb-1.5 h-px bg-nexus-border" />
            )}
            <ProjectListItem
              project={p}
              selected={selectedId === p.id}
              isExpanded={expanded.has(p.id)}
              expanding={expandingId === p.id}
              services={svcCache[p.id] || []}
              isRunning={isProjectRunning(p.id)}
              actingId={actingId}
              expandedSvc={expandedSvc}
              onSelect={() => { onSelect(p.id); onProjectName?.(p.name); onProjectPath?.(p.path); }}
              onToggleExpand={e => toggleExpand(e, p.id)}
              onContextMenu={(e) => { e.preventDefault(); e.stopPropagation(); setCtxMenu({ id: p.id, name: p.name, path: p.path, x: e.clientX, y: e.clientY }); }}
              onStart={e => handleStart(e, p.id, p.name)}
              onStop={e => handleStop(e, p.id, p.name)}
              onTogglePin={e => handleTogglePin(e, p.id)}
              onToggleSvcExpand={serviceId => toggleSvcExpand(NO_DOM_EVENT, p.id, serviceId)}
            />
          </Fragment>
        ))}
      </div>

      {/* 右键菜单 */}
      {ctxMenu && (
        <ContextMenu x={ctxMenu.x} y={ctxMenu.y} onClose={() => setCtxMenu(null)}>
          {/* 操作项 */}
          <div className="py-1.5 px-1.5">
            <ContextMenuItem
              iconBox
              icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
                <path d="M1.5 3h2l1-1.5h4a1 1 0 011 1v5.5a1 1 0 01-1 1h-7a1 1 0 01-1-1V3z"/>
              </svg>}
              label="在资源管理器中打开"
              onClick={() => {
                // 失败原因由 openInExplorer 统一提示（白名单拒绝与路径不存在文案一致，都带原因）
                void openInExplorer(ctxMenu.path);
                setCtxMenu(null);
              }}
            />

            <ContextMenuItem
              iconBox
              icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
                <path d="M1.5 2.5l3.5 2.5-3.5 2.5"/><line x1="6.5" y1="8" x2="8.5" y2="8"/>
              </svg>}
              label="打开终端"
              onClick={() => {
                void openTerminal(ctxMenu.path);
                setCtxMenu(null);
              }}
            />

            <ContextMenuItem
              iconBox
              icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
                <rect x="2.5" y="3" width="5" height="5.5" rx=".8"/>
                <path d="M2 2.5v4.5h.5V3.5h4V2.5H3a.5.5 0 00-.5.5z"/>
              </svg>}
              label="复制项目"
              onClick={() => { setDuplicateTarget({ id: ctxMenu.id, name: ctxMenu.name }); setCtxMenu(null); }}
            />

            <ContextMenuItem
              iconBox
              icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
                <path d="M7 2l1 1-5.5 5.5H1.5V7.5L7 2z"/>
              </svg>}
              label="编辑项目"
              onClick={() => {
                const p = projects.find(pr => pr.id === ctxMenu.id);
                if (p) setEditTarget(p);
                setCtxMenu(null);
              }}
            />
          </div>

          {/* 分隔线和删除 */}
          <div className="border-t border-nexus-border/30 py-1.5 px-1.5">
            <ContextMenuItem
              iconBox
              tone="danger"
              icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
                <path d="M2.5 3h5M3.5 3V2a.5.5 0 01.5-.5h2a.5.5 0 01.5.5v1M4 4.5v3M6 4.5v3M3 3l.5 6a1 1 0 001 .5h3a1 1 0 001-.5L9 3"/>
              </svg>}
              label="删除项目"
              onClick={() => { setDeleteTarget({ id: ctxMenu.id, name: ctxMenu.name }); setCtxMenu(null); }}
            />
          </div>
        </ContextMenu>
      )}

      {/* Modals */}
      <CreateProjectModal
        open={showNewModal}
        onClose={() => setShowNewModal(false)}
        onCreated={(p) => { load(); onSelect(p.id); onProjectName?.(p.name); onProjectPath?.(p.path); }}
      />

      <EditProjectModal
        project={editTarget}
        onClose={() => setEditTarget(null)}
        onUpdated={load}
        onProjectName={onProjectName}
      />

      <DuplicateProjectModal
        target={duplicateTarget}
        onClose={() => setDuplicateTarget(null)}
        onDuplicated={load}
      />

      <DeleteProjectModal
        target={deleteTarget}
        onClose={() => setDeleteTarget(null)}
        onDeleted={() => {
          load();
          if (deleteTarget) useSvcCacheStore.getState().invalidate(deleteTarget.id);
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

function SearchBar({ search, setSearch, onNew, onCollapse }: {
  search: string;
  setSearch: (s: string) => void;
  onNew: () => void;
  /** 收起项目列（由 MainLayout 提供；缺省时不渲染该按钮） */
  onCollapse?: () => void;
}) {
  return (
    <div className="flex items-center gap-2 px-3 py-2 flex-shrink-0">
      {/* min-w-0：flex 子项默认 min-width:auto，input 的固有宽度会让面板缩窄时溢出 */}
      <div className="flex-1 min-w-0 relative">
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
      {onCollapse && (
        <button
          className="flex-shrink-0 p-1 text-nexus-muted hover:text-nexus-text rounded hover:bg-nexus-hover/50 transition-colors"
          title="收起项目列表"
          onClick={onCollapse}
        >
          <PanelToggleIcon />
        </button>
      )}
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
