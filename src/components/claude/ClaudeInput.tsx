import { useState, useRef, useEffect, useCallback } from 'react';

interface Props {
  onSend: (prompt: string) => void;
  disabled?: boolean;
  running?: boolean;
}

export function ClaudeInput({ onSend, disabled, running }: Props) {
  const [input, setInput] = useState('');
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  // 自动调整高度
  const adjustHeight = useCallback(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = 'auto';
    const maxHeight = 150;
    el.style.height = `${Math.min(el.scrollHeight, maxHeight)}px`;
  }, []);

  useEffect(() => {
    adjustHeight();
  }, [input, adjustHeight]);

  // 聚焦快捷键
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      // Ctrl/Cmd + Shift + A 聚焦输入框
      if ((e.ctrlKey || e.metaKey) && e.shiftKey && e.key === 'A') {
        e.preventDefault();
        textareaRef.current?.focus();
      }
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, []);

  const handleSend = () => {
    const trimmed = input.trim();
    if (!trimmed || disabled) return;
    onSend(trimmed);
    setInput('');
    // 重置高度
    if (textareaRef.current) {
      textareaRef.current.style.height = 'auto';
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    // Enter 发送（Shift+Enter 换行）
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  return (
    <div className="flex-shrink-0 border-t border-nexus-border/30 bg-nexus-surface p-3">
      <div className="flex items-end gap-2">
        <textarea
          ref={textareaRef}
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={handleKeyDown}
          disabled={disabled}
          placeholder={running ? 'Claude 正在处理中...' : '输入问题... (Enter 发送, Shift+Enter 换行)'}
          rows={1}
          className="flex-1 bg-nexus-bg border border-nexus-border/40 rounded-lg px-3 py-2
                     text-[12px] text-nexus-text placeholder:text-nexus-muted/50
                     resize-none overflow-y-auto
                     focus:outline-none focus:border-nexus-accent/50 focus:ring-1 focus:ring-nexus-accent/20
                     disabled:opacity-50 disabled:cursor-not-allowed
                     transition-colors"
          style={{ maxHeight: '150px' }}
        />
        <button
          onClick={handleSend}
          disabled={disabled || !input.trim()}
          className="flex-shrink-0 w-[32px] h-[32px] flex items-center justify-center
                     bg-nexus-accent/20 hover:bg-nexus-accent/30 border border-nexus-accent/30
                     rounded-lg text-nexus-accent
                     disabled:opacity-30 disabled:cursor-not-allowed
                     transition-colors"
        >
          {running ? (
            <svg width="14" height="14" viewBox="0 0 14 14" fill="currentColor">
              <rect x="3" y="3" width="8" height="8" rx="1" />
            </svg>
          ) : (
            <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.5">
              <path d="M2 7h10M8 3l4 4-4 4" />
            </svg>
          )}
        </button>
      </div>
    </div>
  );
}
