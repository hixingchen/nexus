import { useCallback, useState } from 'react';
import { processApi, type Service } from '../services/service';
import { startSingleService, stopSingleService } from '../stores/serviceActions';
import { reportError } from '../utils/error';

/** 服务动作：卡片按钮与右键菜单共同的三档操作 */
export type ServiceActionName = 'start' | 'stop' | 'restart';

/**
 * 服务动作的共享实现（启动 / 停止 / 重启）。
 *
 * 为什么抽出来：这些动作有两个入口——服务卡片（`ServiceTreeEntry`）与服务列收起后的圆点条
 * （`ProjectDetail` 的 `CollapsedView`）。各写一份的后果与 `stores/serviceActions.ts` 当初
 * 收口时同源：差异只会出现在"某个入口忘了同步文件监听 / 忘了清日志 / 失败后忘了刷新"
 * 这类**只在收起态复现**的隐蔽故障上。
 *
 * `busyId` 是 id 而不是布尔：收起态那一列圆点要按服务分别禁用，卡片那边用
 * `busyId === service.id` 即可，两种用法同一个来源。
 */
export function useServiceActions(onDone: () => void) {
  /** 正在执行动作的服务 id：调用方据此禁用按钮/菜单项，避免第二次点击 */
  const [busyId, setBusyId] = useState<string | null>(null);

  const runAction = useCallback(async (service: Service, action: ServiceActionName) => {
    setBusyId(service.id);
    try {
      if (action === 'start') {
        // 共享动作层：启动进程 + 追加该服务的文件监听（口径与详情页一致）
        await startSingleService(service.project_id, service.id);
      } else if (action === 'stop') {
        // 停止进程 + 移除监听 + 清日志（后端已清缓冲，前端缓存同步清）
        await stopSingleService(service.project_id, service.id);
      } else {
        await processApi.restart(service.id);
      }
      onDone();
    } catch (err: unknown) {
      reportError(`${action === 'start' ? '启动' : action === 'stop' ? '停止' : '重启'}服务失败`, err);
      // 启动失败：后端已记失败状态（spawn 失败），刷新让卡片显示"失败"按钮
      if (action === 'start') onDone();
    }
    setBusyId(null);
  }, [onDone]);

  return { busyId, runAction };
}
