import { invoke } from '@tauri-apps/api/core';

/// 日志来源：stdout/stderr 为进程输出，system 为生命周期标记（启动失败/退出码/停止）
export type LogStream = 'stdout' | 'stderr' | 'system';

/// 单条服务日志
export interface ServiceLogLine {
  timestamp: string;
  stream: LogStream;
  text: string;
}

/// 服务日志实时事件（由后端推送）
export interface ServiceLogEvent {
  service_key: string;
  stream: LogStream;
  data: string;
  /** 行产生时间（RFC3339，后端打点） */
  timestamp: string;
}

export const logService = {
  /** 获取某服务的已缓冲日志 */
  getServiceLogs: (serviceKey: string) =>
    invoke<ServiceLogLine[]>('get_service_logs', { serviceKey }),
};
