import { invoke } from '@tauri-apps/api/core';

export interface ToolCommand {
  id: string;
  name: string;
  command: string;
  /** 执行超时（秒）：不填 = 默认 60；0 = 不限制（长构建类命令）；正数 = 该秒数 */
  timeout_secs?: number;
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
}

export interface ProjectDetail {
  project: Project;
  services: Service[];
}

export interface ToolCommandResult {
  success: boolean;
  /** stdout/stderr 按时间序合并的输出（与 cmd 终端展示一致） */
  output: string;
  exit_code: number | null;
}

/** 服务模板：跨项目复用的服务配置 */
export interface ServiceTemplate {
  id: string;
  name: string;
  command: string;
  cwd: string;
  watch_paths: string;
  watch_include: string;
  watch_exclude: string;
  env_vars: string;
  restart_mode: number;
  enabled: boolean;
  show_file_tree: boolean;
  tool_commands: string;
  /** 模板携带的默认打开工具（从模板添加服务时复制为新服务绑定） */
  open_tool_id: string;
  created_at: string;
}

/** 工具命令实时输出事件（run_id 用于区分并发执行） */
export interface ToolCommandLogPayload {
  run_id: string;
  stream: 'stdout' | 'stderr';
  data: string;
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

  /** 重排项目服务顺序（orderedIds 为新的展示顺序） */
  reorderServices: (projectId: string, orderedIds: string[]) =>
    invoke<void>('reorder_services', { projectId, orderedIds }),

  // ── 服务模板（跨项目复用） ──
  getServiceTemplates: () => invoke<ServiceTemplate[]>('get_service_templates'),
  /** 重排服务模板顺序 */
  reorderServiceTemplates: (orderedIds: string[]) =>
    invoke<void>('reorder_service_templates', { orderedIds }),
  saveServiceAsTemplate: (serviceId: string) =>
    invoke<ServiceTemplate>('save_service_as_template', { serviceId }),
  addServiceFromTemplate: (projectId: string, templateId: string) =>
    invoke<Service>('add_service_from_template', { projectId, templateId }),

  /** 更新模板配置（编辑模板本身） */
  updateTemplate: (params: {
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
    openToolId: string;
  }) => invoke<void>('update_service_template', {
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
      openToolId: params.openToolId,
    }
  }),

  deleteServiceTemplate: (id: string) =>
    invoke<void>('delete_service_template', { id }),
};

// ─── Project API ────────────────────────────────────────────

export const projectApi = {
  /** 获取所有项目（列表用，不含服务） */
  getAll: () => invoke<Project[]>('get_projects'),

  /** 获取项目详情（含服务列表） */
  getDetail: (projectId: string) =>
    invoke<ProjectDetail>('get_project_detail', { projectId }),

  /** 创建项目 */
  add: (name: string, path: string) =>
    invoke<Project>('add_project', { name, path }),

  /** 更新项目 */
  update: (id: string, name: string, path: string) =>
    invoke<void>('update_project', { id, name, path }),

  /** 删除项目 */
  delete: (id: string) => invoke<void>('delete_project', { id }),

  /** 复制项目（含所有服务配置） */
  duplicate: (id: string) => invoke<Project>('duplicate_project', { id }),

  /** 切换项目置顶 */
  togglePin: (id: string) => invoke<boolean>('toggle_pin_project', { id }),
};

// ─── Process API ────────────────────────────────────────────

/** 运行中的服务（含所属项目，供项目级运行状态判断） */
export interface RunningService {
  service_id: string;
  project_id: string;
}

/** 意外退出的服务（崩溃/秒退/spawn 失败）；主动停止不算失败 */
export interface FailedService {
  service_id: string;
  /** spawn 失败（进程未启动）时为 null */
  exit_code: number | null;
  timestamp: string;
}

/** 运行状态总览：运行中 + 意外失败 */
export interface ProcessStatus {
  running: RunningService[];
  failed: FailedService[];
}

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

  /** 获取运行状态总览（运行中 + 意外失败） */
  getRunning: () => invoke<ProcessStatus>('get_running'),

  /** 执行工具命令 */
  runToolCommand: (serviceId: string, commandId: string, runId: string) =>
    invoke<ToolCommandResult>('run_tool_command', { serviceId, commandId, runId }),

  /** 停止正在执行的工具命令（按 run_id 终止其进程树） */
  stopToolCommand: (runId: string) => invoke<void>('stop_tool_command', { runId }),
};

// ─── Watcher API ───────────────────────────────────────────

export interface FileChange {
  path: string;
  service_name: string;
  service_id: string;
  kind: string;
  /** 0=关闭监听, 1=确认重启, 2=自动重启 */
  restart_mode: number;
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
  /** 启动文件监听：serviceId 为空=项目级（所有服务），非空=单服务追加 */
  start: (projectId: string, serviceId?: string) =>
    invoke<void>('start_watching', { projectId, serviceId: serviceId ?? null }),

  /** 停止文件监听：serviceId 为空=项目级（全部停止），非空=仅移除该服务 */
  stop: (projectId: string, serviceId?: string) =>
    invoke<void>('stop_watching', { projectId, serviceId: serviceId ?? null }),
};

// ─── 外部打开工具 API（服务右键「用 XX 打开」）────────────

/**
 * 外部打开工具（服务右键「用 XX 打开」）
 * executable：程序路径（.exe 直启；.cmd/.bat 自动走 cmd）
 * args：参数模板，{path} 以独立参数注入（不经过 shell，路径含空格无需引号）
 * command：旧版整串命令（遗留字段，executable 为空的历史行使用）
 */
export interface OpenTool {
  id: string;
  name: string;
  command: string;
  executable: string;
  args: string;
}

export const openToolsApi = {
  /** 全部工具（全局共享，按添加顺序） */
  list: () => invoke<OpenTool[]>('list_open_tools'),

  /** 新增（id 空）或更新工具 */
  save: (tool: { id?: string | null; name: string; executable: string; args: string }) =>
    invoke<OpenTool>('save_open_tool', { params: tool }),

  /** 删除工具（服务绑定级联解除） */
  delete: (id: string) => invoke<void>('delete_open_tool', { id }),

  /** 绑定/解绑服务与工具（toolId 为 null → 解绑） */
  bindService: (serviceId: string, toolId: string | null) =>
    invoke<void>('set_service_open_tool', { serviceId, toolId }),

  /** 项目下所有服务的工具绑定 */
  listBindings: (projectId: string) =>
    invoke<{ service_id: string; tool_id: string }[]>('list_service_open_tool_bindings', { projectId }),

  /** 用服务绑定的工具打开其工作目录 */
  openWith: (serviceId: string) =>
    invoke<void>('open_service_with_tool', { serviceId }),
};
