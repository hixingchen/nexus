import { invoke } from '@tauri-apps/api/core';
import { reportError } from '../utils/error';
import type { ConflictPolicy, PasteResponse } from '../utils/pasteResult';
import type { FileEntry } from '../types/file';

/**
 * 系统级调用封装（原生对话框 / 外部程序 / 剪贴板文件）。
 *
 * 为什么单独一个模块：这些命令此前在 5 个组件里各自 `invoke(...)` + 各写一份
 * try/catch + 提示（`open_in_explorer` 5 处、`open_terminal` 3 处、`list_directory` 3 处），
 * 改一次签名/文案要改 5 个文件。放在这里后，组件只调用语义化的函数。
 */

/**
 * 原生目录选择器（由 Rust 侧弹框）。
 *
 * 为什么要走后端：选择结果会被后端记为"用户已确认的目录"，从而允许把它配成项目外的工作目录。
 * 若走前端 `plugin-dialog` + 一个"确认目录"命令，被攻陷的 webview 可以自己确认任意路径，
 * 白名单收口就形同虚设（见 `src-tauri/src/commands/editor.rs` 的"配置路径收口"）。
 */
/**
 * 用途键 → 对话框标题由**后端**决定（SEC-17）：原生对话框无法被网页伪造，用户对它的
 * 信任天然更高，所以标题不能被 IPC 指定——否则被攻陷的 webview 能弹出标题写着
 * 「Nexus 需要访问 …\.ssh 才能继续」的系统级选择框。这里只传用途，不传文案。
 */
export type PickPurpose = 'projectDir' | 'serviceCwd';

export function pickDirectory(opts?: { purpose?: PickPurpose; defaultPath?: string }): Promise<string | null> {
  return invoke<string | null>('pick_directory', {
    purpose: opts?.purpose ?? null,
    defaultPath: opts?.defaultPath ?? null,
  });
}

/** 保存对话框的用途键（标题由后端决定，同 `PickPurpose`） */
export type SavePurpose = 'exportTemplates';

/**
 * 原生文件保存对话框（由 Rust 侧弹框）。
 *
 * 为什么必须走后端（SEC-19）：导出要把文件写到"用户挑的位置"，而这个结论不能由一个
 * IPC 参数说了算——前端 `plugin-dialog` 的 `save()` 拿到的路径经 IPC 回传，服务端分辨不出
 * 它是不是用户刚选的。Rust 侧弹框会把选中的**所在目录**当场记为"已确认"，于是导出到
 * 桌面/文档照常可用，而凭空指定 `C:/Windows/...` 会被白名单拒掉。
 *
 * `defaultName` 只是对话框里的默认文件名，不是写入路径。
 */
export function pickSaveFile(opts?: { purpose?: SavePurpose; defaultName?: string }): Promise<string | null> {
  return invoke<string | null>('pick_save_file', {
    purpose: opts?.purpose ?? null,
    defaultName: opts?.defaultName ?? null,
  });
}

/**
 * 当前应用版本号（"关于"与「检查更新」共用）。
 *
 * 为什么由后端答而不是前端读 `package.json`：那是 npm 侧的版本号，与打包出来的安装包
 * 未必同步；后端返回的是 `CARGO_PKG_VERSION`——**这个二进制自己的**版本（见
 * `commands/app.rs::get_app_version`）。
 *
 * 收进本模块是为了守住"IPC 面收敛在 `services/*`"这条可机械审计的规则（SEC-30）：
 * `TitleBar` 原先直接 `invoke('get_app_version')`，那是个只读、无参数、无风险的调用，
 * 但它是全仓仅剩的两处例外之一——新命令照抄这种写法就会绕过服务层。
 */
export function getAppVersion(): Promise<string> {
  return invoke<string>('get_app_version');
}

/** 在系统资源管理器中打开（失败带原因提示） */
export async function openInExplorer(path: string): Promise<void> {
  try {
    await invoke('open_in_explorer', { path });
  } catch (e) {
    reportError('打开资源管理器失败', e);
  }
}

/** 在系统终端中打开（失败带原因提示） */
export async function openTerminal(path: string): Promise<void> {
  try {
    await invoke('open_terminal', { path });
  } catch (e) {
    reportError('打开终端失败', e);
  }
}

/**
 * 粘贴结果类型定义在 `utils/pasteResult.ts`（那里零依赖、可被 node 直接测），
 * 这里只做转发，免得字段名在前后端之外又多出一份手工副本。
 */
export type { PasteResponse } from '../utils/pasteResult';

/** 列目录（文件树 / jar 子树用） */
export function listDirectory(path: string) {
  return invoke<FileEntry[]>('list_directory', { path });
}

/**
 * 把系统剪贴板里的文件粘贴到目标目录。**两阶段**：
 *
 * - `conflictPolicy` 为空 = 探测（后端**一个字节都不落盘**）：有同名冲突就回
 *   `status: 'conflict'`，交给弹框问用户；没冲突就直接粘完。
 * - 定好策略后再调一次，并把探测那轮回传的 `expectedSources` 原样带上——后端只拿它跟
 *   **当前剪贴板**比对（弹框开着时用户可能去别处又复制了一次），**不当作复制输入**
 *   （理由见 `commands/fileops.rs` 的 `PasteResponse`）。
 */
export function pasteFiles(
  targetDir: string,
  conflictPolicy: ConflictPolicy | null,
  expectedSources: string[] | null,
) {
  return invoke<PasteResponse>('paste_files', { targetDir, conflictPolicy, expectedSources });
}

/** 把文件写入系统剪贴板（复制文件本身，不是文本） */
export function copyFilesToClipboard(paths: string[]) {
  return invoke<void>('copy_files_to_clipboard', { paths });
}

/**
 * 删除文件/目录到系统回收站（目录整棵进回收站）。
 *
 * 不在这里 catch：调用方要按成败决定"关掉对应标签 + 刷新父目录"还是"留在确认框里重试"，
 * 所以错误交给它（与 `pasteFiles` 同口径，`openInExplorer` 那种无后续动作的才自带提示）。
 */
export function deletePath(path: string) {
  return invoke<void>('delete_path', { path });
}

/**
 * 请求退出应用：后端先做非阻塞清理（停服务、关 AI 会话、落库），完成后再真正退出。
 *
 * 为什么不在前端直接 `getCurrentWindow().close()`：窗口关闭会立刻销毁 WebView，
 * 而清理（等进程退出/刷盘）需要时间，用户会看到"服务还在跑但界面没了"。
 * 关闭前的未保存确认由 CloseGuard 负责，这里只负责"确认后怎么退"。
 */
export function prepareExit() {
  return invoke<void>('prepare_exit');
}
