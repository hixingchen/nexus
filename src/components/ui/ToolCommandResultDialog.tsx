import { useEffect, useRef } from 'react';
import type { ToolCommandResult } from '../../services/service';

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
  onClose: () => void;
}

export function ToolCommandResultDialog({ open, commandName, result, logs, loading, onClose }: Props) {
  const outputRef = useRef<HTMLPreElement>(null);

  // 自动滚动到底部（result 和实时 logs 都在变化）
  useEffect(() => {
    if (outputRef.current) {
      outputRef.current.scrollTop = outputRef.current.scrollHeight;
    }
  }, [result, logs]);

  if (!open) return null;

  return (
    <div className="fixed inset-0 z-[80] flex items-center justify-center bg-black/50">
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

        {/* 内容：单框混合输出（与 cmd 终端一致，stdout/stderr 按时间序交错） */}
        <div className="flex-1 overflow-auto p-4">
          {loading ? (
            logs.length > 0 ? (
              <pre
                ref={outputRef}
                className="bg-[#0d1117] text-[#c9d1d9] text-[12px] leading-relaxed p-3 rounded-md overflow-auto max-h-[400px] font-mono whitespace-pre-wrap break-all"
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
                className="bg-[#0d1117] text-[#c9d1d9] text-[12px] leading-relaxed p-3 rounded-md overflow-auto max-h-[400px] font-mono whitespace-pre-wrap break-all"
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
