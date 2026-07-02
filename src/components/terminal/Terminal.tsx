import { useRef, useState, useEffect } from 'react';
import { useTerminalSetup } from '../../hooks/useTerminalSetup';

interface TerminalProps {
  /** PTY 会话 ID（后端 session_id） */
  ptySessionId: string;
  /** 终端工作目录 */
  workingDir?: string;
  /** 面板是否可见 */
  visible?: boolean;
  /** 初始命令 */
  initCommand?: string;
}

export function Terminal({ ptySessionId, workingDir, visible = true, initCommand }: TerminalProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);
  const { copySelection, paste } = useTerminalSetup(ptySessionId, containerRef, visible, workingDir, initCommand);

  // 点击外部关闭菜单
  useEffect(() => {
    if (!contextMenu) return;
    const handleClose = () => setContextMenu(null);
    document.addEventListener('click', handleClose);
    return () => document.removeEventListener('click', handleClose);
  }, [contextMenu]);

  const handleContextMenu = (e: React.MouseEvent) => {
    e.preventDefault();
    setContextMenu({ x: e.clientX, y: e.clientY });
  };

  return (
    <div
      ref={containerRef}
      className="h-full w-full bg-[#282c34] relative"
      style={{ padding: '4px 0 0 4px' }}
      onContextMenu={handleContextMenu}
    >
      {contextMenu && (
        <div
          className="fixed z-[70] w-[160px] bg-nexus-surface border border-nexus-border/60 rounded-lg shadow-2xl overflow-hidden"
          style={{
            left: Math.min(contextMenu.x, window.innerWidth - 168),
            top: Math.min(contextMenu.y, window.innerHeight - 120),
          }}
        >
          <div className="py-1.5 px-1.5">
            <button
              className="w-full flex items-center gap-2 px-2.5 py-1.5 rounded-md hover:bg-nexus-accent/10 transition-colors group text-left"
              onClick={() => { copySelection(); setContextMenu(null); }}
            >
              <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.2" className="text-nexus-muted group-hover:text-nexus-accent">
                <rect x="3.5" y="3.5" width="6" height="6.5" rx=".8"/>
                <path d="M2.5 3v6h.5V4h5V3H3a.5.5 0 00-.5.5z"/>
              </svg>
              <span className="text-[12px] text-nexus-text">复制</span>
            </button>
            <button
              className="w-full flex items-center gap-2 px-2.5 py-1.5 rounded-md hover:bg-nexus-accent/10 transition-colors group text-left"
              onClick={() => { paste(); setContextMenu(null); }}
            >
              <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.2" className="text-nexus-muted group-hover:text-nexus-accent">
                <rect x="2" y="3" width="8" height="7" rx="1"/>
                <path d="M4 3V2a1 1 0 011-1h2a1 1 0 011 1v1"/>
              </svg>
              <span className="text-[12px] text-nexus-text">粘贴</span>
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
