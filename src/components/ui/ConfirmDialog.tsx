import { useEffect, useRef } from 'react';

type ConfirmVariant = 'danger' | 'warning' | 'info';

interface ConfirmDialogProps {
  open: boolean;
  variant?: ConfirmVariant;
  title: string;
  description?: string;
  /** 操作后果列表（bullet points） */
  consequences?: string[];
  confirmLabel?: string;
  cancelLabel?: string;
  loading?: boolean;
  onConfirm: () => void;
  onClose: () => void;
}

const VARIANT_STYLES: Record<ConfirmVariant, {
  icon: string;
  iconBg: string;
  iconColor: string;
  btnBg: string;
  btnHover: string;
}> = {
  danger: {
    icon: '⚠',
    iconBg: 'bg-nexus-error/10',
    iconColor: 'text-nexus-error',
    btnBg: 'bg-nexus-error',
    btnHover: 'hover:bg-nexus-error/80',
  },
  warning: {
    icon: '⚡',
    iconBg: 'bg-nexus-warning/10',
    iconColor: 'text-nexus-warning',
    btnBg: 'bg-nexus-warning',
    btnHover: 'hover:bg-nexus-warning/80',
  },
  info: {
    icon: 'ℹ',
    iconBg: 'bg-nexus-info/10',
    iconColor: 'text-nexus-info',
    btnBg: 'bg-nexus-accent',
    btnHover: 'hover:bg-nexus-accent/80',
  },
};

export function ConfirmDialog({
  open,
  variant = 'danger',
  title,
  description,
  consequences,
  confirmLabel = '确认',
  cancelLabel = '取消',
  loading = false,
  onConfirm,
  onClose,
}: ConfirmDialogProps) {
  const overlayRef = useRef<HTMLDivElement>(null);
  const mouseDownTarget = useRef<EventTarget | null>(null);
  const confirmRef = useRef<HTMLButtonElement>(null);

  const style = VARIANT_STYLES[variant];

  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
      if (e.key === 'Enter' && !loading) onConfirm();
    };
    document.addEventListener('keydown', handler);
    // 自动聚焦确认按钮
    setTimeout(() => confirmRef.current?.focus(), 50);
    return () => document.removeEventListener('keydown', handler);
  }, [open, onClose, onConfirm, loading]);

  if (!open) return null;

  return (
    <div
      ref={overlayRef}
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
      style={{ animation: 'fadeIn 150ms ease-out' }}
      onMouseDown={(e) => { mouseDownTarget.current = e.target; }}
      onMouseUp={(e) => {
        if (mouseDownTarget.current === overlayRef.current && e.target === overlayRef.current) {
          onClose();
        }
        mouseDownTarget.current = null;
      }}
    >
      <div
        className="bg-nexus-surface border border-nexus-border rounded-xl shadow-2xl w-[380px] max-w-[calc(100vw-40px)]"
        style={{ animation: 'dialogIn 150ms ease-out' }}
      >
        {/* 内容区 */}
        <div className="p-5 pb-4">
          {/* 图标 + 标题 */}
          <div className="flex items-start gap-3.5">
            <div className={`w-9 h-9 rounded-lg ${style.iconBg} flex items-center justify-center flex-shrink-0 text-base`}>
              <span className={style.iconColor}>{style.icon}</span>
            </div>
            <div className="flex-1 min-w-0 pt-0.5">
              <h3 className="text-[14px] font-semibold text-nexus-text leading-tight">{title}</h3>
              {description && (
                <p className="mt-1.5 text-[12px] text-nexus-muted leading-relaxed">{description}</p>
              )}
            </div>
          </div>

          {/* 操作后果列表 */}
          {consequences && consequences.length > 0 && (
            <div className="mt-3.5 ml-[50px] space-y-1">
              {consequences.map((item, i) => (
                <div key={i} className="flex items-start gap-2 text-[12px] text-nexus-muted">
                  <span className="text-nexus-muted/40 mt-[1px] flex-shrink-0">›</span>
                  <span>{item}</span>
                </div>
              ))}
            </div>
          )}
        </div>

        {/* 按钮区 */}
        <div className="flex items-center justify-end gap-2 px-5 py-3 border-t border-nexus-border/50 bg-nexus-bg/30 rounded-b-xl">
          <button
            className="px-3.5 py-1.5 text-[12px] text-nexus-text-muted hover:text-nexus-text rounded-md hover:bg-nexus-hover/50 transition-colors"
            onClick={onClose}
            disabled={loading}
          >
            {cancelLabel}
          </button>
          <button
            ref={confirmRef}
            className={`px-4 py-1.5 text-[12px] text-white rounded-md ${style.btnBg} ${style.btnHover} transition-colors disabled:opacity-40`}
            disabled={loading}
            onClick={onConfirm}
          >
            {loading ? '处理中…' : confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
