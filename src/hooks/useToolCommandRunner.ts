import { useCallback } from 'react';
import { listen } from '@tauri-apps/api/event';
import { processApi, type ToolCommandLogBatchPayload } from '../services/service';
import { useToolLogStore } from '../stores/toolLogStore';
import { useToolCommandStore } from '../stores/toolCommandStore';
import { showNotification } from '../components/ui/Toast';
import { reportError } from '../utils/error';

/**
 * 工具命令执行（流式输出 + 停止）。
 *
 * 运行状态放在全局 `toolCommandStore`（按 run_id 分桶）而不是本 hook 的 useState：
 * 弹窗关闭后命令仍在后台跑，状态必须活得更久；多条命令并发时也不能互相覆盖结果
 * （详见 store 里的说明）。
 *
 * 输出行**不进 React state**：按 `run_id` 存在 `stores/toolLogStore`（50ms 合帧）。
 * 放在 state 里会让每行输出都重渲染整个窗格（构建日志每秒上百行时直接卡死）。
 */

/** 启动一条工具命令（由服务右键菜单 / 卡片入口调用） */
export function useRunToolCommand() {
  return useCallback(async (serviceId: string, commandId: string, commandName: string) => {
    const store = useToolCommandStore.getState();

    // 同一条命令已在跑（用户把弹窗关到后台后又点了一次）：**不再起第二个进程**，
    // 直接把已有运行打开。并行两份 npm install / mvn 会互踩文件锁和构建产物
    const existing = store.runningByCommand(serviceId, commandId);
    if (existing) {
      store.open(existing.runId);
      showNotification({
        variant: 'info',
        title: `「${existing.commandName}」正在运行`,
        description: '已打开执行窗口，无需重复执行',
        duration: 4000,
      });
      return;
    }

    const runId = crypto.randomUUID();
    useToolLogStore.getState().reset(runId);
    store.begin({ runId, serviceId, commandId, commandName });

    // 订阅放在 try 内：订阅失败也必须让弹窗脱离 loading（否则停在"等待执行…"，
    // 只能重启应用），且不能留下未处理的 rejection
    let unlisten: (() => void) | null = null;
    try {
      unlisten = await listen<ToolCommandLogBatchPayload>('tool-command-log-batch', event => {
        if (event.payload.run_id !== runId) return;
        // 只入缓冲，不做任何 React 工作（合帧由 store 负责）。
        // 后端已按 50ms 批量，所以这里是一次事件一批行，而不是一行一事件
        useToolLogStore.getState().append(runId, event.payload.lines.map(l => l.data));
      });
      const result = await processApi.runToolCommand(serviceId, commandId, runId);
      const before = useToolCommandStore.getState();
      const backgrounded = before.openRunId !== runId;
      const stoppedByUser = before.runs[runId]?.stopping ?? false;
      before.finish(runId, result);
      // 后台跑完：用户看不见结果，必须主动告知；「查看输出」可把弹窗再打开
      if (backgrounded && !stoppedByUser) {
        showNotification({
          variant: result.success ? 'success' : 'error',
          title: `工具命令「${commandName}」${result.success ? '执行完成' : '执行失败'}`,
          description: result.success
            ? '已在后台完成'
            : `退出码 ${result.exit_code ?? '未知'}（点「查看输出」看详情）`,
          duration: result.success ? 5000 : 10000,
          action: { label: '查看输出', run: () => useToolCommandStore.getState().open(runId) },
        });
      }
    } catch (err) {
      reportError('执行工具命令失败', err);
      const before = useToolCommandStore.getState();
      const backgrounded = before.openRunId !== runId;
      const stoppedByUser = before.runs[runId]?.stopping ?? false;
      // 失败原因写进 run：弹窗关着时靠通知的「查看输出」把它再展示出来
      before.fail(runId, String(err));
      if (backgrounded && !stoppedByUser) {
        showNotification({
          variant: 'error',
          title: `工具命令「${commandName}」执行失败`,
          description: String(err),
          duration: 10000,
          action: { label: '查看输出', run: () => useToolCommandStore.getState().open(runId) },
        });
      }
    } finally {
      unlisten?.();
    }
  }, []);
}

/**
 * 当前弹窗展示的运行 + 停止/关闭动作（由 MainLayout 的全局弹窗宿主使用）。
 *
 * 与启动分开两个 hook：ProjectDetail 只启动、不订阅弹窗状态，避免开/关弹窗把整个
 * 详情页（标签栏 + 编辑器 + 服务卡 + 模板卡）重渲染一遍。
 */
export function useToolCommandDialog() {
  const state = useToolCommandStore(s => (s.openRunId ? s.runs[s.openRunId] ?? null : null));

  /** 终止进程树。返回是否成功——「停止并关闭」要等它成功后才关窗 */
  const stop = useCallback(async (): Promise<boolean> => {
    const store = useToolCommandStore.getState();
    const runId = store.openRunId;
    if (!runId || !store.runs[runId]?.loading) return false;
    // 先标记：命令随后会以失败退出码返回，别把"用户主动停止"再报成一次失败通知
    store.markStopping(runId, true);
    try {
      await processApi.stopToolCommand(runId);
      showNotification({ title: '已停止工具命令', duration: 2000 });
      return true;
    } catch (e) {
      store.markStopping(runId, false);
      reportError('停止失败', e);
      return false;
    }
  }, []);

  const close = useCallback(() => useToolCommandStore.getState().closeDialog(), []);

  return { state, stop, close };
}
