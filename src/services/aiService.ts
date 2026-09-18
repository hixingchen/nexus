import { invoke } from '@tauri-apps/api/core';

/** 会话状态快照（字段与后端 AiStatus 同名：响应 DTO 一律 snake_case） */
interface AiStatus {
  running: boolean;
  /** 子 WebView 导航目标（后端已把 host 规范为 localhost；打包环境不受父页 CSP/cookie 同站限制） */
  url: string | null;
  pid: number | null;
  /** 会话工作目录 */
  cwd: string | null;
  /** dsh CLI 是否可用 */
  dsh_found: boolean;
}

/** 创建 AI 面板子 WebView 的入参（矩形以内容区 DOM 槽为准） */
interface PanelWebviewBounds {
  windowLabel: string;
  label: string;
  url: string;
  x: number;
  y: number;
  width: number;
  height: number;
}

export const aiService = {
  status: () => invoke<AiStatus>('ai_status'),
  /**
   * 启动（或按需重启）dsh web 会话。
   * cwd：工作目录；name：项目显示名（dsh workspace 标题用，保证 GUI
   * 工作区名与 Nexus 项目列表一致）
   */
  start: (cwd: string | null, name: string | null = null) =>
    invoke<AiStatus>('ai_start', { cwd, name }),
  stop: () => invoke<void>('ai_stop'),
  /** 查询 dsh 当前版本与 npm 最新版（联网） */
  checkUpdate: () => invoke<DshVersionInfo>('ai_check_update'),
  /** 升级/安装 dsh 到最新版；返回升级后的当前版本号（可能为空串） */
  upgrade: () => invoke<string>('ai_upgrade_dsh'),
  /**
   * 创建面板子 WebView。必须经 Rust 命令而非 JS `new Webview()`：
   * 只有命令入口能带 initialization_script（dsh 页面焦点记忆/恢复脚本），
   * 原因与实现见 src-tauri/src/commands/ai.rs。
   */
  createPanelWebview: (bounds: PanelWebviewBounds) =>
    invoke<void>('create_ai_panel_webview', { ...bounds }),
  /**
   * 把系统焦点交给面板子 WebView（切回窗口时用）。
   * 走 Rust 的 SetFocus：wry 的 set_focus 底层是 MoveFocus，语义不同会破坏输入。
   */
  focusPanel: (windowLabel: string, label: string) =>
    invoke<void>('ai_panel_focus', { windowLabel, label }),
};

/** dsh 版本信息（检查更新结果；字段与后端 DshVersionInfo 同名） */
export interface DshVersionInfo {
  /** dsh CLI 是否已安装 */
  dsh_found: boolean;
  /** 本地安装版本（dsh --version 解析；未安装/解析失败为 null） */
  current: string | null;
  /** npm registry 最新稳定版 */
  latest: string | null;
  /** 是否存在可升级的新版本 */
  outdated: boolean;
}
