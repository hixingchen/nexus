import { Fragment, useCallback, useEffect, useState } from 'react';
import { projectApi, type Project } from '../../services/service';
import { useRunningStore } from '../../stores/runningStore';
import { useProjectActions } from '../../hooks/useProjectActions';
import { openInExplorer, openTerminal } from '../../services/system';
import { ProjectContextMenu } from './ProjectContextMenu';
import { PanelToggleIcon } from '../ui/PanelToggleIcon';
import { reportError } from '../../utils/error';
import { needsFavoriteDivider } from '../../utils/projectList';

/**
 * 项目列收起后的窄轨（32px）。
 *
 * 与服务列收起态同构：顶部是展开开关，下方是**可点击的项目条目**——
 * 收起不等于"什么都不能做"，仍然要能一眼看到有哪些项目、哪个在跑、并直接切换。
 * 因此这里不复用 ProjectList（收起时它已卸载，且它的搜索框/新建按钮在 32px 里放不下），
 * 而是自己拉一份项目列表。
 *
 * 右键方块出菜单（启动/停止、收藏、打开位置）：**菜单本体与动作都与完整列表共用**
 * （`ProjectContextMenu` + `useProjectActions`）。这一条不是代码洁癖——两个视图功能不一致时，
 * 症状只会出现在"某个入口忘了做某件事"（如窄轨上启动没开文件监听），且只在收起态复现。
 *
 * 用「首字符方块 + 运行小圆点」而不是服务列那种纯圆点：服务列表属于单个项目、条目少且
 * 有顺序可依；项目列表可能很长，纯圆点无法区分是哪个项目，首字符至少可辨认。
 */
export function ProjectRail({ selectedId, onSelect, onExpand, onExpandProject }: {
  selectedId: string | null;
  onSelect: (project: Project) => void;
  onExpand: () => void;
  /** 双击项目：展开项目列表**并**展开这个项目（与列表里双击项目的语义一致） */
  onExpandProject: (project: Project) => void;
}) {
  const [projects, setProjects] = useState<Project[]>([]);
  // 运行状态来自全局 store（MainLayout 已在轮询），无需自己拉
  const running = useRunningStore(s => s.running);
  const [menu, setMenu] = useState<{ id: string; x: number; y: number } | null>(null);

  /**
   * 拉取由 reloadKey 驱动而不是只跑一次：收藏会改变排序（后端 `ORDER BY pinned DESC`），
   * 在右键菜单里点完收藏必须重新拉，否则窄轨的顺序和那条收藏分隔线停在上一次的样子——
   * "点了没反应"的静默失败。
   */
  const [reloadKey, setReloadKey] = useState(0);
  const reload = useCallback(() => setReloadKey(k => k + 1), []);
  // 动作与完整列表共用一份实现（见 useProjectActions 的说明）：窄轨上漏开项目级文件监听、
  // 停止时漏清日志这类**只在收起态复现**的错，靠共用而不是靠"记得两处都改"来避免
  const { actingId, start, stop, togglePin } = useProjectActions(reload);

  useEffect(() => {
    let alive = true;
    projectApi.getAll()
      .then(list => { if (alive) setProjects(list); })
      .catch(e => reportError('加载项目列表失败', e));
    return () => { alive = false; };
  }, [reloadKey]);

  const isRunning = (id: string) => running.some(r => r.project_id === id);

  // 菜单的状态现查、不快照：菜单可能开着好几秒，期间列表会重新加载（比如刚刚收藏完）
  const menuProject = menu ? projects.find(p => p.id === menu.id) : undefined;

  return (
    <div className="flex flex-col h-full w-full bg-nexus-surface border-r border-nexus-border">
      <div className="flex flex-col items-center py-1 flex-shrink-0 border-b border-nexus-border/40">
        <button
          className="w-6 h-6 flex items-center justify-center rounded-md text-nexus-muted hover:text-nexus-text hover:bg-nexus-hover/40 transition-colors"
          title="展开项目列表"
          onClick={onExpand}
        >
          <PanelToggleIcon size={13} />
        </button>
      </div>

      <div className="flex-1 flex flex-col items-center gap-1 overflow-y-auto py-2">
        {projects.map((p, i) => {
          const active = isRunning(p.id);
          const selected = selectedId === p.id;
          return (
            // 收藏与其余之间一条横线：窄轨里收藏本来就排在前（DB 按 pinned DESC 排序），
            // 但没有边界就看不出"从哪一格开始不是收藏"——完整列表那边有「收藏夹」标题，
            // 两个视图对同一个概念的表达要一致（边界条件见 needsFavoriteDivider）
            <Fragment key={p.id}>
              {needsFavoriteDivider(projects, i) && (
                <div className="w-4 h-px bg-nexus-border flex-shrink-0" />
              )}
              <button
                // 与列表卡片同一个属性名（那边在包裹层上）：两处都靠它标识"这一格是哪个项目"
                data-project-id={p.id}
                onClick={() => onSelect(p)}
                // 双击 = 展开面板 + 展开该项目：与列表里双击项目（展开/收起）同一心智，
                // 只是窄轨上看不见展开状态，所以这里只做"展开"、不做"再双击收起"
                onDoubleClick={() => onExpandProject(p)}
                // 右键菜单与完整列表同一份（ProjectContextMenu 的前四项）：窄轨上够不着卡片按钮，
                // 收起状态下"启动一下 / 收藏一下"必须先展开面板才能做，是这里最反直觉的一件事
                onContextMenu={e => {
                  e.preventDefault();
                  e.stopPropagation();
                  setMenu({ id: p.id, x: e.clientX, y: e.clientY });
                }}
                className={`relative w-6 h-6 rounded-md flex items-center justify-center flex-shrink-0 text-[11px] font-medium transition-colors ${
                  selected
                    ? 'bg-nexus-accent/15 text-nexus-accent'
                    : 'text-nexus-muted/80 hover:bg-nexus-hover/50 hover:text-nexus-text'
                }`}
                title={`${p.name}${p.path ? ` · ${p.path}` : ''}${active ? '（运行中）' : ''}
双击：展开项目列表并展开该项目
右键：启动/停止、收藏、打开位置`}
              >
                {Array.from(p.name.trim())[0]?.toUpperCase() ?? '?'}
                {/* 运行指示：右下角小圆点（同服务列的状态点语言） */}
                {active && (
                  <span className="absolute right-0 bottom-0 w-[6px] h-[6px] rounded-full bg-nexus-success ring-2 ring-nexus-surface" />
                )}
              </button>
            </Fragment>
          );
        })}
      </div>

      {/* 与完整列表共用的菜单，只是不传 复制/编辑/删除（窄轨没有这三个入口，也不该为此挂上弹窗） */}
      {menu && menuProject && (
        <ProjectContextMenu
          x={menu.x}
          y={menu.y}
          pinned={menuProject.pinned}
          running={isRunning(menuProject.id)}
          busy={actingId === menuProject.id}
          onClose={() => setMenu(null)}
          onStart={() => void start(menuProject.id, menuProject.name)}
          onStop={() => void stop(menuProject.id, menuProject.name)}
          onTogglePin={() => void togglePin(menuProject.id)}
          onOpenExplorer={() => void openInExplorer(menuProject.path)}
          onOpenTerminal={() => void openTerminal(menuProject.path)}
        />
      )}
    </div>
  );
}
