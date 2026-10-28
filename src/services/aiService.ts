import { invoke } from '@tauri-apps/api/core';

/**
 * 私有运行时的探活结果（字段与后端 `DshProbe` 同名）。
 *
 * 把"没装"（`ready: false`）与"装了但跑不起来"（`ready: true` + `problem`）分开——
 * 这是本轮事故的直接教训：原来只回一个 bool，界面只能笼统说"未检测到 dsh，请先安装"，
 * 用户照着重装一遍还是不行，因为真正的原因是"当前 Node 跑不动 dsh"。
 */
interface DshProbe {
  ready: boolean;
  /** 当前生效的那对路径来自哪里（null = 未就绪） */
  source: RuntimeSource | null;
  /** 自带 Node 的版本（如 `v24.21.0`） */
  node_version: string | null;
  /** 实测到的 dsh 版本（跑得起来时才有） */
  dsh_version?: string | null;
  /** 跑不起来时的原因（人话 + 原话） */
  problem: string | null;
}

/** 会话状态快照（字段与后端 AiStatus 同名：响应 DTO 一律 snake_case） */
interface AiStatus {
  running: boolean;
  /** 子 WebView 导航目标（后端已把 host 规范为 localhost；打包环境不受父页 CSP/cookie 同站限制） */
  url: string | null;
  pid: number | null;
  /** 会话工作目录 */
  cwd: string | null;
  runtime: DshProbe;
}

/** 这一对路径是哪来的（与后端 `RuntimeSource` 的 serde 表示一致） */
export type RuntimeSource = 'manual' | 'private';

/** AI 运行时状态（设置 > AI 运行时；字段与后端 AiRuntimeStatus 同名） */
export interface AiRuntimeStatus {
  ready: boolean;
  /** 当前生效的那对路径来自哪里（null = 未就绪） */
  source: RuntimeSource | null;
  node_version: string | null;
  dsh_version: string | null;
  problem: string | null;
  /** 占用磁盘字节数（"卸载能腾出多少"） */
  size_bytes: number;
  /** 当前生效的那对路径（手动指定时是用户填的那些） */
  path: string;
  /** 自动装的那份的安装目录——**"卸载"删的始终是这一个** */
  install_dir: string;
  /** 是否设了手动指定（界面据此显示「清除」入口） */
  has_manual: boolean;
  /** 安装步骤名（后端 `STEPS` 的唯一事实来源，界面不自己维护一份） */
  steps: string[];
}

/** 安装进度（`ai-runtime-progress` 事件载荷） */
export interface RuntimeProgress {
  /** 当前步骤下标（`steps` 的下标） */
  step_index: number;
  /** 这一步里的子状态（如 "12.3 MB / 35.9 MB"） */
  detail: string;
  downloaded: number;
  total: number;
}

/** 运行时安装进度事件名（与 `commands/ai.rs` 的 `app.emit` 一致） */
export const AI_RUNTIME_PROGRESS_EVENT = 'ai-runtime-progress';

/** 导出 / 导入进度（`ai-runtime-transfer-progress` 事件载荷） */
export interface RuntimeTransferProgress {
  /** 这次是导出还是导入（决定界面文案） */
  kind: 'export' | 'import';
  /** 阶段名（"打包" / "解压" / "验证" / "替换"） */
  phase: string;
  /** 阶段内的细节（当前文件 / 字节数） */
  detail: string;
  /** 已处理字节 */
  done: number;
  /** 预期字节（0 = 未知） */
  total: number;
}

/** 运行时打包 / 导入进度事件名（与 `commands/ai.rs` 的常量一致） */
export const AI_RUNTIME_TRANSFER_PROGRESS_EVENT = 'ai-runtime-transfer-progress';

/** 导出结果（`ai_runtime_export` 返回值） */
export interface RuntimeExportResult {
  path: string;
  bytes: number;
  entries: number;
  node_version: string;
  dsh_version: string;
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
  /**
   * 当前项目目录：后端据此找出该项目最新的会话，在 dsh 页面脚本运行前预置
   * "上次会话"记忆（新 origin 的第一次打开也能直接恢复，而不是新建空白会话）。
   */
  projectCwd: string | null;
  /**
   * false（默认）= 每次打开新建会话：清掉 dsh 的"当前会话"记忆；
   * true = 恢复上次使用的会话。
   */
  resumeSession: boolean;
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

