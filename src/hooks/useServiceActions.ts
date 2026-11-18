import { useCallback, useState } from 'react';
import { processApi, type Service } from '../services/service';
import { startSingleService, stopSingleService } from '../stores/serviceActions';
import { reportError } from '../utils/error';
import { notify } from '../utils/notify';
import { usePortConflictStore } from '../stores/portConflict';

/**
 * 停止动作的前端兜底超时。
 *
 * 后端本地停止正常在几秒内完成；开机启动器的 stop 命令最长 15 秒。超过 25 秒说明
 * 后端卡住了（taskkill/启动器无响应），不能让卡片按钮**永久**停在 disabled 状态——
 * 用户的感受是"整张卡点不动了"。超时只恢复 UI 并提示，不假装后端已经停掉。
 */
const STOP_TIMEOUT_MS = 25_000;

function withTimeout<T>(p: Promise<T>, ms: number, message: string): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(message)), ms);
    p.then(
      (value) => { clearTimeout(timer); resolve(value); },
      (err) => { clearTimeout(timer); reject(err); },
    );
  });
}

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
 *
 * **是一组 id 而不是单个**（CQ-40）：单值时快速点服务 A 的「启动」再点 B 的，B 会把 A 顶掉；
 * A 先返回就 `setBusyId(null)` 把 B 重新点亮——**B 还在执行中，它的按钮却已可用**，
 * 于是同一个动作可以被重复发起（启动两次 = 两个进程抢同一个端口）。
 */
export function useServiceActions(onDone: () => void) {
  /** 正在执行动作的服务 id 集合：调用方据此禁用按钮/菜单项，避免第二次点击 */
  const [busyIds, setBusyIds] = useState<ReadonlySet<string>>(() => new Set());
  const isBusy = useCallback((id: string) => busyIds.has(id), [busyIds]);

  /** 标记开始/结束。用函数式更新，避免闭包拿到过期的集合 */
  const mark = (id: string, busy: boolean) => {
    setBusyIds(prev => {
      const next = new Set(prev);
      if (busy) next.add(id); else next.delete(id);
      return next;
    });
  };

  const runAction = useCallback(async (service: Service, action: ServiceActionName) => {
    mark(service.id, true);
    try {
      if (action === 'start') {
        // 启动前端口冲突预检：配置了 open_port 或上次监听端口被占时，先让用户决定
        // 「结束占用进程 / 仍然尝试 / 取消」，不要直接 spawn 出来撞 EADDRINUSE。
        // 检查失败不阻断启动（照常继续，真正的失败仍会走原有错误提示）。
        try {
          const conflict = await processApi.checkStartConflict(service.id);
          if (conflict) {
            const proceed = await usePortConflictStore.getState().ask({
              serviceName: service.name,
              conflict,
              actionLabel: '启动服务',
            });
            if (!proceed) return;
          }
        } catch (err: unknown) {
          reportError('检查端口占用失败（继续启动）', err, { variant: 'warning', silent: true });
        }
        // 共享动作层：启动进程 + 追加该服务的文件监听（口径与详情页一致）
        await startSingleService(service.project_id, service.id);
      } else if (action === 'stop') {
        // 停止进程 + 移除监听 + 清日志（后端已清缓冲，前端缓存同步清）。
        // 加前端超时：后端卡住时也要把按钮恢复，不能让整张卡永久 disabled。
        await withTimeout(
          stopSingleService(service.project_id, service.id),
          STOP_TIMEOUT_MS,
          '停止服务超过 25 秒未返回；后端可能仍在清理，按钮已恢复，请稍后刷新或重试',
        );
      } else {
        await processApi.restart(service.id);
      }
      onDone();
    } catch (err: unknown) {
      reportError(`${action === 'start' ? '启动' : action === 'stop' ? '停止' : '重启'}服务失败`, err);
      // 任何失败都刷新：卡片要能显示后端此刻的真实状态（失败/仍在运行/已停止），
      // 特别是停止超时——后端可能还在清理，下一次轮询会纠正显示
      onDone();
    } finally {
      // **必须放 finally**：端口冲突弹框里点「取消」会从 try 里提前 return，
      // 放在 try/catch 之后的 mark(false) 会被跳过——按钮一直灰着，用户再也点不动
      // （实测复现：取消一次后启动按钮永久禁用，只能重开面板）。
      mark(service.id, false);
    }
  }, [onDone]);

  /**
   * 取消跟随日志文件（服务照常运行，只是不再读它的日志文件）。
   *
   * 与启停同为共享动作：两个入口（展开态卡片、收起态圆点条）都要能用，
   * 各写一份的话必然有一处忘了刷新运行状态、菜单里的"取消跟随"项不消失。
   */
  const unfollowLog = useCallback(async (service: Service) => {
    mark(service.id, true);
    try {
      const cancelled = await processApi.unfollowLog(service.id);
      // 没在跟随时后端返回 null：那是空操作，说一句"没在跟随"比弹出"已取消"更诚实
      if (cancelled) {
        notify({ title: '已取消跟随日志', description: cancelled });
      }
      onDone();
    } catch (err: unknown) {
      reportError('取消跟随日志失败', err);
    } finally {
      mark(service.id, false);
    }
  }, [onDone]);

  return { isBusy, runAction, unfollowLog };
}
