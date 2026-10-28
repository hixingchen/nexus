import { useEffect } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import {
  AI_RUNTIME_TRANSFER_PROGRESS_EVENT,
  type RuntimeTransferProgress,
} from '../services/aiService';
import { useAiRuntimeTransferStore } from '../stores/aiRuntimeTransferStore';
import { reportError } from '../utils/error';

/**
 * 订阅 AI 运行时导出 / 导入进度，写进全局 store。
 *
 * 必须挂在**常驻组件**（App）上，而不是设置面板里：面板关掉后传输仍在后端跑，
 * 进度要持续落进 store，面板重新打开时才能接着看到同一次传输。
 */
export function useAiRuntimeTransferEvents() {
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let alive = true;
    void listen<RuntimeTransferProgress>(AI_RUNTIME_TRANSFER_PROGRESS_EVENT, (ev) => {
      useAiRuntimeTransferStore.getState().setProgress(ev.payload);
    })
      .then((fn) => {
        if (alive) unlisten = fn;
        else fn();
      })
      .catch((e) => reportError('订阅 AI 运行时传输进度失败', e));
    return () => {
      alive = false;
      unlisten?.();
    };
  }, []);
}
