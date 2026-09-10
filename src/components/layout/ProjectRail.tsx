import { useEffect, useState } from 'react';
import { projectApi, type Project } from '../../services/service';
import { useRunningStore } from '../../stores/runningStore';
import { PanelToggleIcon } from '../ui/PanelToggleIcon';

/**
 * 项目列收起后的窄轨（32px）。
 *
 * 与服务列收起态同构：顶部是展开开关，下方是**可点击的项目条目**——
 * 收起不等于"什么都不能做"，仍然要能一眼看到有哪些项目、哪个在跑、并直接切换。
 * 因此这里不复用 ProjectList（收起时它已卸载，且它的搜索框/新建按钮在 32px 里放不下），
 * 而是自己拉一份只含 id/name 的最小列表。
 *
 * 用「首字符方块 + 运行小圆点」而不是服务列那种纯圆点：服务列表属于单个项目、条目少且
 * 有顺序可依；项目列表可能很长，纯圆点无法区分是哪个项目，首字符至少可辨认。
 */
export function ProjectRail({ selectedId, onSelect, onExpand }: {
  selectedId: string | null;
  onSelect: (project: Project) => void;
  onExpand: () => void;
}) {
  const [projects, setProjects] = useState<Project[]>([]);
  // 运行状态来自全局 store（MainLayout 已在轮询），无需自己拉
  const running = useRunningStore(s => s.running);

  useEffect(() => {
    let alive = true;
    projectApi.getAll()
      .then(list => { if (alive) setProjects(list); })
      .catch(e => console.error('收起态加载项目列表失败:', e));
    return () => { alive = false; };
  }, []);

  const isRunning = (id: string) => running.some(r => r.project_id === id);

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
        {projects.map(p => {
          const active = isRunning(p.id);
          const selected = selectedId === p.id;
          return (
            <button
              key={p.id}
              onClick={() => onSelect(p)}
              className={`relative w-6 h-6 rounded-md flex items-center justify-center flex-shrink-0 text-[11px] font-medium transition-colors ${
                selected
                  ? 'bg-nexus-accent/15 text-nexus-accent'
                  : 'text-nexus-muted/80 hover:bg-nexus-hover/50 hover:text-nexus-text'
              }`}
              title={`${p.name}${p.path ? ` · ${p.path}` : ''}${active ? '（运行中）' : ''}`}
            >
              {Array.from(p.name.trim())[0]?.toUpperCase() ?? '?'}
              {/* 运行指示：右下角小圆点（同服务列的状态点语言） */}
              {active && (
                <span className="absolute right-0 bottom-0 w-[6px] h-[6px] rounded-full bg-nexus-success ring-2 ring-nexus-surface" />
              )}
            </button>
          );
        })}
      </div>
    </div>
  );
}
