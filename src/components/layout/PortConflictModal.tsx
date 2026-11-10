import { useEffect, useState } from 'react';
import { Modal } from '../ui/Modal';
import { processApi } from '../../services/service';
import { reportError, toMessage } from '../../utils/error';
import { usePortConflictStore } from '../../stores/portConflict';
import { portConflictSummary } from '../../utils/serviceFailure';

/**
 * 端口冲突确认弹框（启动前预检 / 失败卡片共用）。
 *
 * 只做三件事：把占用者说清楚、提供「打开目录」定位、在用户明确点击后结束占用进程。
 * 真正的启停动作由调用方在 `ask()` resolve 之后执行——弹框不持有服务动作逻辑，
 * 避免「弹框里也能启动」与「卡片按钮也能启动」两套入口。
 */
export function PortConflictModal() {
  const pending = usePortConflictStore(s => s.pending);
  const settle = usePortConflictStore(s => s.settle);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // 换了一次冲突请求（或弹框关闭）就清掉上一次的"处理中/失败原因"：
  // 组件不会随 pending 卸载，留着会让下一条冲突一打开就顶着上一条的报错
  useEffect(() => {
    setBusy(false);
    setError(null);
  }, [pending]);

  if (!pending) return null;
  const { conflict, serviceName, actionLabel = '启动服务' } = pending;
  const killable = conflict.occupants.filter(o => !o.is_self);

  const handleClose = () => {
    // 正在杀进程时不允许关：关掉会让调用方的 await 以为用户取消，而枪已经响了
    if (!busy) settle(false);
  };

  const handleKill = async () => {
    if (killable.length === 0) return;
    setBusy(true);
    setError(null);
    try {
      for (const o of killable) {
        await processApi.killPortOccupant(conflict.port, o.pid);
      }
      settle(true);
    } catch (e) {
      reportError('结束端口占用进程失败', e);
      setError(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal open title="端口已被占用" onClose={handleClose} width="480px">
      <div className="space-y-3">
        <p className="text-[13px] text-nexus-text">
          服务「{serviceName}」要用的端口{' '}
          <span className="font-mono text-nexus-warning">{conflict.port}</span>{' '}
          已被占用：
        </p>

        <div className="max-h-[180px] overflow-y-auto rounded border border-nexus-border bg-nexus-bg/30">
          {conflict.occupants.map((o, i) => (
            <div
              key={o.pid}
              className={`flex items-start gap-2 px-2.5 py-2 text-[12px] ${
                i > 0 ? 'border-t border-nexus-border/40' : ''
              }`}
            >
              <div className="flex-1 min-w-0">
                <div className="text-nexus-text">
                  <span className="font-medium">{o.name}</span>
                  <span className="ml-2 font-mono text-[11px] text-nexus-muted">PID {o.pid}</span>
                  {o.is_self && (
                    <span className="ml-2 text-[10.5px] text-nexus-warning">（Nexus 自身，不能结束）</span>
                  )}
                </div>
                {o.path && (
                  <div className="mt-0.5 break-all font-mono text-[10.5px] text-nexus-muted/70">
                    {o.path}
                  </div>
                )}
              </div>
              {o.path && (
                <button
                  className="flex-shrink-0 rounded px-2 py-0.5 text-[11px] text-nexus-muted hover:bg-nexus-hover/50 hover:text-nexus-text disabled:opacity-40 disabled:cursor-not-allowed"
                  disabled={busy}
                  // 路径由后端按 PID 重新解析：前端不传路径，避免绕过文件白名单
                  onClick={() => void processApi.openPortOccupant(conflict.port, o.pid)
                    .catch(e => reportError('打开占用者目录失败', e))}
                >打开目录</button>
              )}
            </div>
          ))}
        </div>

        <p className="text-[11.5px] leading-relaxed text-nexus-muted">
          结束占用进程会立即终止它（含子进程），不会自动重启。确认是不要的程序再操作；
          如果是别的工具或另一个 Nexus 正在运行，请改用「仍然尝试」或先手动停止它。
        </p>

        {error && (
          <p className="break-words text-[12px] text-nexus-error">操作失败：{error}</p>
        )}

        <div className="flex items-center justify-end gap-2">
          <button
            className="rounded px-4 py-1.5 text-[12px] text-nexus-text-muted hover:bg-nexus-hover/50 hover:text-nexus-text disabled:opacity-40"
            disabled={busy}
            onClick={() => settle(false)}
          >取消</button>
          <button
            className="rounded px-4 py-1.5 text-[12px] text-nexus-text bg-nexus-surface hover:bg-nexus-hover/60 disabled:opacity-40"
            disabled={busy}
            onClick={() => settle(true)}
          >仍然尝试</button>
          {killable.length > 0 && (
            <button
              className="rounded px-4 py-1.5 text-[12px] text-nexus-error border border-nexus-error/40 hover:bg-nexus-error/10 disabled:opacity-40 disabled:cursor-not-allowed"
              disabled={busy}
              onClick={() => void handleKill()}
            >
              {busy
                ? '处理中…'
                : `${killable.length > 1 ? `结束 ${killable.length} 个占用进程` : '结束占用进程'}并${actionLabel}`}
            </button>
          )}
        </div>

        <p className="sr-only">{portConflictSummary(conflict)}</p>
      </div>
    </Modal>
  );
}
