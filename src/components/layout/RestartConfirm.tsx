import { useState, useEffect, useCallback } from 'react';
import { listen } from '@tauri-apps/api/event';
import { processApi, type FileChangeEvent } from '../../services/service';
import { showNotification } from '../ui/Toast';

type ConfirmItem = {
  serviceId: string;
  serviceName: string;
  count: number;
  restarting: boolean;
};

const MAX_RESTART_ITEMS = 5;

export function RestartConfirm() {
  const [items, setItems] = useState<ConfirmItem[]>([]);

  useEffect(() => {
    const unlisten = listen<FileChangeEvent>('file-changed', (event) => {
      const { changes } = event.payload;
      for (const c of changes) {
        if (c.service_name === '(project)' || !c.service_id) continue;
        const name = c.service_name;
        const sid = c.service_id;
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
    });
    return () => { unlisten.then(fn => fn()); };
  }, []);

  const handleRestart = useCallback(async (item: ConfirmItem) => {
    setItems(prev => prev.map(i =>
      i.serviceName === item.serviceName ? { ...i, restarting: true } : i
    ));
    try {
      await processApi.restart(item.serviceId);
    } catch (e) { console.error('重启服务失败:', e); showNotification({ variant: 'error', title: '重启服务失败', description: String(e) }); }
    setItems(prev => prev.filter(i => i.serviceName !== item.serviceName));
  }, []);

  const handleDismiss = useCallback((serviceName: string) => {
    setItems(prev => prev.filter(i => i.serviceName !== serviceName));
  }, []);

  if (items.length === 0) return null;

  return (
    <div className="fixed bottom-10 right-6 z-[80] flex flex-col-reverse gap-3">
      {items.map(item => (
        <div key={item.serviceName}
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
            className="p-1.5 text-nexus-muted/50 hover:text-nexus-text rounded-md hover:bg-nexus-hover/50 flex-shrink-0"
            onClick={() => handleDismiss(item.serviceName)}
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
