import { invoke } from '@tauri-apps/api/core';

/**
 * 服务开机启动（登录前）相关 IPC。
 *
 * 与 `system.ts` 的「开机自启」不是同一件事：那个是 Nexus 自己在用户登录时启动；
 * 这里是**服务**在登录界面就由计划任务拉起的 runner 启动。
 */

export interface BootRunnerStatus {
  installed: boolean;
  task_name: string;
  /** 回读计划任务得到的登录类型（S4U / Password / 空） */
  task_logon_type: string;
  /** 任务 action 是否指向稳定 runner 路径 + 当前 manifest */
  task_path_ok: boolean;
  /** 稳定目录里的 runner 路径（任务 action 指向它） */
  runner_path: string;
  /** 稳定目录里的 runner 文件是否存在 */
  runner_installed: boolean;
  runner_reachable: boolean;
  runner_pid: number | null;
  heartbeat_at: string;
  /** DB 里开机启动且启用的服务数 */
  boot_service_count: number;
  /** not_installed / installed_stopped / running / unreachable */
  state: string;
  message: string;
}

export interface BootToggleResult {
  boot_enabled: boolean;
  /** manifest 同步或通知 runner 失败的原因（空 = 成功） */
  boot_manifest_error: string;
}

export const bootApi = {
  /** 读取任务 + runner 的真实状态（会回读计划任务，不是读缓存） */
  getStatus: () => invoke<BootRunnerStatus>('get_boot_runner_status'),
  /** 安装/修复 S4U 开机任务（需要一次管理员授权） */
  install: () => invoke<BootRunnerStatus>('install_boot_runner'),
  uninstall: () => invoke<BootRunnerStatus>('uninstall_boot_runner'),
  /** 用当前 manifest 立即启动一次 runner（通过计划任务） */
  start: () => invoke<BootRunnerStatus>('start_boot_runner'),
  /** 让 runner 干净退出（会停掉它启动的服务） */
  stop: () => invoke<BootRunnerStatus>('stop_boot_runner'),
  /** 快速开/关单个服务的开机启动（不动当前进程） */
  setServiceEnabled: (serviceId: string, enabled: boolean) =>
    invoke<BootToggleResult>('set_service_boot_enabled', { serviceId, enabled }),
};