  // ─── 私有运行时（设置 > AI 运行时） ───────────────────────
  /** 运行时状态：版本、路径、占用空间、跑不起来时的原因 */
  runtimeStatus: () => invoke<AiRuntimeStatus>('ai_runtime_status'),
  /**
   * 安装 / 重新安装运行时（下载 Node + 装 dsh，首次约 300MB）。
   * 后端会**先停掉 AI 会话**（私有 node.exe 被占用就删不掉旧目录）。
   * 进度经 `ai-runtime-progress` 事件推送；失败时 reject 的是 `RuntimeFailure` 对象。
   */
  runtimeInstall: () => invoke<AiRuntimeStatus>('ai_runtime_install'),
  /** 卸载运行时（删 `~/.nexus/runtime`，约 300MB）。**不碰 `~/.dsh`**（登录凭据与全部会话） */
  runtimeUninstall: () => invoke<void>('ai_runtime_uninstall'),
  /**
   * **手动指定**运行时（A 方案）：填 `node.exe` 与 dsh 的路径，绕开那 300MB 下载。
   * 给内网机器、或机器上本来就有可用 Node + dsh 的情况。
   * 后端保存前会**当场实测一次**并先停掉会话，失败时会 reject 一个人话原因。
   */
  runtimeSetManual: (nodeExe: string, dshPath: string) =>
    invoke<AiRuntimeStatus>('ai_runtime_set_manual', { nodeExe, dshPath }),
  /** 清除手动指定，回到用 Nexus 自带的那份 */
  runtimeClearManual: () => invoke<AiRuntimeStatus>('ai_runtime_clear_manual'),
  /**
   * 导出自带的 AI 运行时为 zip（内网 / 下载不通时搬到目标机）。
   * 只支持 `source === 'private'` 且实测可跑的那份。
   */
  runtimeExport: (path: string) => invoke<RuntimeExportResult>('ai_runtime_export', { path }),
  /**
   * 从 zip 导入并替换 Nexus 自带的运行时。
   * 后端会先停 AI 会话；解压/验证失败不动现有运行时；成功后清除会遮住它的手动指定。
   */
  runtimeImport: (path: string) => invoke<AiRuntimeStatus>('ai_runtime_import', { path }),
};

// ─── 错误结构化 ────────────────────────────────────────────────

/** 后端错误码前缀（与 `src-tauri/src/commands/ai.rs` 的同名常量一一对应） */
const ERR_RUNTIME_MISSING = 'ai_runtime_missing';
const ERR_RUNTIME_BROKEN = 'ai_runtime_broken';

export type AiErrorKind =
  /** 运行时还没准备（给「准备运行时」按钮） */
  | 'runtime_missing'
  /** 装了但跑不起来（给「重新安装」按钮 + 原话） */
  | 'runtime_broken'
  | 'other';

/**
 * 结构化后的 AI 错误。
 *
 * 为什么不在界面里直接判 `msg.includes(...)`：那种写法把"文案"当成了协议——后端改一个字，
 * 界面就静默走错分支。这里改成解析**错误码**，文案怎么改都不影响该给哪个按钮。
 */
export interface AiError {
  kind: AiErrorKind;
  /** 卡在哪一步（后端给的步骤名）；与具体步骤无关时为 null */
  step: string | null;
  /** 人话摘要（卡片正文那句） */
  summary: string;
  /** 原始输出（npm / node / curl 的原话），可折叠、可复制 */
  raw: string;
}

/** 把后端错误翻成结构。两种形态：字符串（`<码>::<人话>`）与对象（安装失败的 `RuntimeFailure`）。 */
export function toAiError(e: unknown): AiError {
  // ① RuntimeFailure 对象（`ai_runtime_install` 的 Err 载荷，Tauri 会原样反序列化）
  if (e != null && typeof e === 'object' && 'summary' in e) {
    const f = e as { step?: string; summary?: string; raw?: string };
    return {
      kind: 'runtime_broken',
      step: f.step ?? null,
      summary: f.summary ?? '安装失败',
      raw: f.raw ?? '',
    };
  }
  const s = typeof e === 'string' ? e : String(e);
  const i = s.indexOf('::');
  const code = i > 0 ? s.slice(0, i) : '';
  const rest = i > 0 ? s.slice(i + 2) : s;

  if (code === ERR_RUNTIME_MISSING) {
    return { kind: 'runtime_missing', step: null, summary: rest, raw: '' };
  }
  if (code === ERR_RUNTIME_BROKEN) {
    return { kind: 'runtime_broken', ...splitStep(rest) };
  }
  return { kind: 'other', step: null, summary: s, raw: '' };
}

/** 拆 `【步骤】摘要\n\n原话`（形状由 `RuntimeFailure::to_message` 决定） */
function splitStep(text: string): { step: string | null; summary: string; raw: string } {
  const m = /^【(.+?)】([\s\S]*)$/.exec(text.trim());
  if (!m) return { step: null, summary: text, raw: '' };
  const body = m[2] ?? '';
  const cut = body.indexOf('\n\n');
  return cut < 0
    ? { step: m[1] ?? null, summary: body.trim(), raw: '' }
    : { step: m[1] ?? null, summary: body.slice(0, cut).trim(), raw: body.slice(cut + 2).trim() };
}

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
