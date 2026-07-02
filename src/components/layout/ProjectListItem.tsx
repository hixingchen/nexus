import { type Project, type Service } from '../../services/service';
import { FileTree } from '../file-tree/FileTree';

interface Props {
  project: Project;
  selected: boolean;
  isExpanded: boolean;
  services: Service[];
  isRunning: boolean;
  actingId: string | null;
  expandedSvc: Set<string>;
  onSelect: () => void;
  onDoubleClick: (e: React.MouseEvent) => void;
  onContextMenu: (e: React.MouseEvent) => void;
  onStart: (e: React.MouseEvent) => void;
  onStop: (e: React.MouseEvent) => void;
  onTogglePin: (e: React.MouseEvent) => void;
  onToggleSvcExpand: (serviceId: string) => void;
}

export function ProjectListItem({
  project, selected, isExpanded, services, isRunning, actingId, expandedSvc,
  onSelect, onDoubleClick, onContextMenu, onStart, onStop, onTogglePin, onToggleSvcExpand,
}: Props) {
  const showTreeServices = services.filter(s => s.show_file_tree && s.cwd);

  return (
    <div className="mb-1.5">
      <div
        className={`mx-2 rounded-md px-3 py-2.5 cursor-pointer group ${
          selected
            ? 'bg-nexus-accent/10 border border-nexus-accent/30'
            : 'bg-nexus-bg/30 border border-nexus-border hover:bg-nexus-hover hover:border-nexus-muted'
        }`}
        onClick={onSelect}
        onDoubleClick={onDoubleClick}
        onContextMenu={onContextMenu}
      >
        <div className="flex items-start gap-2">
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
              className="flex-shrink-0 p-1 rounded text-nexus-success/70 hover:text-nexus-success hover:bg-nexus-success/10 disabled:opacity-30"
              disabled={actingId === project.id}
              onClick={onStart}
              title="启动"
            >
              {actingId === project.id ? (
                <svg width="14" height="14" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.5"><circle cx="6" cy="6" r="4"/></svg>
              ) : (
                <svg width="14" height="14" viewBox="0 0 12 12" fill="currentColor"><polygon points="3,1.5 10.5,6 3,10.5"/></svg>
              )}
            </button>
          )}

          {/* 置顶按钮 */}
          <button
            className={`flex-shrink-0 p-1 rounded transition-all ${
              project.pinned
                ? 'text-nexus-accent bg-nexus-accent/10'
                : 'text-nexus-muted/50 hover:text-nexus-accent hover:bg-nexus-accent/10'
            }`}
            onClick={onTogglePin}
            title={project.pinned ? '取消置顶' : '置顶'}
          >
            <svg width="14" height="14" viewBox="0 0 12 12" fill={project.pinned ? 'currentColor' : 'none'} stroke="currentColor" strokeWidth="1.5" strokeLinejoin="round">
              <path d="M7.5 1.5L10.5 4.5 8 7l1.5 4-7-7L7 2.5l.5-1z"/>
            </svg>
          </button>
        </div>
      </div>
      {/* 展开：服务列表 + 可选展开的目录树 */}
      {isExpanded && (
        <div className="border-t border-nexus-border/50 mt-1 mx-2 bg-nexus-bg/20 rounded-b-md">
          {showTreeServices.length === 0 ? (
            <div className="py-3 px-3 text-[11px] text-nexus-muted/50 text-center">
              暂无开启目录树的服务
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
  );
}
