import { useEffect, useState, useCallback } from 'react';
import { Terminal } from './Terminal';
import { ConfirmDialog } from '../ui/ConfirmDialog';
import {
  useTerminalStore,
  createProjectTab,
  closeProjectTab,
} from '../../stores/terminal';
import { processApi, projectApi } from '../../services/service';

interface Props {
  projectId: string | null;
  projectName: string | null;
  projectPath: string | null;
  visible?: boolean;
  terminalInitCommand?: string;
}

type RunningMap = Record<string, number>;

interface CloseConfirm {
  projectId: string;
  projectName: string;
  runningCount: number;
}

export function DevConsole({ projectId, projectName, projectPath, visible, terminalInitCommand }: Props) {
  const { sessions, activeProjectId } = useTerminalStore();
  const [runningMap, setRunningMap] = useState<RunningMap>({});
  const [closeConfirm, setCloseConfirm] = useState<CloseConfirm | null>(null);
  const [closing, setClosing] = useState(false);

  const setActiveProjectId = useTerminalStore((s) => s.setActiveProjectId);

  // ── 轮询 ──────────────────────────────────────────────────

  useEffect(() => {
    const poll = async () => {
      try {
        const running = await processApi.getRunning();
        const map: RunningMap = {};
        for (const key of running) {
          const pid = key.split(':')[0];
          if (pid) map[pid] = (map[pid] ?? 0) + 1;
        }
        setRunningMap(map);
      } catch { /* ignore */ }
    };
    poll();
    const id = setInterval(poll, 2000);
    return () => clearInterval(id);
  }, []);

  // ── 自动创建终端 Tab ──────────────────────────────────────

  useEffect(() => {
    for (const [pid, count] of Object.entries(runningMap)) {
      if (count > 0 && !useTerminalStore.getState().hasSession(pid)) {
        // 当前选中项目直接用 props
        if (pid === projectId && projectName && projectPath) {
          createProjectTab(pid, projectName, projectPath, terminalInitCommand);
        } else {
          // 非当前选中项目，从后端获取项目信息
          projectApi.getDetail(pid).then(
            (detail) => createProjectTab(pid, detail.project.name, detail.project.path, detail.project.terminal_init_command),
            () => { /* 项目可能已删除 */ }
          );
        }
      }
    }
  }, [runningMap, projectId, projectName, projectPath, terminalInitCommand]);

  // ── 同步活动 Tab ──────────────────────────────────────────

  useEffect(() => {
    if (projectId && useTerminalStore.getState().hasSession(projectId)) {
      setActiveProjectId(projectId);
    }
  }, [projectId, setActiveProjectId]);

  // ── 关闭 Tab ──────────────────────────────────────────────

  const requestClose = (tabProjectId: string) => {
    const session = sessions.find((s) => s.projectId === tabProjectId);
    if (!session) return;
    const runningCount = runningMap[tabProjectId] ?? 0;
    if (runningCount === 0) {
      closeProjectTab(tabProjectId);
      return;
    }
    setCloseConfirm({ projectId: tabProjectId, projectName: session.projectName, runningCount });
  };

  const handleConfirmClose = useCallback(async () => {
    if (!closeConfirm) return;
    setClosing(true);
    closeProjectTab(closeConfirm.projectId);
    try {
      await Promise.race([
        processApi.stopProject(closeConfirm.projectId),
        new Promise((_, r) => setTimeout(() => r(new Error('timeout')), 5000)),
      ]);
    } catch { /* timeout or already stopped */ }
    setClosing(false);
    setCloseConfirm(null);
  }, [closeConfirm]);

  // ── 渲染 ──────────────────────────────────────────────────

  return (
    <div className="flex flex-col h-full bg-nexus-bg">
      {/* Tab 栏 */}
      {sessions.length > 0 && (
        <div className="flex-shrink-0 flex items-end h-[34px] bg-nexus-surface border-b border-nexus-border/30 px-1 gap-0.5 overflow-x-auto">
          {sessions.map((s) => {
            const isActive = s.projectId === activeProjectId;
            const isRunning = (runningMap[s.projectId] ?? 0) > 0;
            return (
              <button
                key={s.projectId}
                className={`
                  group flex items-center gap-1.5 px-3 h-[30px] rounded-t-md text-[12px]
                  transition-colors select-none flex-shrink-0 max-w-[180px]
                  ${isActive
                    ? 'bg-nexus-bg text-nexus-text'
                    : 'text-nexus-muted hover:text-nexus-text hover:bg-nexus-bg/50'
                  }
                `}
                onClick={() => setActiveProjectId(s.projectId)}
              >
                <span className={`w-[6px] h-[6px] rounded-full flex-shrink-0 ${isRunning ? 'bg-emerald-400' : 'bg-nexus-muted/40'}`} />
                <span className="truncate">{s.projectName}</span>
                <span
                  role="button"
                  className="ml-0.5 w-[18px] h-[18px] flex items-center justify-center
                             rounded text-nexus-muted/50 hover:text-nexus-error hover:bg-nexus-error/10
                             opacity-0 group-hover:opacity-100 transition-opacity flex-shrink-0"
                  onClick={(e) => { e.stopPropagation(); requestClose(s.projectId); }}
                >×</span>
              </button>
            );
          })}
        </div>
      )}

      {/* 终端区域：所有 Terminal 同时挂载，CSS 控制显示 */}
      <div className="flex-1 overflow-hidden relative">
        {sessions.length === 0 ? (
          <div className="flex items-center justify-center h-full text-[12px] text-nexus-muted/50">
            启动服务后自动创建终端
          </div>
        ) : (
          sessions.map((s) => (
            <div
              key={s.projectId}
              className="absolute inset-0"
              style={{ visibility: s.projectId === activeProjectId ? 'visible' : 'hidden' }}
            >
              <Terminal ptySessionId={s.sessionId} workingDir={s.projectPath} visible={visible && s.projectId === activeProjectId} initCommand={s.initCommand} />
            </div>
          ))
        )}
      </div>


      {/* 关闭确认弹窗 */}
      <ConfirmDialog
        open={!!closeConfirm}
        variant="warning"
        title="关闭终端"
        description={closeConfirm ? `项目「${closeConfirm.projectName}」有 ${closeConfirm.runningCount} 个服务正在运行。` : undefined}
        consequences={['终端会话将被关闭', '该项目下所有运行中的服务将被停止']}
        confirmLabel="关闭并停止服务"
        cancelLabel="取消"
        loading={closing}
        onConfirm={handleConfirmClose}
        onClose={() => setCloseConfirm(null)}
      />
    </div>
  );
}
