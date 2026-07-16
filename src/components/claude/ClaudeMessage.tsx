import { useState } from 'react';
import type { ClaudeChatMessage } from '../../types/claude';

interface Props {
  message: ClaudeChatMessage;
  showThinking: boolean;
}

export function ClaudeMessage({ message, showThinking }: Props) {
  const { role, content, thinking, toolName, toolInput, toolResult, toolIsError, costUsd, durationMs } = message;

  // 用户消息
  if (role === 'user') {
    return (
      <div className="flex justify-end">
        <div className="max-w-[80%] bg-nexus-accent/15 border border-nexus-accent/20 rounded-lg px-3 py-2">
          <p className="text-[12px] text-nexus-text whitespace-pre-wrap break-words">{content}</p>
        </div>
      </div>
    );
  }

  // 系统/错误消息
  if (role === 'system') {
    return (
      <div className="flex justify-center">
        <div className="bg-nexus-error/10 border border-nexus-error/20 rounded-lg px-3 py-2 max-w-[80%]">
          <p className="text-[12px] text-nexus-error whitespace-pre-wrap break-words">{content}</p>
        </div>
      </div>
    );
  }

  // 工具调用消息
  if (role === 'tool' && toolName) {
    return <ToolCallMessage toolName={toolName} toolInput={toolInput} toolResult={toolResult} toolIsError={toolIsError} />;
  }

  // Assistant 消息
  return (
    <div className="flex flex-col gap-1">
      {/* 思考过程 */}
      {thinking && showThinking && (
        <div className="bg-nexus-surface/50 border border-nexus-border/20 rounded-lg px-3 py-2 ml-1">
          <p className="text-[10px] text-nexus-muted mb-1">💭 思考过程</p>
          <p className="text-[11px] text-nexus-muted/80 whitespace-pre-wrap break-words leading-relaxed">
            {thinking}
          </p>
        </div>
      )}

      {/* 文本内容 */}
      {content && (
        <div className="bg-nexus-surface border border-nexus-border/30 rounded-lg px-3 py-2">
          <div className="text-[12px] text-nexus-text whitespace-pre-wrap break-words leading-relaxed">
            {content}
          </div>
        </div>
      )}

      {/* 元信息（费用、耗时） */}
      {(costUsd != null || durationMs != null) && (
        <div className="flex items-center gap-3 ml-1">
          {costUsd != null && costUsd > 0 && (
            <span className="text-[10px] text-nexus-muted">
              ${costUsd.toFixed(4)}
            </span>
          )}
          {durationMs != null && durationMs > 0 && (
            <span className="text-[10px] text-nexus-muted">
              {(durationMs / 1000).toFixed(1)}s
            </span>
          )}
        </div>
      )}
    </div>
  );
}

/// 工具调用消息组件
function ToolCallMessage({
  toolName,
  toolInput,
  toolResult,
  toolIsError,
}: {
  toolName: string;
  toolInput?: unknown;
  toolResult?: string;
  toolIsError?: boolean;
}) {
  // 避免 unknown 直接渲染
  const inputDisplay = toolInput ? formatToolInput(toolInput) : null;
  const [expanded, setExpanded] = useState(false);

  const icon = getToolIcon(toolName);
  const label = getToolLabel(toolName, toolInput);
  const hasResult = toolResult !== undefined && toolResult !== null;

  return (
    <div className="bg-nexus-surface/70 border border-nexus-border/20 rounded-lg overflow-hidden">
      {/* 工具头部 */}
      <button
        className="w-full flex items-center gap-2 px-3 py-2 hover:bg-nexus-bg/30 transition-colors text-left"
        onClick={() => setExpanded(p => !p)}
      >
        <span className="text-[12px]">{icon}</span>
        <span className="text-[12px] text-nexus-accent font-medium">{toolName}</span>
        <span className="text-[11px] text-nexus-muted truncate flex-1">{label}</span>
        {hasResult && (
          <span className={`text-[10px] px-1.5 py-0.5 rounded ${toolIsError ? 'bg-nexus-error/10 text-nexus-error' : 'bg-emerald-500/10 text-emerald-400'}`}>
            {toolIsError ? '失败' : '完成'}
          </span>
        )}
        <svg
          width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2"
          className={`text-nexus-muted transition-transform ${expanded ? 'rotate-90' : ''}`}
        >
          <path d="M3.5 1.5L6.5 5L3.5 8.5" />
        </svg>
      </button>

      {/* 展开详情 */}
      {expanded && (
        <div className="border-t border-nexus-border/20 px-3 py-2 space-y-2">
          {/* 输入参数 */}
          {inputDisplay && (
            <div>
              <p className="text-[10px] text-nexus-muted mb-1">参数</p>
              <pre className="text-[11px] text-nexus-text/80 bg-nexus-bg rounded p-2 overflow-x-auto max-h-[200px] overflow-y-auto">
                {inputDisplay}
              </pre>
            </div>
          )}

          {/* 结果 */}
          {hasResult && (
            <div>
              <p className="text-[10px] text-nexus-muted mb-1">结果</p>
              <pre className={`text-[11px] ${toolIsError ? 'text-nexus-error/80' : 'text-nexus-text/80'} bg-nexus-bg rounded p-2 overflow-x-auto max-h-[300px] overflow-y-auto whitespace-pre-wrap break-words`}>
                {toolResult}
              </pre>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function getToolIcon(name: string): string {
  const icons: Record<string, string> = {
    Read: '📖',
    Write: '✏️',
    Edit: '🔧',
    Bash: '⚡',
    Grep: '🔍',
    Glob: '📁',
    WebFetch: '🌐',
    WebSearch: '🔎',
  };
  return icons[name] || '🛠';
}

function getToolLabel(name: string, input?: unknown): string {
  if (!input || typeof input !== 'object') return '';

  const inp = input as Record<string, unknown>;

  switch (name) {
    case 'Read':
    case 'Write':
    case 'Edit':
      return typeof inp.file_path === 'string' ? inp.file_path : '';
    case 'Bash':
      return typeof inp.command === 'string' ? inp.command.slice(0, 80) : '';
    case 'Grep':
      return typeof inp.pattern === 'string' ? inp.pattern : '';
    case 'Glob':
      return typeof inp.pattern === 'string' ? inp.pattern : '';
    default:
      return '';
  }
}

function formatToolInput(input: unknown): string {
  if (typeof input === 'string') return input;
  try {
    return JSON.stringify(input, null, 2);
  } catch {
    return String(input);
  }
}
