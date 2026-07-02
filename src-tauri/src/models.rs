use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    pub pinned: bool,
    pub sort_index: i32,
    pub terminal_init_command: String,
}

/// 工具命令定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCommand {
    pub id: String,
    pub name: String,
    pub command: String,
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

/// 项目详情：项目信息 + 其下所有服务
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectDetail {
    pub project: Project,
    pub services: Vec<Service>,
}
