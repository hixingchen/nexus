import { useEffect, useRef, useState } from 'react';
import type { ToolCommandResult } from '../../services/service';
import { useUiStore } from '../../stores/uiStore';

// 常见退出码 → 中文含义（Unix 约定），未识别返回 null
function describeExitCode(code: number | null): string | null {
  if (code === null) return null;
  const known: Record<number, string> = {
    1: '通用错误',
    2: '用法错误',
    126: '无法执行',
    127: '命令未找到',
    130: '已中断（Ctrl+C）',
  };
  if (known[code]) return known[code];
  // 128+N：被信号 N 终止
  if (code > 128 && code < 192) return `被信号 ${code - 128} 终止`;
  return null;
}

interface Props {
  open: boolean;
  commandName: string;
  result: ToolCommandResult | null;
  /** 执行中的实时输出（stdout/stderr 合并流式追加，最多保留最近 2000 行） */
  logs: string[];
  loading: boolean;
  /** 停止执行（终止进程树）；仅执行中提供 */
  onStop?: () => void;
  onClose: () => void;
}

export function ToolCommandResultDialog({ open, commandName, result, logs, loading, onStop, onClose }: Props) {
  const outputRef = useRef<HTMLPreElement>(null);
  /** 已请求停止（防重复点击）：命令返回（loading 变 false）后自动复位 */
  const [stopping, setStopping] = useState(false);
  /** 遮罩按下/抬起都在遮罩上才算「点遮罩关闭」（防止框内拖选文本松手时误关） */
  const overlayRef = useRef<HTMLDivElement>(null);
  const mouseDownTarget = useRef<EventTarget | null>(null);

  useEffect(() => {
    if (!loading) setStopping(false);
  }, [loading]);

  // 自动滚动到底部（result 和实时 logs 都在变化）
  useEffect(() => {
    if (outputRef.current) {
      outputRef.current.scrollTop = outputRef.current.scrollHeight;
    }
  }, [result, logs]);

  // 全屏遮罩弹窗：打开期间通知全局（AI 面板子 WebView 移出屏幕，防弹窗期间仍可点击）
  useEffect(() => {
    if (!open) return;
    useUiStore.getState().pushModal();
    return () => useUiStore.getState().popModal();
  }, [open]);

  // Esc 关闭（与 Modal 行为一致）：仅隐藏弹窗，执行中的命令在后台继续（结果不再展示）
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
    document.addEventListener('keydown', handler);
    return () => document.removeEventListener('keydown', handler);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div
      ref={overlayRef}
      role="dialog"
      aria-modal="true"
      aria-label={commandName}
      className="fixed inset-0 z-[80] flex items-center justify-center bg-black/50"
      onMouseDown={(e) => { mouseDownTarget.current = e.target; }}
      onMouseUp={(e) => {
        if (mouseDownTarget.current === overlayRef.current && e.target === overlayRef.current) onClose();
        mouseDownTarget.current = null;
      }}
    >
      <div
        className="w-[700px] max-h-[80vh] bg-nexus-bg border border-nexus-border rounded-lg shadow-2xl flex flex-col overflow-hidden"
        style={{ maxWidth: 'calc(100vw - 40px)' }}
        onClick={e => e.stopPropagation()}
      >
        {/* 头部 */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-nexus-border flex-shrink-0">
          <div className="flex items-center gap-2">
            {loading ? (
              <span className="w-[8px] h-[8px] rounded-full bg-nexus-warning animate-pulse" />
            ) : result?.success ? (
              <span className="w-[8px] h-[8px] rounded-full bg-nexus-success" />
            ) : (
              <span className="w-[8px] h-[8px] rounded-full bg-nexus-error" />
            )}
            <span className="text-[13px] text-nexus-text font-medium">{commandName}</span>
            {result && (
              <span className={`text-[11px] px-1.5 py-0.5 rounded ${
                result.success
                  ? 'bg-nexus-success/15 text-nexus-success'
                  : 'bg-nexus-error/15 text-nexus-error'
              }`}>
                {result.success ? '成功' : `失败：${describeExitCode(result.exit_code) ?? `(code ${result.exit_code})`}`}
              </span>
            )}
          </div>
          <button
            className="w-7 h-7 flex items-center justify-center rounded-lg text-nexus-muted hover:text-nexus-text hover:bg-nexus-hover/50"
            onClick={onClose}
          >
            <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
              <path d="M4 4l8 8M12 4l-8 8" />
            </svg>
          </button>
        </div>

        {/* 内容：单框混合输出（与 cmd 终端一致，stdout/stderr 按时间序交错）。
            自身不滚动，交给下面的输出框滚动——否则自动滚到底要跨两层容器 */}
        <div className="flex-1 min-h-0 p-4 flex flex-col">
          {loading ? (
            logs.length > 0 ? (
              <pre
                ref={outputRef}
                className="flex-1 min-h-0 bg-[#0d1117] text-[#c9d1d9] text-[12px] leading-relaxed p-3 rounded-md overflow-auto font-mono whitespace-pre-wrap break-all"
              >
                {logs.join('\n')}
              </pre>
            ) : (
              <div className="flex items-center justify-center py-8">
                <span className="text-[12px] text-nexus-muted">执行中...</span>
              </div>
            )
          ) : result ? (
            result.output ? (
              <pre
                ref={outputRef}
                className="flex-1 min-h-0 bg-[#0d1117] text-[#c9d1d9] text-[12px] leading-relaxed p-3 rounded-md overflow-auto font-mono whitespace-pre-wrap break-all"
              >
                {result.output}
              </pre>
            ) : (
              <div className="text-[12px] text-nexus-muted text-center py-4">
                命令执行完成，无输出
              </div>
            )
          ) : (
            <div className="text-[12px] text-nexus-muted text-center py-4">
              等待执行...
            </div>
          )}
        </div>

        {/* 底部：执行中可停止（终止进程树）；长构建类命令不必等到超时 */}
        <div className="flex items-center justify-end gap-2 px-4 py-3 border-t border-nexus-border flex-shrink-0">
          {loading && onStop && (
            <button
              className="px-4 py-1.5 text-[12px] text-nexus-error border border-nexus-error/40 rounded hover:bg-nexus-error/10 disabled:opacity-40 disabled:cursor-not-allowed"
              disabled={stopping}
              onClick={() => { setStopping(true); onStop(); }}
            >
              {stopping ? '停止中…' : '停止命令'}
            </button>
          )}
          <button
            className="px-4 py-1.5 text-[12px] bg-nexus-surface text-nexus-text rounded hover:bg-nexus-hover/50"
            onClick={onClose}
          >
            关闭
          </button>
        </div>
      </div>
    </div>
  );
}
