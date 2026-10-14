import { useState, useCallback, useRef, useEffect, type ReactNode } from 'react';

interface ResizablePanelProps {
  left: ReactNode;
  right: ReactNode;
  defaultLeftWidth?: number;
  minWidth?: number;
  maxWidth?: number;
  direction?: 'horizontal' | 'vertical';
  onResize?: (size: number) => void;
  /**
   * 收起态：左槽换成固定宽度的窄轨（`left` 传窄轨内容），分隔条与拖拽一起消失。
   *
   * 为什么由本组件承担、而不是在调用处换一棵树（MainLayout 原先的写法是
   * `cond ? <div>{窄轨}{右区}</div> : <ResizablePanel/>`）：换根 = 右区的组件身份变了，
   * React 会把整块右区**卸载重建**——点一次收缩，详情页、模板库、文件树、编辑器全部
   * 重新挂载并各自重拉一次数据，界面上连闪好几帧（实测：模板库闪「暂无模板」、
   * 窄轨闪空白、`get_project_detail` 等六个命令重发一遍）。
   *
   * 这里保持两侧槽位的**位置与类型**不变，只把中间的分隔条换成 null 占位——
   * 数组按 index 对账，右槽仍然对上原来那棵树，原地保留。
   */
  collapsed?: boolean;
  /** 收起态左槽尺寸（px） */
  collapsedWidth?: number;
}

export function ResizablePanel({
  left,
  right,
  defaultLeftWidth = 260,
  minWidth = 150,
  maxWidth = 600,
  direction = 'horizontal',
  onResize,
  collapsed = false,
  collapsedWidth = 32,
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
  /** 收起时左槽固定宽度；此时 `size` 不再被读，收回去的宽度值留在 state 里等展开时恢复 */
  const slotSize = collapsed ? collapsedWidth : size;

  return (
    <div
      ref={containerRef}
      className={`flex ${isHorizontal ? 'flex-row' : 'flex-col'} h-full w-full overflow-hidden`}
    >
      <div
        className="flex-shrink-0 overflow-hidden"
        style={isHorizontal ? { width: slotSize } : { height: slotSize }}
      >
        {left}
      </div>

      {/* 收起时不渲染分隔条（连 3px 都不占）。用条件表达式而非重新排列子节点：
          它渲染成 null 时仍占着第 2 个 index，右槽因此不会被 React 当成"新位置"重建 */}
      {!collapsed && (
        <div
          className={`flex-shrink-0 bg-nexus-border hover:bg-nexus-accent transition-colors ${
            isHorizontal ? 'w-[3px] cursor-col-resize' : 'h-[3px] cursor-row-resize'
          } ${isDragging ? 'bg-nexus-accent' : ''}`}
          onMouseDown={handleMouseDown}
        />
      )}

      <div className="flex-1 overflow-hidden min-w-0">
        {right}
      </div>
    </div>
  );
}
