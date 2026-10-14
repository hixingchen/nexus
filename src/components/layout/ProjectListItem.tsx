import { useMemo, useState } from 'react';
import { createPortal } from 'react-dom';
import { openInExplorer, openTerminal } from '../../services/system';
import { type Project, type Service } from '../../services/service';
import { FileTree } from '../file-tree/FileTree';
import { useSearchModalStore } from '../../stores/searchModal';
import { copyText } from '../../utils/clipboard';
import { focusProjectStep } from '../../utils/keyboard';
import { ContextMenu, ContextMenuItem } from '../ui/ContextMenu';

interface Props {
  project: Project;
  selected: boolean;
  isExpanded: boolean;
  /** 正在拉服务列表（箭头槽位显示 spinner） */
  expanding: boolean;
  services: Service[];
  isRunning: boolean;
  actingId: string | null;
  expandedSvc: Set<string>;
  onSelect: () => void;
  /**
   * 展开/收起服务目录树（鼠标：双击卡片；键盘：卡片上的 → / ←，UX-7）。
   *
   * 参数类型与 `useProjectList.toggleSvcExpand` 同一口径（只用到 `stopPropagation`）：
   * 键盘那条路没有真实鼠标事件，不这么写就得伪造一个假事件。
   */
  onToggleExpand: (e: { stopPropagation(): void }) => void;
  onContextMenu: (e: React.MouseEvent) => void;
  onStart: (e: React.MouseEvent) => void;
  onStop: (e: React.MouseEvent) => void;
  onTogglePin: (e: React.MouseEvent) => void;
  onToggleSvcExpand: (serviceId: string) => void;
}

/**
 * 键盘触发的展开/收起没有真实鼠标事件：`toggleExpand` 只用到 `stopPropagation`。
 * 与 `ProjectList` 里那个同名的 no-op 是同一手法（CQ-21：不伪造 `as React.MouseEvent`）。
 */
const NO_DOM_EVENT = { stopPropagation: () => {} };

/**
 * `Enter` 等价于单击（UX-7）：给那些"只绑了 onClick 的 div 行"用。
 *
 * 为什么需要：项目目录/服务目录这两行是打开对应文件树的**唯一入口**，而它们此前是纯鼠标的
 * ——卡片能展开项目了，键盘用户却在这一步断掉，树里那些刚做好的键盘操作一行也够不着。
 */
function activateOnEnter(run: () => void) {
  return (ev: React.KeyboardEvent) => {
    if (ev.key !== 'Enter') return;
    ev.preventDefault();
    run();
  };
}

