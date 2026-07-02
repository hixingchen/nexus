import { create } from 'zustand';
import type { TerminalSession } from '../types/terminal';

interface TerminalStore {
  /** 所有终端会话（按 projectId 唯一） */
  sessions: TerminalSession[];
  /** 当前活动的终端 projectId */
  activeProjectId: string | null;

  // 同步操作
  addSession: (session: TerminalSession) => void;
  removeSession: (projectId: string) => void;
  setActiveProjectId: (projectId: string | null) => void;

  // 查询
  hasSession: (projectId: string) => boolean;
  getSession: (projectId: string) => TerminalSession | undefined;
  activeSession: () => TerminalSession | undefined;
}

export const useTerminalStore = create<TerminalStore>((set, get) => ({
  sessions: [],
  activeProjectId: null,

  addSession: (session) =>
    set((state) => ({
      sessions: [...state.sessions.filter(s => s.projectId !== session.projectId), session],
      activeProjectId: session.projectId,
    })),

  removeSession: (projectId) =>
    set((state) => {
      const newSessions = state.sessions.filter((s) => s.projectId !== projectId);
      const newActiveId = state.activeProjectId === projectId
        ? (newSessions[0]?.projectId ?? null)
        : state.activeProjectId;
      return { sessions: newSessions, activeProjectId: newActiveId };
    }),

  setActiveProjectId: (projectId) => set({ activeProjectId: projectId }),

  hasSession: (projectId) => get().sessions.some(s => s.projectId === projectId),

  getSession: (projectId) => get().sessions.find(s => s.projectId === projectId),

  activeSession: () => {
    const { sessions, activeProjectId } = get();
    return sessions.find(s => s.projectId === activeProjectId);
  },
}));

// ── 异步操作 ──────────────────────────────────────────────

/**
 * 为项目创建终端 Tab（仅注册元数据，PTY 由 Terminal 组件创建）
 */
export function createProjectTab(
  projectId: string,
  projectName: string,
  projectPath: string,
  initCommand?: string,
): void {
  const { hasSession, addSession } = useTerminalStore.getState();
  if (hasSession(projectId)) return;

  const sessionId = `term-${projectId}-${Date.now()}`;
  addSession({ id: projectId, projectId, projectName, projectPath, sessionId, initCommand });
}

/**
 * 关闭项目的终端 Tab（PTY 由 Terminal 组件的 cleanup 关闭）
 */
export function closeProjectTab(projectId: string): void {
  useTerminalStore.getState().removeSession(projectId);
}
