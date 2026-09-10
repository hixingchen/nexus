import { useEffect, useRef } from 'react';
import { useUiStore } from '../../stores/uiStore';

interface ModalProps {
  open: boolean;
  title: string;
  onClose: () => void;
  children: React.ReactNode;
  width?: string;
}

export function Modal({ open, title, onClose, children, width = '420px' }: ModalProps) {
  const overlayRef = useRef<HTMLDivElement>(null);
  const mouseDownTarget = useRef<EventTarget | null>(null);

  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
    document.addEventListener('keydown', handler);
    return () => document.removeEventListener('keydown', handler);
  }, [open, onClose]);

  // 全局弹窗计数：打开期间通知 AiPanel 移走原生子 WebView（原生层不受 DOM
  // 遮罩约束，不移走则弹窗打开时 dsh 页面仍可点击）；关闭后恢复原位。
  // 多个弹窗叠加计数，全部关闭才恢复。
  useEffect(() => {
    if (!open) return;
    useUiStore.getState().pushModal();
    return () => useUiStore.getState().popModal();
  }, [open]);

  if (!open) return null;

  // 遮罩层级 z-[65]：必须高于编辑面板 z-[60]（否则抽屉浮在遮罩上仍可点击），低于右键菜单 z-[70]
  return (
    <div
      ref={overlayRef}
      role="dialog"
      aria-modal="true"
      aria-label={title}
      className="fixed inset-0 z-[65] flex items-center justify-center bg-black/50"
      onMouseDown={(e) => { mouseDownTarget.current = e.target; }}
      onMouseUp={(e) => {
        // 仅在遮罩自身上下都按下+抬起时才关闭：拖拽选中文字后在遮罩上松手不算点击遮罩
        if (mouseDownTarget.current === overlayRef.current && e.target === overlayRef.current) {
          onClose();
        }
        mouseDownTarget.current = null;
      }}
    >
      <div
        className="bg-nexus-surface border border-nexus-border rounded-lg shadow-2xl overflow-hidden"
        style={{ width, maxWidth: 'calc(100vw - 40px)' }}
      >
        <div className="flex items-center justify-between px-4 h-[36px] border-b border-nexus-border bg-nexus-bg/30">
          <span className="text-[13px] text-nexus-text font-medium">{title}</span>
          <button
            className="text-nexus-muted hover:text-nexus-text p-1 rounded hover:bg-nexus-hover/50"
            aria-label="关闭"
            title="关闭"
            onClick={onClose}
          >
            <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.5">
              <line x1="2" y1="2" x2="10" y2="10"/><line x1="10" y1="2" x2="2" y2="10"/>
            </svg>
          </button>
        </div>
        <div className="p-4">
          {children}
        </div>
      </div>
    </div>
  );
}
