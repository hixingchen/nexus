import { useEffect, useRef, useState } from 'react';
import { useClaudeStore } from '../../stores/claude';
import { ClaudeMessage } from './ClaudeMessage';
import { ClaudeInput } from './ClaudeInput';
import type { ClaudeStartParams } from '../../types/claude';

interface Props {
  /** 当前项目工作目录 */
  workingDir?: string;
  /** 是否可见 */
  visible?: boolean;
}

export function ClaudeChat({ workingDir, visible: _visible = true }: Props) {
  const { messages, status, model, sessionId, cliAvailable, checkCli, sendMessage, clearMessages, reset } = useClaudeStore();
  const scrollRef = useRef<HTMLDivElement>(null);
  const [showThinking, setShowThinking] = useState(false);

  // 启动时检查 CLI
  useEffect(() => {
    checkCli();
  }, [checkCli]);

  // 自动滚动到底部（仅当用户已在底部附近时）
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const isNearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 100;
    if (isNearBottom) {
      el.scrollTop = el.scrollHeight;
    }
  }, [messages]);

  const handleSend = (prompt: string) => {
    const params: ClaudeStartParams = {
      prompt,
      workingDir,
      permissionMode: 'auto',
    };
    sendMessage(params);
  };

  const isRunning = status === 'running';

  // CLI 不可用时的提示
  if (cliAvailable === false) {
    return (
      <div className="flex flex-col h-full bg-nexus-bg">
        <div className="flex-1 flex items-center justify-center p-6">
          <div className="text-center space-y-3 max-w-sm">
            <div className="text-[32px] opacity-30">⚡</div>
            <p className="text-[13px] text-nexus-text font-medium">Claude Code CLI 未安装</p>
            <p className="text-[12px] text-nexus-muted leading-relaxed">
              请先安装 Claude Code CLI：
            </p>
            <div className="bg-nexus-surface rounded-lg p-3 text-left">
              <code className="text-[12px] text-nexus-accent">npm install -g @anthropic-ai/claude-code</code>
            </div>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full bg-nexus-bg">
      {/* 顶栏 */}
      <div className="flex-shrink-0 flex items-center justify-between h-[34px] bg-nexus-surface border-b border-nexus-border/30 px-3">
        <div className="flex items-center gap-2">
          <span className="text-[12px] text-nexus-accent font-medium">Claude Code</span>
          {model && (
            <span className="text-[10px] text-nexus-muted bg-nexus-bg rounded px-1.5 py-0.5">
              {model}
            </span>
          )}
          {isRunning && (
            <span className="flex items-center gap-1 text-[10px] text-emerald-400">
              <span className="w-[5px] h-[5px] rounded-full bg-emerald-400 animate-pulse" />
              运行中
            </span>
          )}
        </div>
        <div className="flex items-center gap-1">
          <button
            className="text-[10px] text-nexus-muted hover:text-nexus-text px-2 py-1 rounded hover:bg-nexus-bg/50 transition-colors"
            onClick={() => setShowThinking(p => !p)}
          >
            {showThinking ? '隐藏思考' : '显示思考'}
          </button>
          <button
            className="text-[10px] text-nexus-muted hover:text-nexus-text px-2 py-1 rounded hover:bg-nexus-bg/50 transition-colors"
            onClick={clearMessages}
            disabled={isRunning}
          >
            清空
          </button>
          {sessionId && (
            <button
              className="text-[10px] text-nexus-muted hover:text-nexus-error px-2 py-1 rounded hover:bg-nexus-error/10 transition-colors"
              onClick={reset}
              disabled={isRunning}
              title="丢弃当前会话上下文，开始全新对话"
            >
              新对话
            </button>
          )}
        </div>
      </div>

      {/* 消息区域 */}
      <div ref={scrollRef} className="flex-1 overflow-y-auto p-3 space-y-3">
        {messages.length === 0 && (
          <div className="flex items-center justify-center h-full">
            <div className="text-center space-y-2">
              <div className="text-[40px] opacity-10 select-none">✦</div>
              <p className="text-[12px] text-nexus-muted">输入问题开始对话</p>
              <p className="text-[11px] text-nexus-muted/60">
                Claude Code 将自动读写文件、执行命令
              </p>
              <p className="text-[10px] text-nexus-muted/40">
                支持多轮对话，上下文自动保持
              </p>
            </div>
          </div>
        )}

        {messages.map((msg) => (
          <ClaudeMessage
            key={msg.id}
            message={msg}
            showThinking={showThinking}
          />
        ))}

        {isRunning && messages.length > 0 && messages[messages.length - 1].role === 'user' && (
          <div className="flex items-center gap-2 text-nexus-muted">
            <div className="flex gap-1">
              <span className="w-[5px] h-[5px] rounded-full bg-nexus-accent/60 animate-bounce" style={{ animationDelay: '0ms' }} />
              <span className="w-[5px] h-[5px] rounded-full bg-nexus-accent/60 animate-bounce" style={{ animationDelay: '150ms' }} />
              <span className="w-[5px] h-[5px] rounded-full bg-nexus-accent/60 animate-bounce" style={{ animationDelay: '300ms' }} />
            </div>
            <span className="text-[11px]">思考中...</span>
          </div>
        )}
      </div>

      {/* 输入框 */}
      <ClaudeInput
        onSend={handleSend}
        disabled={isRunning}
        running={isRunning}
      />
    </div>
  );
}
