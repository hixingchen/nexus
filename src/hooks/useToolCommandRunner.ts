import { useCallback, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { processApi, type ToolCommandResult, type ToolCommandLogBatchPayload } from '../services/service';
import { useToolLogStore } from '../stores/toolLogStore';
import { showNotification } from '../components/ui/Toast';
import { reportError } from '../utils/error';

/** 工具命令执行弹窗的状态（输出行不在这里，见下方说明） */
interface ToolCommandState {
  open: boolean;
  loading: boolean;
  commandName: string;
  runId: string;
  result: ToolCommandResult | null;
  error: string | null;
}

const CLOSED: ToolCommandState = { open: false, loading: false, commandName: '', runId: '', result: null, error: null };

/**
 * 工具命令执行（流式输出 + 停止）。
 *
 * 从 ProjectDetail 抽出来（ARCH-7 ②）：这块是"执行一条工具命令"的完整生命周期——
 * 订阅事件流、跑命令、兜底完整结果、失败态、手动停止，与组件布局无关；留在 800 行的
 * 详情组件里既难测也容易被无关改动碰到。
 *
 * 输出行**不进 React state**：按 `run_id` 存在 `stores/toolLogStore`（50ms 合帧）。
 * 放在这里的 state 里会让每行输出都重渲染整个窗格（构建日志每秒上百行时直接卡死）。
 */
export function useToolCommandRunner() {
  const [state, setState] = useState<ToolCommandState>(CLOSED);
  /** stop/close 需要读"当下"的状态，但回调身份必须稳定（依赖 state 会让每次输出都换身份） */
  const stateRef = useRef(state);
  stateRef.current = state;

  const run = useCallback(async (serviceId: string, commandId: string, commandName: string) => {
    const runId = crypto.randomUUID();
    useToolLogStore.getState().reset(runId);
    setState({ open: true, loading: true, commandName, runId, result: null, error: null });

    // 订阅放在 try 内：订阅失败也必须让弹窗脱离 loading（否则停在"等待执行…"，只能重启应用），
    // 且不能留下未处理的 rejection
    let unlisten: (() => void) | null = null;
    try {
      unlisten = await listen<ToolCommandLogBatchPayload>('tool-command-log-batch', event => {
        if (event.payload.run_id !== runId) return;
        // 只入缓冲，不做任何 React 工作（合帧由 store 负责）。
        // 后端已按 50ms 批量，所以这里是一次事件一批行，而不是一行一事件
        useToolLogStore.getState().append(runId, event.payload.lines.map(l => l.data));
      });
      const result = await processApi.runToolCommand(serviceId, commandId, runId);
      // 以完整结果兜底（含按序号合并的顺序，避免事件流微乱序）；行数与事件流同口径
      setState(prev => ({ ...prev, loading: false, result, error: null }));
    } catch (err) {
      reportError('执行工具命令失败', err);
      // 失败也要让弹窗脱离 loading 态并把原因写进弹窗（原实现只有 3 秒 toast，弹窗停在"等待执行…"）
      setState(prev => ({ ...prev, loading: false, error: String(err) }));
    } finally {
      unlisten?.();
    }
  }, []);

  /** 停止正在执行的命令（终止进程树）：命令随后以失败退出码正常返回并展示 */
  const stop = useCallback(async () => {
    const { runId } = stateRef.current;
    if (!runId) return;
    try {
      await processApi.stopToolCommand(runId);
      showNotification({ title: '已停止工具命令', duration: 2000 });
    } catch (e) {
      reportError('停止失败', e);
    }
  }, []);

  const close = useCallback(() => setState(CLOSED), []);

  return { state, run, stop, close };
}
