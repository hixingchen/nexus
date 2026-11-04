import { useState } from 'react';
import { openInExplorer, openWithDefaultApp } from '../../services/system';
import { toMessage } from '../../utils/message';

/**
 * 旧版 `.doc`（OLE 复合文档）不内嵌渲染：交给系统默认程序。
 *
 * 不引入 Word COM / LibreOffice 自动转换——那是启动外部重进程、可能弹窗、可能挂起，
 * 为一个只读预览付的代价远大于收益。
 */
export function DocFallback({ path, name }: { path: string; name: string }) {
  const [opening, setOpening] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const openWithDefault = async () => {
    setOpening(true);
    setError(null);
    try {
      await openWithDefaultApp(path);
    } catch (e) {
      // 常见原因是该机器没有装 Word/WPS（ShellExecute 返回 31「没有关联程序」）：
      // 把后端原文留在页面上，用户才知道是缺程序而不是 Nexus 坏了
      setError(toMessage(e));
    } finally {
      setOpening(false);
    }
  };

  return (
    <div className="h-full flex items-center justify-center bg-nexus-editor p-6">
      <div className="max-w-[520px] w-full rounded-lg border border-nexus-border bg-nexus-surface p-6 space-y-4">
        <div className="space-y-1">
          <p className="text-[13px] text-nexus-text font-medium truncate" title={path}>{name}</p>
          <p className="text-[12px] text-nexus-text-muted leading-relaxed">
            旧版 <span className="text-nexus-warning">.doc</span> 格式不支持内嵌预览，请用 Word / WPS 打开。
          </p>
          <p className="text-[11px] text-nexus-muted">
            新版 .docx 可以直接在 Nexus 里预览。
          </p>
        </div>

        {error && (
          <p className="text-[12px] text-nexus-error break-words">{error}</p>
        )}

        <div className="flex items-center gap-2">
          <button
            className="px-4 py-1.5 text-[12px] bg-nexus-accent text-white rounded-md hover:bg-nexus-accent-hover disabled:opacity-40"
            disabled={opening}
            onClick={() => void openWithDefault()}
          >{opening ? '正在打开…' : '用系统默认程序打开'}</button>
          <button
            className="px-3 py-1.5 text-[12px] text-nexus-text-muted hover:text-nexus-text rounded-md hover:bg-nexus-hover/50"
            onClick={() => void openInExplorer(path)}
          >在资源管理器中显示</button>
        </div>
      </div>
    </div>
  );
}
