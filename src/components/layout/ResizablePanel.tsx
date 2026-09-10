import { useState, useCallback, useRef, useEffect, type ReactNode } from 'react';

interface ResizablePanelProps {
  left: ReactNode;
  right: ReactNode;
  defaultLeftWidth?: number;
  minWidth?: number;
  maxWidth?: number;
  direction?: 'horizontal' | 'vertical';
  onResize?: (size: number) => void;
}

export function ResizablePanel({
  left,
  right,
  defaultLeftWidth = 260,
  minWidth = 150,
  maxWidth = 600,
  direction = 'horizontal',
  onResize,
}: ResizablePanelProps) {
  const [size, setSize] = useState(defaultLeftWidth);
  const sizeRef = useRef(size);
  sizeRef.current = size;
  const [isDragging, setIsDragging] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  // 外部初始值变化（如布局从数据库异步恢复）时同步内部大小。
  // 拖拽期间父组件不改 prop（onResize 只做持久化），故不会打断拖拽
  const lastDefaultRef = useRef(defaultLeftWidth);
  useEffect(() => {
    if (lastDefaultRef.current === defaultLeftWidth) return;
    lastDefaultRef.current = defaultLeftWidth;
    setSize(defaultLeftWidth);
  }, [defaultLeftWidth]);

  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    setIsDragging(true);
  }, []);

  useEffect(() => {
    if (!isDragging) return;

    let rafId: number | null = null;
    let pendingEvent: MouseEvent | null = null;
    /** 结束标记：mouseup / blur / 卸载 任一先到即拆监听，重复调用安全 */
    let finished = false;

    const flush = (): number | null => {
      rafId = null;
      if (!pendingEvent || !containerRef.current) return null;
      const e = pendingEvent;
      pendingEvent = null;

      const rect = containerRef.current.getBoundingClientRect();
      let newSize: number;

      if (direction === 'horizontal') {
        newSize = e.clientX - rect.left;
      } else {
        newSize = e.clientY - rect.top;
      }

      newSize = Math.max(minWidth, Math.min(maxWidth, newSize));
      setSize(newSize);
      return newSize;
    };

    const handleMouseUp = () => {
      if (finished) return;
      finished = true;
      try {
        if (rafId !== null) cancelAnimationFrame(rafId);
        // 冲刷排队帧：否则松手位置被丢弃，面板停在光标之前
        const flushed = flush();
        setIsDragging(false);
        onResize?.(flushed ?? sizeRef.current);
      } finally {
        removeListeners();
      }
    };

    const handleMouseMove = (e: MouseEvent) => {
      // 无按键的移动 = 松手事件丢失（在窗口外/原生子 WebView 上松手），按松手处理
      if (e.buttons === 0) { handleMouseUp(); return; }
      pendingEvent = e;
      if (rafId === null) {
        rafId = requestAnimationFrame(flush);
      }
    };

    const removeListeners = () => {
      document.removeEventListener('mousemove', handleMouseMove);
      document.removeEventListener('mouseup', handleMouseUp);
      window.removeEventListener('blur', handleMouseUp);
    };

    document.addEventListener('mousemove', handleMouseMove);
    document.addEventListener('mouseup', handleMouseUp);
    // 拖拽中切走窗口（alt-tab）收不到 mouseup，用 blur 兜底结束
    window.addEventListener('blur', handleMouseUp);

    return () => {
      finished = true;
      if (rafId !== null) cancelAnimationFrame(rafId);
      removeListeners();
    };
  }, [isDragging, direction, minWidth, maxWidth]);

  const isHorizontal = direction === 'horizontal';

  return (
    <div
      ref={containerRef}
      className={`flex ${isHorizontal ? 'flex-row' : 'flex-col'} h-full w-full overflow-hidden`}
    >
      <div
        className="flex-shrink-0 overflow-hidden"
        style={isHorizontal ? { width: size } : { height: size }}
      >
        {left}
      </div>

      <div
        className={`flex-shrink-0 bg-nexus-border hover:bg-nexus-accent transition-colors ${
          isHorizontal ? 'w-[3px] cursor-col-resize' : 'h-[3px] cursor-row-resize'
        } ${isDragging ? 'bg-nexus-accent' : ''}`}
        onMouseDown={handleMouseDown}
      />

      <div className="flex-1 overflow-hidden min-w-0">
        {right}
      </div>
    </div>
  );
}
