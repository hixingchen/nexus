import { invoke } from '@tauri-apps/api/core';

/// 日志来源：stdout/stderr 为进程输出，system 为生命周期标记（启动失败/退出码/停止），
/// file 为跟随服务自己写的日志文件（服务用 `start` 另开窗口跑时，stdout 拿不到内容）
export type LogStream = 'stdout' | 'stderr' | 'system' | 'file';

/// 单条服务日志
export interface ServiceLogLine {
  /** 服务内单调行号：快照与实时事件靠它做幂等合并（见 stores/logStore 的 lastSeq） */
  seq: number;
  timestamp: string;
  stream: LogStream;
  text: string;
}

/// 服务日志批量事件（由后端每 ~50ms 推送一批；取代原来的逐行事件）
export interface ServiceLogBatchEvent {
  service_key: string;
  lines: ServiceLogLine[];
}

export const logService = {
  /** 获取某服务的已缓冲日志 */
  getServiceLogs: (serviceKey: string) =>
    invoke<ServiceLogLine[]>('get_service_logs', { serviceKey }),
};
