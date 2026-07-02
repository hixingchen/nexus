use serde::{Deserialize, Serialize};

/// 终端配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalConfig {
    /// 终端类型
    pub term_type: String,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            term_type: "xterm-256color".to_string(),
        }
    }
}
