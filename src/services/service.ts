import { invoke } from '@tauri-apps/api/core';

export interface ToolCommand {
  id: string;
  name: string;
  command: string;
}

export interface Service {
  id: string;
  project_id: string;
  name: string;
  command: string;
  cwd: string;
  watch_paths: string;    // JSON array
  watch_include: string;  // glob patterns, one per line
  watch_exclude: string;  // patterns to exclude, one per line
  env_vars: string;       // KEY=VALUE dotenv format
  restart_mode: number;  // 0=关闭监听, 1=确认重启, 2=自动重启
  enabled: boolean;
  show_file_tree: boolean;
  sort_index: number;
  tool_commands: string;  // JSON array of ToolCommand
}

export interface Project {
  id: string;
  name: string;
  path: string;
  pinned: boolean;
  sort_index: number;
  terminal_init_command: string;
}

export interface ProjectDetail {
  project: Project;
  services: Service[];
}

export interface ToolCommandResult {
  success: boolean;
  stdout: string;
  stderr: string;
  exit_code: number | null;
}

// ─── Service API (scoped to project) ────────────────────────

export const serviceApi = {
  /** 获取某项目下所有服务 */
  getByProject: (projectId: string) =>
    invoke<Service[]>('get_services', { projectId }),

  /** 给项目添加服务 */
  add: (params: {
    projectId: string;
    name: string;
    command: string;
    cwd: string;
    watchPaths: string;
    envVars: string;
    restartMode: number;
    toolCommands: string;
  }) => invoke<Service>('add_service', {
    params: {
      projectId: params.projectId,
      name: params.name,
      command: params.command,
      cwd: params.cwd,
      watchPaths: params.watchPaths,
      envVars: params.envVars,
      restartMode: params.restartMode,
      toolCommands: params.toolCommands,
    }
  }),

  /** 更新服务配置 */
  update: (params: {
    id: string;
    name: string;
    command: string;
    cwd: string;
    watchPaths: string;
    watchInclude: string;
    watchExclude: string;
    envVars: string;
    restartMode: number;
    enabled: boolean;
    showFileTree: boolean;
    toolCommands: string;
  }) => invoke<void>('update_service', {
    params: {
      id: params.id,
      name: params.name,
      command: params.command,
      cwd: params.cwd,
      watchPaths: params.watchPaths,
      watchInclude: params.watchInclude,
      watchExclude: params.watchExclude,
      envVars: params.envVars,
      restartMode: params.restartMode,
      enabled: params.enabled,
      showFileTree: params.showFileTree,
      toolCommands: params.toolCommands,
    }
  }),

  /** 删除服务 */
  delete: (id: string) => invoke<void>('delete_service', { id }),
};

// ─── Project API ────────────────────────────────────────────

export const projectApi = {
  /** 获取所有项目（列表用，不含服务） */
  getAll: () => invoke<Project[]>('get_projects'),

  /** 获取项目详情（含服务列表） */
  getDetail: (projectId: string) =>
    invoke<ProjectDetail>('get_project_detail', { projectId }),

  /** 创建项目 */
  add: (name: string, path: string, terminalInitCommand: string = '') =>
    invoke<Project>('add_project', { name, path, terminalInitCommand }),

  /** 更新项目 */
  update: (id: string, name: string, path: string, terminalInitCommand: string) =>
    invoke<void>('update_project', { id, name, path, terminalInitCommand }),

  /** 删除项目 */
  delete: (id: string) => invoke<void>('delete_project', { id }),

  /** 复制项目（含所有服务配置） */
  duplicate: (id: string) => invoke<Project>('duplicate_project', { id }),

  /** 切换项目置顶 */
  togglePin: (id: string) => invoke<boolean>('toggle_pin_project', { id }),
};

// ─── Process API ────────────────────────────────────────────

export const processApi = {
  /** 启动单个服务（传 service_id） */
  start: (serviceId: string) =>
    invoke<void>('start_service', { serviceId }),

  /** 停止单个服务 */
  stop: (serviceId: string) =>
    invoke<void>('stop_service', { serviceId }),

  /** 重启单个服务 */
  restart: (serviceId: string) =>
    invoke<void>('restart_service', { serviceId }),

  /** 一键启动项目所有已启用服务 */
  startProject: (projectId: string) =>
    invoke<string[]>('start_project_services', { projectId }),

  /** 停止项目所有服务 */
  stopProject: (projectId: string) =>
    invoke<void>('stop_project_services', { projectId }),

  /** 获取当前运行中的 key 列表 */
  getRunning: () => invoke<string[]>('get_running'),

  /** 执行工具命令 */
  runToolCommand: (serviceId: string, commandId: string) =>
    invoke<ToolCommandResult>('run_tool_command', { serviceId, commandId }),
};

// ─── Watcher API ───────────────────────────────────────────

export interface FileChange {
  path: string;
  service_name: string;
  service_id: string;
  kind: string;
}

export interface FileChangeEvent {
  project_id: string;
  project_name: string;
  changes: FileChange[];
}

// ─── Layout API ────────────────────────────────────────────

export const layoutApi = {
  save: (items: Record<string, string>) =>
    invoke<void>('save_layout', { items }),

  load: () => invoke<Record<string, string>>('load_layout'),
};

// ─── Security API ──────────────────────────────────────────

export const securityApi = {
  setProjectRoot: (root: string | null) =>
    invoke<void>('set_project_root', { path: root }),
};

// ─── Watcher API ───────────────────────────────────────────

export const watchApi = {
  start: (projectId: string) =>
    invoke<void>('start_watching', { projectId }),

  stop: (projectId: string) =>
    invoke<void>('stop_watching', { projectId }),
};
