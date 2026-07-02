import { useEffect, useRef } from 'react';
import type { ToolCommandResult } from '../../services/service';

interface Props {
  open: boolean;
  commandName: string;
  result: ToolCommandResult | null;
  loading: boolean;
  onClose: () => void;
}

export function ToolCommandResultDialog({ open, commandName, result, loading, onClose }: Props) {
  const stdoutRef = useRef<HTMLPreElement>(null);
  const stderrRef = useRef<HTMLPreElement>(null);

  // 自动滚动到底部
  useEffect(() => {
    if (stdoutRef.current) {
      stdoutRef.current.scrollTop = stdoutRef.current.scrollHeight;
    }
    if (stderrRef.current) {
      stderrRef.current.scrollTop = stderrRef.current.scrollHeight;
    }
  }, [result]);

  if (!open) return null;

  return (
    <div className="fixed inset-0 z-[80] flex items-center justify-center bg-black/50" onClick={onClose}>
      <div
        className="w-[700px] max-h-[80vh] bg-nexus-bg border border-nexus-border rounded-lg shadow-2xl flex flex-col"
        onClick={e => e.stopPropagation()}
      >
        {/* 头部 */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-nexus-border">
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
                {result.success ? '成功' : `失败 (${result.exit_code})`}
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

        {/* 内容 */}
        <div className="flex-1 overflow-auto p-4 space-y-3">
          {loading ? (
            <div className="flex items-center justify-center py-8">
              <span className="text-[12px] text-nexus-muted">执行中...</span>
            </div>
          ) : result ? (
            <>
              {/* stdout */}
              {result.stdout && (
                <div>
                  <div className="text-[11px] text-nexus-muted mb-1">标准输出</div>
                  <pre
                    ref={stdoutRef}
                    className="bg-[#0d1117] text-[#c9d1d9] text-[12px] leading-relaxed p-3 rounded-md overflow-auto max-h-[200px] font-mono whitespace-pre-wrap break-all"
                  >
                    {result.stdout}
                  </pre>
                </div>
              )}

              {/* stderr */}
              {result.stderr && (
                <div>
                  <div className="text-[11px] text-nexus-error mb-1">错误输出</div>
                  <pre
                    ref={stderrRef}
                    className="bg-[#0d1117] text-[#e06c75] text-[12px] leading-relaxed p-3 rounded-md overflow-auto max-h-[200px] font-mono whitespace-pre-wrap break-all"
                  >
                    {result.stderr}
                  </pre>
                </div>
              )}

              {/* 无输出 */}
              {!result.stdout && !result.stderr && (
                <div className="text-[12px] text-nexus-muted text-center py-4">
                  命令执行完成，无输出
                </div>
              )}
            </>
          ) : (
            <div className="text-[12px] text-nexus-muted text-center py-4">
              等待执行...
            </div>
          )}
        </div>

        {/* 底部 */}
        <div className="flex justify-end px-4 py-3 border-t border-nexus-border">
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
