import { useState } from 'react';
import { createPortal } from 'react-dom';
import { openInExplorer, openTerminal } from '../../services/system';
import { type Project, type Service } from '../../services/service';
import { FileTree } from '../file-tree/FileTree';
import { useSearchModalStore } from '../../stores/searchModal';
import { copyText } from '../../utils/clipboard';
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
  /** 展开/收起服务目录树（双击卡片；卡片上不再有箭头按钮） */
  onToggleExpand: (e: React.MouseEvent) => void;
  onContextMenu: (e: React.MouseEvent) => void;
  onStart: (e: React.MouseEvent) => void;
  onStop: (e: React.MouseEvent) => void;
  onTogglePin: (e: React.MouseEvent) => void;
  onToggleSvcExpand: (serviceId: string) => void;
}

export function ProjectListItem({
  project, selected, isExpanded, expanding, services, isRunning, actingId, expandedSvc,
  onSelect, onToggleExpand, onContextMenu, onStart, onStop, onTogglePin, onToggleSvcExpand,
}: Props) {
  const showTreeServices = services.filter(s => s.show_file_tree && s.cwd);
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

  return (
    <>
    {/* data-project-id：ProjectList 用它把"选中项滚进视野"（见那边的 effect） */}
    <div className="mb-1.5" data-project-id={project.id}>
      <div
        className={`mx-2 rounded-md px-3 py-2.5 cursor-pointer group ${
          selected
            ? 'bg-nexus-accent/10 border border-nexus-accent/30'
            : 'bg-nexus-bg/30 border border-nexus-border hover:bg-nexus-hover hover:border-nexus-muted'
        }`}
        onClick={onSelect}
        onDoubleClick={onToggleExpand}
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
          {/* 启动/停止按钮 */}
          {isRunning ? (
            <button
              className="flex-shrink-0 p-1 rounded text-nexus-error/70 hover:text-nexus-error hover:bg-nexus-error/10 disabled:opacity-30"
              disabled={actingId === project.id}
              onClick={onStop}
              title="停止"
            >
              <svg width="14" height="14" viewBox="0 0 12 12" fill="currentColor"><rect x="2" y="2" width="8" height="8" rx="1"/></svg>
            </button>
          ) : (
            <button
              /* 不带 disabled:opacity-30：启动中要显示 spinner，压暗会让它看不清 */
              className="flex-shrink-0 p-1 rounded text-nexus-success/70 hover:text-nexus-success hover:bg-nexus-success/10"
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
              className={`flex items-center gap-2 px-3 py-1.5 cursor-pointer transition-colors ${
                projectTreeOpen
                  ? 'text-nexus-text bg-nexus-bg/40'
                  : 'text-nexus-text-muted hover:bg-nexus-hover/30 hover:text-nexus-text'
              }`}
              onClick={() => setProjectTreeOpen(v => !v)}
              onContextMenu={(e) => {
                e.preventDefault();
                e.stopPropagation();
                setProjectMenu({ x: e.clientX, y: e.clientY });
              }}
              title="项目根目录（不属于任何服务的文件在这里）· 右键可搜索整个项目"
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
                    className={`flex items-center gap-2 px-3 py-1.5 cursor-pointer transition-colors ${
                      svcExpanded
                        ? 'text-nexus-text bg-nexus-bg/40'
                        : 'text-nexus-text-muted hover:bg-nexus-hover/30 hover:text-nexus-text'
                    }`}
                    onClick={() => onToggleSvcExpand(s.id)}
                    onContextMenu={(e) => {
                      e.preventDefault();
                      e.stopPropagation();
                      setSvcMenu({ x: e.clientX, y: e.clientY, svc: s });
                    }}
                    title="右键可搜索文件内容"
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

    {/* 服务行右键菜单：搜索文件内容 / 资源管理器 / 复制 */}
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

        {/* 在资源管理器中打开 / 复制路径 / 复制文件名 */}
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
