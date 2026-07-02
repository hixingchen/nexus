/// 终端会话
export interface TerminalSession {
  id: string;
  /** 所属项目 ID */
  projectId: string;
  /** 所属项目名称（Tab 显示用） */
  projectName: string;
  /** 项目路径 */
  projectPath: string;
  /** PTY 会话 ID（后端用） */
  sessionId: string;
  /** 初始命令 */
  initCommand?: string;
}