export function ProjectListItem({
  project, selected, isExpanded, expanding, services, isRunning, actingId, expandedSvc,
  onSelect, onToggleExpand, onContextMenu, onStart, onStop, onTogglePin, onToggleSvcExpand,
}: Props) {
  // useMemo（PERF-38）：本组件没有 memo，而父级的每次 running / svcCache / 搜索输入变化
  // 都会重渲**全部项目行**；这里每渲一次就 filter 一遍服务列表，乘上项目数就是白跑
  const showTreeServices = useMemo(
    () => services.filter(s => s.show_file_tree && s.cwd),
    [services],
  );
  const openSearch = useSearchModalStore(s => s.openSearch);

  // 服务行右键菜单（搜索文件内容）
  const [svcMenu, setSvcMenu] = useState<{ x: number; y: number; svc: Service } | null>(null);
  /**
   * 项目目录标题行的右键菜单。
   *
   * 为什么需要：项目目录树是 `embedded` 模式，**不渲染根节点那一行**，空白区右键只有
   * 「粘贴到项目根目录」——于是"在整个项目里搜一段代码"这个最常用的场景，在这个入口里
   * 做不到，只能挨个儿右键子目录（等于逐目录搜）。标题行是这里**唯一代表"项目根"的可点
   * 对象**，搜索入口挂它身上最自然。服务行那边早有同名入口（范围 = 该服务的工作目录）。
   */
  const [projectMenu, setProjectMenu] = useState<{ x: number; y: number } | null>(null);
  /**
   * 项目自己的目录树是否展开。用本地 state 而不是 store：与服务的树展开态（`expandedSvc`）
   * 同一性质——只活本次会话，重开应用/收起左栏后回到收起态（默认收起才不打扰）。
   */
  const [projectTreeOpen, setProjectTreeOpen] = useState(false);

  /**
   * 卡片上的键盘操作（UX-7）。
   *
   * 此前卡片是纯 `div`：键盘既选不中项目，也展不开它的服务目录树（展开只绑在双击上），
   * 而"进项目的文件树"整个应用只有这一条路。键位：Enter 选中、→ / ← 展开/收起、↑↓ 换项目。
   */
  const handleCardKeyDown = (ev: React.KeyboardEvent<HTMLDivElement>) => {
    // 焦点在卡片里的启动/收藏按钮上时，按键归按钮（否则 Enter 会被卡片抢走）
    if (ev.target !== ev.currentTarget) return;
    switch (ev.key) {
      case 'Enter':
        ev.preventDefault();
        onSelect();
        break;
      case 'ArrowRight':
        if (!isExpanded) { ev.preventDefault(); onToggleExpand(NO_DOM_EVENT); }
        break;
      case 'ArrowLeft':
        if (isExpanded) { ev.preventDefault(); onToggleExpand(NO_DOM_EVENT); }
        break;
      case 'ArrowUp':
      case 'ArrowDown':
        // 已在首/末项时不拦默认行为（让页面自己滚动，而不是"按了没反应"）
        if (focusProjectStep(ev.currentTarget, ev.key === 'ArrowDown' ? 1 : -1)) ev.preventDefault();
        break;
      default:
        break;
    }
  };

  return (
    <>
    {/* data-project-id：ProjectList 用它把"选中项滚进视野"（见那边的 effect） */}
    <div className="mb-1.5" data-project-id={project.id}>
      <div
        className={`mx-2 rounded-md px-3 py-2.5 cursor-pointer group focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-nexus-accent ${
          selected
            ? 'bg-nexus-accent/10 border border-nexus-accent/30'
            : 'bg-nexus-bg/30 border border-nexus-border hover:bg-nexus-hover hover:border-nexus-muted'
        }`}
        // 可聚焦 + Enter 选中（UX-7）；title 里写明键盘等价操作——
        // "双击展开"此前没有任何可见提示，键盘用户更无从得知
        tabIndex={0}
        aria-current={selected ? 'true' : undefined}
        title="Enter 选中；→ 展开 / ← 收起服务目录树；↑↓ 切换项目；右键更多操作"
        onClick={onSelect}
        onDoubleClick={onToggleExpand}
        onKeyDown={handleCardKeyDown}
        onContextMenu={onContextMenu}
      >
        <div className="flex items-start gap-2">
          {/* 展开入口 = 双击卡片（见 onDoubleClick）。此处只保留"展开加载中"的反馈：
              不渲染箭头图标（左侧留白反而更干净），未加载时不占位；
              spinner 仍必须有——双击到数据回来之间若无反馈，用户会以为双击没生效而反复点 */}
          {expanding && (
            <span className="flex-shrink-0 -ml-1 w-3.5 h-[18px] flex items-center justify-center">
              <span className="w-[11px] h-[11px] border-[1.5px] border-nexus-success/30 border-t-nexus-success rounded-full animate-spin" />
            </span>
          )}
          {/* 文件夹图标 */}
          <span className="flex-shrink-0 mt-px text-nexus-muted/60 group-hover:text-nexus-muted">
            <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.2">
              <path d="M2 3.5a1 1 0 011-1h3l1.5 1.5H12a1 1 0 011 1v5a1 1 0 01-1 1H3a1 1 0 01-1-1V3.5z"/>
            </svg>
          </span>
          <div className="min-w-0 flex-1">
            <span className="text-[13px] text-nexus-text font-medium truncate block">{project.name}</span>
            {project.path && (
              <span className="text-[11px] text-nexus-muted truncate block mt-0.5 font-mono">
                {project.path}
              </span>
            )}
          </div>
          {/* 启动/停止按钮。
              两个状态共用一个**固定 22×22 的按钮盒**（图标居中，不再靠 p-1 + 图标自身撑开）：
              三角形 14px、spinner 13px，靠内边距撑的话启动中会差 1px（按钮跟着缩一下），
              而 ▶→■ 的切换又是同一瞬间发生的——"按下时按钮动了一下"就是这么来的。
              盒子定死之后，两个状态、三种图标都占同一块地方（同 CodeViewer 的图标按钮）。

              运行态给底色、图标用满色（与 ServiceTreeEntry 的「■ 停止」同一套语义色）：
              此前运行态是 70% 透明度的**裸图标**、没有任何按钮底色，压在卡片上就是一个暗红方块
              ——看着像按下去没弹起来的坑，而不是"点一下能停"的控件。 */}
          {isRunning ? (
            <button
              className="flex-shrink-0 w-[22px] h-[22px] flex items-center justify-center rounded bg-nexus-error/15 text-nexus-error hover:bg-nexus-error/25 active:bg-nexus-error/30 disabled:opacity-30 transition-colors"
              disabled={actingId === project.id}
              onClick={onStop}
              title="停止"
            >
              <svg width="14" height="14" viewBox="0 0 12 12" fill="currentColor"><rect x="2" y="2" width="8" height="8" rx="1"/></svg>
            </button>
          ) : (
            <button
              /* 不带 disabled:opacity-30：启动中要显示 spinner，压暗会让它看不清 */
              className="flex-shrink-0 w-[22px] h-[22px] flex items-center justify-center rounded text-nexus-success/70 hover:text-nexus-success hover:bg-nexus-success/10 active:bg-nexus-success/20 transition-colors"
              disabled={actingId === project.id}
              onClick={onStart}
              title="启动"
            >
              {actingId === project.id ? (
                /* 启动中：真转的 spinner（原来是个静态圆环，看着像"停止"） */
                <span className="w-[13px] h-[13px] border-[1.5px] border-nexus-success/30 border-t-nexus-success rounded-full animate-spin" />
              ) : (
                <svg width="14" height="14" viewBox="0 0 12 12" fill="currentColor"><polygon points="3,1.5 10.5,6 3,10.5"/></svg>
              )}
            </button>
          )}

          {/* 收藏按钮（字段名仍是 pinned，界面上统一叫收藏） */}
          <button
            className={`flex-shrink-0 p-1 rounded transition-all ${
              project.pinned
                ? 'text-nexus-accent bg-nexus-accent/10'
                : 'text-nexus-muted/50 hover:text-nexus-accent hover:bg-nexus-accent/10'
            }`}
            onClick={onTogglePin}
            title={project.pinned ? '取消收藏' : '收藏'}
          >
            <svg width="14" height="14" viewBox="0 0 12 12" fill={project.pinned ? 'currentColor' : 'none'} stroke="currentColor" strokeWidth="1.5" strokeLinejoin="round">
              <path d="M7.5 1.5L10.5 4.5 8 7l1.5 4-7-7L7 2.5l.5-1z"/>
            </svg>
          </button>
        </div>
      </div>
      {/* 展开：项目目录树（恒定）+ 各服务的目录树（可选，取决于服务的「在目录树中显示」） */}
      {isExpanded && (
        <div className="border-t border-nexus-border/50 mt-1 mx-2 bg-nexus-bg/20 rounded-b-md">
          {/* 项目自己的目录树。
              为什么要有：项目根下的文件（README、package.json、docker-compose、docs/…）不属于
              任何服务的工作目录，此前只能靠"建一个指到根目录的服务"才看得到。
              放在服务**之前**：树形结构的通行直觉是"根在上、子在下"（文件树本身也是目录在前），
              而且它是**恒定存在**的那一项（服务树可有可无：取决于哪些服务开了"在目录树中显示"）——
              不变的东西放在固定位置，展开项目时它永远在第一行。**默认收起**，不展开只占一行。
              定位优先级低于服务树（见 FileTree 的 kind / utils/fileTree.ts）。 */}
          <div>
            <div
              className={`flex items-center gap-2 px-3 py-1.5 cursor-pointer transition-colors focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-nexus-accent ${
                projectTreeOpen
                  ? 'text-nexus-text bg-nexus-bg/40'
                  : 'text-nexus-text-muted hover:bg-nexus-hover/30 hover:text-nexus-text'
              }`}
              // 这一行是走到项目文件树的**唯一入口**（树在它下面），此前只有鼠标能开（UX-7）
              tabIndex={0}
              aria-expanded={projectTreeOpen}
              onClick={() => setProjectTreeOpen(v => !v)}
              onKeyDown={activateOnEnter(() => setProjectTreeOpen(v => !v))}
              onContextMenu={(e) => {
                e.preventDefault();
                e.stopPropagation();
                setProjectMenu({ x: e.clientX, y: e.clientY });
              }}
              title="项目根目录（不属于任何服务的文件在这里）· 右键可搜索整个项目 · Enter 展开"
            >
              <svg
                className={`flex-shrink-0 text-nexus-muted/60 transition-transform ${projectTreeOpen ? 'rotate-90' : ''}`}
                width="10" height="10" viewBox="0 0 10 10" fill="none"
                stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round"
              >
                <polyline points="3,1 7,5 3,9" />
              </svg>
              <span className="text-[13px] truncate">项目目录</span>
            </div>
            {projectTreeOpen && (
              <div className="ml-[11px] pl-2 border-l border-nexus-border/30">
                <FileTree rootPath={project.path} embedded kind="project" />
              </div>
            )}
          </div>

          {showTreeServices.length === 0 ? (
            // 不再是"空状态"（上面固定有项目目录节点），所以改成一条可操作的提示：
            // 告诉用户服务树是**可以开的**，而不是让人以为这里出问题了
            <div className="px-3 pt-2 pb-0.5 text-[11px] text-nexus-muted/50">
              暂无服务目录树（可在服务编辑面板开启）
            </div>
          ) : (
            showTreeServices.map(s => {
              const svcKey = `${project.id}:${s.id}`;
              const svcExpanded = expandedSvc.has(svcKey);
              return (
                <div key={s.id}>
                  <div
                    className={`flex items-center gap-2 px-3 py-1.5 cursor-pointer transition-colors focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-nexus-accent ${
                      svcExpanded
                        ? 'text-nexus-text bg-nexus-bg/40'
                        : 'text-nexus-text-muted hover:bg-nexus-hover/30 hover:text-nexus-text'
                    }`}
                    // 同「项目目录」那一行：它是这棵服务文件树的唯一入口（UX-7）
                    tabIndex={0}
                    aria-expanded={svcExpanded}
                    onClick={() => onToggleSvcExpand(s.id)}
                    onKeyDown={activateOnEnter(() => onToggleSvcExpand(s.id))}
                    onContextMenu={(e) => {
                      e.preventDefault();
                      e.stopPropagation();
                      setSvcMenu({ x: e.clientX, y: e.clientY, svc: s });
                    }}
                    title="右键可搜索文件内容 · Enter 展开"
                  >
                    <svg
                      className={`flex-shrink-0 text-nexus-muted/60 transition-transform ${svcExpanded ? 'rotate-90' : ''}`}
                      width="10" height="10" viewBox="0 0 10 10" fill="none"
                      stroke="currentColor" strokeWidth="1.3" strokeLinecap="round"
                    >
                      <polyline points="3,1 7,5 3,9" />
                    </svg>
                    <span className="text-[13px] truncate">{s.name}</span>
                  </div>
                  {svcExpanded && (
                    <div className="ml-[11px] pl-2 border-l border-nexus-border/30">
                      <FileTree rootPath={s.cwd} embedded />
                    </div>
                  )}
                </div>
              );
            })
          )}
        </div>
      )}
    </div>

    {/* 项目目录标题行右键菜单：范围 = 整个项目根。与服务行（范围 = 该服务的工作目录）对称 */}
    {projectMenu && createPortal(
      <ContextMenu x={projectMenu.x} y={projectMenu.y} onClose={() => setProjectMenu(null)}>
        {/* 搜索文件内容 */}
        <div className="py-1.5 px-1.5">
          <ContextMenuItem
            icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
              <circle cx="4.2" cy="4.2" r="3"/><line x1="6.5" y1="6.5" x2="8.8" y2="8.8"/>
            </svg>}
            label="搜索文件内容"
            onClick={() => {
              // 标题用项目名（服务行那边用服务名），范围是项目根
              openSearch(project.path, project.name);
              setProjectMenu(null);
            }}
          />
        </div>

        {/* 在资源管理器中打开 / 打开终端（项目根是常用的定位目标） */}
        <div className="border-t border-nexus-border/30 py-1.5 px-1.5">
          <ContextMenuItem
            icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
              <path d="M1.5 3h2l1-1.5h4a1 1 0 011 1v5.5a1 1 0 01-1 1h-7a1 1 0 01-1-1V3z"/>
            </svg>}
            label="在资源管理器中打开"
            onClick={() => {
              void openInExplorer(project.path);
              setProjectMenu(null);
            }}
          />
          <ContextMenuItem
            icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
              <path d="M1.5 2.5l3.5 2.5-3.5 2.5"/><line x1="6.5" y1="8" x2="8.5" y2="8"/>
            </svg>}
            label="打开终端"
            onClick={() => {
              void openTerminal(project.path);
              setProjectMenu(null);
            }}
          />
        </div>
      </ContextMenu>,
      document.body,
    )}

    {/* 服务行右键菜单：搜索文件内容 / 资源管理器 / 终端 / 复制 */}
    {svcMenu && createPortal(
      <ContextMenu x={svcMenu.x} y={svcMenu.y} onClose={() => setSvcMenu(null)}>
        {/* 搜索文件内容 */}
        <div className="py-1.5 px-1.5">
          <ContextMenuItem
            icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
              <circle cx="4.2" cy="4.2" r="3"/><line x1="6.5" y1="6.5" x2="8.8" y2="8.8"/>
            </svg>}
            label="搜索文件内容"
            onClick={() => {
              openSearch(svcMenu.svc.cwd, svcMenu.svc.name);
              setSvcMenu(null);
            }}
          />
        </div>

        {/* 在资源管理器中打开 / 打开终端 / 复制路径 / 复制文件名
            （「打开终端」与「项目目录」那一行逐字一致：两者都代表一棵树的根，
            工作目录就是那个根，是终端里 cd 过去的第一目标） */}
        <div className="border-t border-nexus-border/30 py-1.5 px-1.5">
          <ContextMenuItem
            icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
              <path d="M1.5 3h2l1-1.5h4a1 1 0 011 1v5.5a1 1 0 01-1 1h-7a1 1 0 01-1-1V3z"/>
            </svg>}
            label="在资源管理器中打开"
            onClick={() => {
              const { cwd } = svcMenu.svc;
              setSvcMenu(null);
              void openInExplorer(cwd);
            }}
          />
          <ContextMenuItem
            icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
              <path d="M1.5 2.5l3.5 2.5-3.5 2.5"/><line x1="6.5" y1="8" x2="8.5" y2="8"/>
            </svg>}
            label="打开终端"
            onClick={() => {
              const { cwd } = svcMenu.svc;
              setSvcMenu(null);
              void openTerminal(cwd);
            }}
          />
          <ContextMenuItem
            icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
              <rect x="3" y="3" width="5" height="5.5" rx=".8"/>
              <path d="M2 2.5v4.5h.5V3.5h4V2.5H3a.5.5 0 00-.5.5z"/>
            </svg>}
            label="复制路径"
            onClick={async () => {
              const { cwd } = svcMenu.svc;
              setSvcMenu(null);
              await copyText(cwd, '路径');
            }}
          />
          <ContextMenuItem
            icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
              <rect x="1" y="2" width="8" height="6" rx="1"/>
              <path d="M3 4h4M3 6h2"/>
            </svg>}
            label="复制文件名"
            onClick={async () => {
              const { name } = svcMenu.svc;
              setSvcMenu(null);
              await copyText(name, '文件名');
            }}
          />
        </div>
      </ContextMenu>,
      document.body,
    )}
    </>
  );
}
