import { useEffect, useRef } from 'react';

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

  if (!open) return null;

  return (
    <div
      ref={overlayRef}
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
      onMouseDown={(e) => { mouseDownTarget.current = e.target; }}
      onMouseUp={(e) => {
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
