import { Fragment, useEffect, useRef } from 'react';
import { ProjectListItem } from './ProjectListItem';
import { CreateProjectModal, EditProjectModal, DeleteProjectModal, DuplicateProjectModal } from './ProjectModals';
import { useProjectList } from '../../hooks/useProjectList';
import { useSvcCacheStore } from '../../stores/svcCacheStore';
import { openInExplorer, openTerminal } from '../../services/system';
import { PanelToggleIcon } from '../ui/PanelToggleIcon';
import { ProjectContextMenu } from './ProjectContextMenu';
import { needsFavoriteDivider } from '../../utils/projectList';

interface Props {
  selectedId: string | null;
  onSelect: (id: string | null) => void;
  onProjectName?: (name: string) => void;
  onProjectPath?: (path: string) => void;
  /** 收起项目列（由 MainLayout 提供） */
  onCollapse?: () => void;
  /**
   * 「展开这个项目」的请求（由窄轨双击发出）。列表挂载后自行执行一次并回调 `onExpandHandled`
   * 清除请求——用"请求 + 回执"而不是直接调用：窄轨与列表**不会同时挂载**，
   * 窄轨手上没有 `useProjectList` 的行动作可调。
   */
  expandProjectId?: string | null;
  onExpandHandled?: () => void;
}

/**
 * 展开/收起服务分组时需要的"事件"其实是空的：子组件（`ProjectListItem`）只回传
 * serviceId，没有真实 DOM 事件。这里给一个名实相符的 no-op，而不是伪造一个
 * `as React.MouseEvent` 的假事件（CQ-21，见 `useProjectList.toggleSvcExpand` 的说明）。
 */
const NO_DOM_EVENT = { stopPropagation: () => {} };

export function ProjectList({ selectedId, onSelect, onProjectName, onProjectPath, onCollapse, expandProjectId, onExpandHandled }: Props) {
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
    start, stop, togglePin,
    toggleSvcExpand, toggleExpand, expandProject,
  } = useProjectList();

  // 窄轨双击 → 面板展开 → 这里把那个项目也展开（"我就是要看到它"）。
  // 先回执再执行：回执清除父组件的请求，避免下一次渲染重复触发
  useEffect(() => {
    if (!expandProjectId) return;
    onExpandHandled?.();
    void expandProject(expandProjectId);
  }, [expandProjectId, expandProject, onExpandHandled]);

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

  /**
   * 右键菜单里那两个"按状态改文案"的条目（启动/停止、收藏/取消收藏）需要目标项目的当前状态。
   *
   * 两者都是**现查**而不是右键时快照进 `ctxMenu`：菜单可能开着好几秒，期间列表会因别处
   * 的操作重新 `load()`（如新开一个项目、详情页改了名字），快照会指向过期的值——
   * 表现是"收藏后菜单仍显示收藏"、"启动了但菜单还写着启动"。运行状态本就在共享 store 里
   * 实时变（3 秒轮询），更没有快照的道理。
   */
  const ctxProject = ctxMenu ? projects.find(p => p.id === ctxMenu.id) : undefined;
  const ctxRunning = ctxMenu ? isProjectRunning(ctxMenu.id) : false;

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
              // 卡片按钮有真实 DOM 事件：不拦住冒泡就会顺带触发卡片的 onClick（选中项目）
              onStart={e => { e.stopPropagation(); void start(p.id, p.name); }}
              onStop={e => { e.stopPropagation(); void stop(p.id, p.name); }}
              onTogglePin={e => { e.stopPropagation(); void togglePin(p.id); }}
              onToggleSvcExpand={serviceId => toggleSvcExpand(NO_DOM_EVENT, p.id, serviceId)}
            />
          </Fragment>
        ))}
      </div>

      {/* 右键菜单：菜单本体与窄轨共用（ProjectContextMenu），这里只接行动作与三个弹窗 */}
      {ctxMenu && (
        <ProjectContextMenu
          x={ctxMenu.x}
          y={ctxMenu.y}
          pinned={ctxProject?.pinned ?? false}
          running={ctxRunning}
          busy={actingId === ctxMenu.id}
          onClose={() => setCtxMenu(null)}
          onStart={() => void start(ctxMenu.id, ctxMenu.name)}
          onStop={() => void stop(ctxMenu.id, ctxMenu.name)}
          onTogglePin={() => void togglePin(ctxMenu.id)}
          // 失败原因由 openInExplorer 统一提示（白名单拒绝与路径不存在文案一致，都带原因）
          onOpenExplorer={() => void openInExplorer(ctxMenu.path)}
          onOpenTerminal={() => void openTerminal(ctxMenu.path)}
          onDuplicate={() => setDuplicateTarget({ id: ctxMenu.id, name: ctxMenu.name })}
          // 复用上面现查到的 ctxProject，而不是再 find 一次（同一个查找两处写，迟早分叉）
          onEdit={() => { if (ctxProject) setEditTarget(ctxProject); }}
          onDelete={() => setDeleteTarget({ id: ctxMenu.id, name: ctxMenu.name })}
        />
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
