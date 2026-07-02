import { invoke } from '@tauri-apps/api/core';

/// 单条服务日志
export interface ServiceLogLine {
  timestamp: string;
  stream: 'stdout' | 'stderr';
  text: string;
}

/// 服务日志实时事件（由后端推送）
export interface ServiceLogEvent {
  service_key: string;
  stream: 'stdout' | 'stderr';
  data: string;
}

export const logService = {
  /** 获取某服务的已缓冲日志 */
  getServiceLogs: (serviceKey: string) =>
    invoke<ServiceLogLine[]>('get_service_logs', { service_key: serviceKey }),
};
