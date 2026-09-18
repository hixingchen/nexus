import { invoke } from '@tauri-apps/api/core';
import { reportError } from '../utils/error';
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

/** 粘贴结果：created 为已落盘路径，failed 为逐个源的失败原因（空数组 = 全部成功） */
export interface PasteFilesResult {
  created: string[];
  failed: string[];
}

/** 列目录（文件树 / jar 子树用） */
export function listDirectory(path: string) {
  return invoke<FileEntry[]>('list_directory', { path });
}

/** 把系统剪贴板里的文件粘贴到目标目录（返回新建与失败清单） */
export function pasteFiles(targetDir: string) {
  return invoke<PasteFilesResult>('paste_files', { targetDir });
}

/** 把文件写入系统剪贴板（复制文件本身，不是文本） */
export function copyFilesToClipboard(paths: string[]) {
  return invoke<void>('copy_files_to_clipboard', { paths });
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
