import { invoke } from '@tauri-apps/api/core';

export interface HarnessStatus {
  running: boolean;
  /** 带一次性 token 的 dsh web 启动地址（导航后自动换 cookie 跳干净首页） */
  url: string | null;
  pid: number | null;
  /** dsh CLI 是否已存在于 PATH（未安装时前端展示安装引导） */
  dshFound: boolean;
  /** 会话启动时的工作目录（判断会话归属哪个项目；null = 未指定） */
  cwd: string | null;
}

export interface HarnessRunningEvent {
  running: boolean;
  /** 启动成功事件附带 URL（失败/停止时为空） */
  url?: string | null;
}

export interface HarnessErrorEvent {
  error: string;
}

export interface HarnessDshInstalledEvent {
  ok: boolean;
  error?: string;
}

export const harnessService = {
  start: (cwd: string | null) => invoke<void>('harness_start', { cwd }),

  status: () => invoke<HarnessStatus>('harness_status'),

  /** 后台执行 npm i -g @deepseek-ai/dsh；结果经 harness-dsh-installed 事件/轮询反映 */
  install: () => invoke<void>('harness_install'),

  stop: () => invoke<void>('harness_stop'),
};
