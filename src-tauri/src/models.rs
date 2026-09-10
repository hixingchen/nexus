use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    pub pinned: bool,
    pub sort_index: i32,
}

/// 工具命令定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCommand {
    pub id: String,
    pub name: String,
    pub command: String,
    /// 执行超时（秒）：缺省 = 默认 60 秒；0 = 不限制（长构建类命令）；
    /// 正数 = 该秒数。超时会终止进程树（见 commands::process::run_tool_command）
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// 外部打开工具（服务右键「用 XX 打开」）
///
/// 新模型：executable（程序路径）+ args（参数模板，`{path}` 参数化注入，不经过 shell）。
/// command 为已废弃的整串命令（仅历史行使用：executable 为空时后端按旧格式执行）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenTool {
    pub id: String,
    pub name: String,
    pub command: String,
    pub executable: String,
    pub args: String,
}

/// 服务 ↔ 打开工具绑定
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceOpenToolBinding {
    pub service_id: String,
    pub tool_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Service {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub command: String,
    pub cwd: String,
    pub watch_paths: String,    // JSON array, e.g. ["./src", "./lib"]
    pub watch_include: String,  // glob patterns, one per line, e.g. *\n*.ts
    pub watch_exclude: String,  // patterns to exclude, one per line
    pub env_vars: String,       // KEY=VALUE per line (dotenv format)
    pub restart_mode: i32,  // 0=关闭监听, 1=确认重启, 2=自动重启
    pub enabled: bool,
    pub show_file_tree: bool,
    pub sort_index: i32,
    pub tool_commands: String,  // JSON array of ToolCommand
}

/// 服务模板：跨项目复用的服务配置（独立于项目，复制到项目时值拷贝）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceTemplate {
    pub id: String,
    pub name: String,
    pub command: String,
    pub cwd: String,
    pub watch_paths: String,
    pub watch_include: String,
    pub watch_exclude: String,
    pub env_vars: String,
    pub restart_mode: i32,
    pub enabled: bool,
    pub show_file_tree: bool,
    pub tool_commands: String,
    /// 模板携带的默认打开工具（从模板添加服务时复制为新服务的绑定）
    pub open_tool_id: String,
    pub created_at: String,
}

/// 项目详情：项目信息 + 其下所有服务
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectDetail {
    pub project: Project,
    pub services: Vec<Service>,
}
