import { invoke } from '@tauri-apps/api/core';

/** 会话状态快照（字段与后端 AiStatus 对齐，serde camelCase） */
export interface AiStatus {
  running: boolean;
  /** 子 WebView 导航目标（后端已把 host 规范为 localhost；打包环境不受父页 CSP/cookie 同站限制） */
  url: string | null;
  pid: number | null;
  /** 会话工作目录 */
  cwd: string | null;
  /** dsh CLI 是否可用 */
  dshFound: boolean;
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
};

/** dsh 版本信息（检查更新结果；字段与后端 DshVersionInfo 对齐，serde camelCase） */
export interface DshVersionInfo {
  /** dsh CLI 是否已安装 */
  dshFound: boolean;
  /** 本地安装版本（dsh --version 解析；未安装/解析失败为 null） */
  current: string | null;
  /** npm registry 最新稳定版 */
  latest: string | null;
  /** 是否存在可升级的新版本 */
  outdated: boolean;
}
