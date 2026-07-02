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

  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    setIsDragging(true);
  }, []);

  useEffect(() => {
    if (!isDragging) return;

    let rafId: number | null = null;
    let pendingEvent: MouseEvent | null = null;

    const flush = () => {
      if (!pendingEvent || !containerRef.current) { rafId = null; return; }
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
      rafId = null;
    };

    const handleMouseMove = (e: MouseEvent) => {
      pendingEvent = e;
      if (rafId === null) {
        rafId = requestAnimationFrame(flush);
      }
    };

    const handleMouseUp = () => {
      if (rafId !== null) {
        cancelAnimationFrame(rafId);
        rafId = null;
      }
      setIsDragging(false);
      onResize?.(sizeRef.current);
    };

    document.addEventListener('mousemove', handleMouseMove);
    document.addEventListener('mouseup', handleMouseUp);

    return () => {
      if (rafId !== null) cancelAnimationFrame(rafId);
      document.removeEventListener('mousemove', handleMouseMove);
      document.removeEventListener('mouseup', handleMouseUp);
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
