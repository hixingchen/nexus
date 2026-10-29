import { invoke } from '@tauri-apps/api/core';

/**
 * 服务开机启动（登录前）相关 IPC。
 *
 * 与 `system.ts` 的「开机自启」不是同一件事：那个是 Nexus 自己在用户登录时启动；
 * 这里是**服务**在登录界面就由计划任务拉起的 runner 启动。
 */

export interface BootRunnerStatus {
  /** s4u / password */
  mode: 's4u' | 'password';
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
  /** runner 进程是否还在（不要求心跳）：任务卸载后残留实例的判断依据 */
  runner_alive: boolean;
  /** 当前运行实例的模式（state.json）；任务模式可能已切换但实例尚未重启 */
  running_mode: string;
  runner_pid: number | null;
  heartbeat_at: string;
  /** DB 里开机启动且启用的服务数 */
  boot_service_count: number;
  /** not_installed / installed_stopped / stopped / running / stale / exited */
  state: string;
  message: string;
}

/**
 * 单个开机服务的状态（设置页逐服务列表用）。
 *
 * 与 `getRunning` 的差别：读取不要求启动器可响应。`alive` 由后端用
 * pid + 进程启动时刻校验，启动器退出后仍能说清服务还在不在跑。
 */
export interface BootServiceStatus {
  service_id: string;
  name: string;
  project_id: string;
  /** s4u / password / legacy（旧版单任务） */
  mode: string;
  /** state.json 的 phase；空串 = 已配置开机启动但启动器从未记录过它 */
  phase: string;
  /** 进程是否仍活着（pid + 启动时刻校验） */
  alive: boolean;
  pid: number | null;
  started_at: string;
  ready_at: string;
  exit_code: number | null;
  error: string;
  log_path: string;
  /** 该来源的启动器是否存活且心跳新鲜 */
  runner_reachable: boolean;
  /** 该来源的启动器进程是否还在（不要求心跳） */
  runner_alive: boolean;
  /** 当前 DB 是否仍配置为开机启动 */
  configured: boolean;
  /** DB 里服务的「跟随项目启动」开关；false 时 manifest 会排除它（开机不会启动） */
  service_enabled: boolean;
}

export interface BootToggleResult {
  boot_enabled: boolean;
  /** manifest 同步或通知 runner 失败的原因（空 = 成功） */
  boot_manifest_error: string;
}

export const bootApi = {
  /** 读取两个模式的任务 + runner 真实状态（会回读计划任务，不是读缓存） */
  getStatus: () => invoke<BootRunnerStatus[]>('get_boot_runner_status'),
  /** 逐服务状态（含 runner 不可达时仍存活的实例、旧版遗留记录） */
  getServices: () => invoke<BootServiceStatus[]>('get_boot_services_status'),
  /**
   * 安装/修复/切换开机任务（需要一次管理员授权）。
   * `password` 模式会额外弹出 Windows 原生凭据对话框，密码只交给 Task Scheduler。
   */
  install: (mode: 's4u' | 'password') =>
    invoke<BootRunnerStatus>('install_boot_runner', { mode }),
  uninstall: (mode: 's4u' | 'password') =>
    invoke<BootRunnerStatus>('uninstall_boot_runner', { mode }),
  /** 用当前 manifest 立即启动一次该模式的 runner（通过计划任务） */
  start: (mode: 's4u' | 'password') =>
    invoke<BootRunnerStatus>('start_boot_runner', { mode }),
  /** 让该模式 runner 干净退出（会停掉它启动的服务） */
  stop: (mode: 's4u' | 'password') =>
    invoke<BootRunnerStatus>('stop_boot_runner', { mode }),
  /** 快速开/关单个服务的开机启动（不动当前进程） */
  setServiceEnabled: (serviceId: string, enabled: boolean) =>
    invoke<BootToggleResult>('set_service_boot_enabled', { serviceId, enabled }),
};
