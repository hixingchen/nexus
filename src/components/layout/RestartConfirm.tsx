import { useState, useEffect, useCallback } from 'react';
import { listen } from '@tauri-apps/api/event';
import { processApi, type FileChangeEvent } from '../../services/service';
import { reportError } from '../../utils/error';
import { notify } from '../../utils/notify';
import { createAutoRestarter } from '../../utils/autoRestart';
import { useRunningStore } from '../../stores/runningStore';
import { useAiPanelReserve } from '../../hooks/useAiPanelReserve';

type ConfirmItem = {
  serviceId: string;
  serviceName: string;
  count: number;
  restarting: boolean;
};

const MAX_RESTART_ITEMS = 5;

export function RestartConfirm() {
  const [items, setItems] = useState<ConfirmItem[]>([]);
  /** AI 面板占用右侧：卡片必须让位——原生子 WebView 盖在所有 DOM 之上，
   *  卡片若落在面板区域内会既看不见也点不到（z-index 对原生层无效）。
   *  口径收在 useAiPanelReserve（与通知/右键菜单同一处），随面板宽度变化自动跟随 */
  const aiPanelReserve = useAiPanelReserve();
  const cardRight = aiPanelReserve + 24;

  useEffect(() => {
    /**
     * 自动重启的决策逻辑在 `utils/autoRestart.ts`（可单测，见它的说明）；这里只把它接到
     * 真实的重启命令、运行状态与提示上。
     *
     * 两处不能省：
     * - `probe` 必须先 `refresh()` 拿**权威状态**再判定。`unknown`（还在启动）与
     *   `failed`（秒退）的处理完全相反，拿 3 秒前的轮询缓存下结论会把两者判反；
     * - `onGiveUp` 是唯一会打扰用户的时刻（中间的重试是静默的，否则改代码时会刷屏）。
     *
     * 这个状态机的定时器活到组件卸载之后（`RestartConfirm` 挂在 `MainLayout` 上，
     * 与应用同寿命；卸载即退出，剩下的定时器打到的是模块级函数，不会碰已卸载的 DOM）。
     */
    const restarter = createAutoRestarter({
      restart: (sid) => processApi.restart(sid),
      probe: async (sid) => {
        await useRunningStore.getState().refresh();
        const st = useRunningStore.getState();
        if (st.failed.some(f => f.service_id === sid)) return 'failed';
        if (st.running.some(r => r.service_id === sid)) return 'running';
        return 'unknown';
      },
      sleep: (ms) => new Promise<void>((r) => setTimeout(r, ms)),
      onGiveUp: ({ serviceName, attempts }) => {
        notify({
          variant: 'error',
          title: `服务「${serviceName}」自动重启未成功`,
          description:
            `已连续尝试 ${attempts} 次。失败那次的输出保留在日志面板里（点服务卡片上的「失败」），`
            + `多半是端口没释放或代码当时还没写完——稍后手动重启一次通常就好了。`,
          duration: 12000,
        });
      },
      onError: (sid, e) => reportError(`自动重启服务失败（服务 ${sid}）`, e),
    });

    let disposed = false;
    let unlisten: (() => void) | null = null;
    listen<FileChangeEvent>('file-changed', (event) => {
      if (disposed) return;
      const { changes } = event.payload;
      for (const c of changes) {
        if (c.service_name === '(project)' || !c.service_id) continue;
        // 0=关闭监听：后端已不为其生成事件（此处兜底旧 watcher 残留/异常事件）
        if (c.restart_mode === 0) continue;
        const name = c.service_name;
        const sid = c.service_id;

        // 自动重启模式：不弹确认框，交给状态机（节流 + 尾沿去抖 + 失败重试）。
        // **不能在这里判断"跳过"**——状态机自己会合并同一服务的连续变更，
        // 外面再丢一次就等于把"最后一次改动"丢了（原实现就是这么丢的）
        if (c.restart_mode === 2) {
          restarter.change(sid, name);
          continue;
        }

        setItems(prev => {
          const existing = prev.find(i => i.serviceId === sid);
          if (existing) {
            return prev.map(i =>
              i.serviceId === sid ? { ...i, count: i.count + 1 } : i
            );
          }
          // 限制最大通知数量，超出时移除最早的
          const next = [...prev, {
            serviceId: sid,
            serviceName: name,
            count: 1,
            restarting: false,
          }];
          if (next.length > MAX_RESTART_ITEMS) next.shift();
          return next;
        });
      }
    }).then(fn => {
      if (disposed) { fn(); return; }
      unlisten = fn;
    }).catch((e) => {
      // 订阅失败 = "改动文件后提示重启"整体失效，用户只会觉得"改了没反应"
      reportError('订阅 file-changed 失败', e, {
        title: '文件变更订阅失败',
        description: '监听文件发生变化时将不再提示重启，请重启应用',
      });
    });
    return () => { disposed = true; unlisten?.(); };
  }, []);

  const handleRestart = useCallback(async (item: ConfirmItem) => {
    setItems(prev => prev.map(i =>
      i.serviceId === item.serviceId ? { ...i, restarting: true } : i
    ));
    try {
      await processApi.restart(item.serviceId);
    } catch (e) { reportError('重启服务失败', e); }
    setItems(prev => prev.filter(i => i.serviceId !== item.serviceId));
  }, []);

  const handleDismiss = useCallback((serviceId: string) => {
    setItems(prev => prev.filter(i => i.serviceId !== serviceId));
  }, []);

  if (items.length === 0) return null;

  return (
    <div
      className="fixed bottom-10 z-[80] flex flex-col-reverse gap-3"
      // right 动态让位 AI 面板（见 aiPanelReserve 注释）：面板宽度可拖拽变化，订阅 store 自动跟随
      style={{ right: cardRight }}
    >
      {items.map(item => (
        <div key={item.serviceId}
          className="flex items-center gap-4 bg-nexus-surface border border-nexus-border rounded-xl shadow-2xl pl-5 pr-3 py-3.5 min-w-[340px] max-w-[420px]"
        >
          <svg width="18" height="18" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.3"
            className="text-nexus-warning flex-shrink-0">
            <circle cx="7" cy="7" r="5"/><path d="M7 4v3.5L9 9"/>
          </svg>

          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-2">
              <span className="text-[14px] text-nexus-text font-semibold truncate">{item.serviceName}</span>
              {item.count > 1 && (
                <span className="text-[11px] bg-nexus-warning/20 text-nexus-warning px-2 py-0.5 rounded-full font-semibold flex-shrink-0">
                  {item.count}
                </span>
              )}
            </div>
            <p className="text-[12px] text-nexus-muted mt-0.5">检测到文件变更，需要重启服务</p>
          </div>

          <button
            className="px-5 py-2 text-[13px] bg-nexus-accent text-white rounded-lg hover:bg-nexus-accent-hover disabled:opacity-40 font-semibold flex-shrink-0 shadow-sm"
            disabled={item.restarting}
            onClick={() => handleRestart(item)}
          >{item.restarting ? '重启中…' : '重启服务'}</button>

          <button
            aria-label="忽略这条重启提示"
            className="p-1.5 text-nexus-muted/50 hover:text-nexus-text rounded-md hover:bg-nexus-hover/50 flex-shrink-0"
            onClick={() => handleDismiss(item.serviceId)}
          >
            <svg width="12" height="12" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.5">
              <line x1="2" y1="2" x2="8" y2="8"/><line x1="8" y1="2" x2="2" y2="8"/>
            </svg>
          </button>
        </div>
      ))}
    </div>
  );
}
