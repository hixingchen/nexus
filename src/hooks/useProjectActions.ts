import { useCallback, useState } from 'react';
import { projectApi } from '../services/service';
import { useSvcCacheStore } from '../stores/svcCacheStore';
import { startProjectWithFeedback, stopProjectWithFeedback } from '../stores/serviceActions';
import { reportError } from '../utils/error';

/**
 * 项目级动作的共享实现（启动 / 停止 / 收藏）。
 *
 * 为什么单独抽出来：这些动作有两个**必须保持一致**的入口——左侧完整列表（`ProjectList`）
 * 与它收起后的窄轨（`ProjectRail`）。各写一份的后果与 `stores/serviceActions.ts` 当初收口时
 * 同源：差异只会出现在"某个入口忘了开项目级文件监听""窄轨上停止没清日志"这类
 * **只在收起态复现**的隐蔽故障上（而不是两处都看得见的明显错）。
 * 菜单项的措辞与禁用口径由 `ProjectContextMenu` 保证，动作口径由这里保证。
 *
 * 详情页的"启动/停止全部"（`useProjectDetail`）不并入：它用的是不带反馈的 serviceActions
 * 版本，通知措辞与刷新目标（详情 vs 列表）都不同，硬合并只会把差异挤进参数里。
 *
 * 项目列表的刷新不在这里做：两个入口刷新的是各自的数据（列表走 `useProjectList.load`，
 * 窄轨走它自己的 state），经 `onProjectsChanged` 交回调用点。收藏会改变排序
 * （后端 `ORDER BY pinned DESC`），所以刷新是必须的。
 */
export function useProjectActions(onProjectsChanged?: () => void) {
  /**
   * 正在启停的项目 id 集合。
   *
   * 不能只存单个 id：列表/窄轨上可以先后对 A、B 发起启动，A 先返回时若 `set(null)` 会把
   * B 的忙碌态一并清掉，B 的按钮重新可点，能再次发起启动/停止（CQ-40 在服务动作层的同类问题）。
   */
  const [actingIds, setActingIds] = useState<ReadonlySet<string>>(() => new Set());
  const begin = useCallback((id: string) => {
    setActingIds(prev => {
      const next = new Set(prev);
      next.add(id);
      return next;
    });
  }, []);
  const end = useCallback((id: string) => {
    setActingIds(prev => {
      if (!prev.has(id)) return prev;
      const next = new Set(prev);
      next.delete(id);
      return next;
    });
  }, []);

  const start = useCallback(async (id: string, name: string) => {
    begin(id);
    try {
      // 共享动作层：启动 + 项目级监听 + 运行状态刷新 + 逐服务失败汇总
      await startProjectWithFeedback(id, name);
    } finally {
      end(id);
    }
  }, [begin, end]);

  const stop = useCallback(async (id: string, name: string) => {
    begin(id);
    try {
      // 停止的日志清单取自共享服务缓存（与详情页同一口径：缓存里就是这个项目的服务数组）
      await stopProjectWithFeedback(id, name, useSvcCacheStore.getState().cache[id] ?? []);
    } finally {
      end(id);
    }
  }, [begin, end]);

  const togglePin = useCallback(async (id: string) => {
    try {
      await projectApi.togglePin(id);
      onProjectsChanged?.();
    } catch (e: unknown) {
      reportError('收藏项目失败', e);
    }
  }, [onProjectsChanged]);

  return { actingIds, start, stop, togglePin };
}
