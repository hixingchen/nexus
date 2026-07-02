import { toast } from 'sonner';

type ToastVariant = 'success' | 'error' | 'warning' | 'info';

const VARIANT_STYLES: Record<ToastVariant, {
  icon: string;
  iconColor: string;
  iconBg: string;
}> = {
  success: {
    icon: '✓',
    iconColor: 'text-emerald-400',
    iconBg: 'bg-emerald-400/10',
  },
  error: {
    icon: '✕',
    iconColor: 'text-nexus-error',
    iconBg: 'bg-nexus-error/10',
  },
  warning: {
    icon: '⚡',
    iconColor: 'text-nexus-warning',
    iconBg: 'bg-nexus-warning/10',
  },
  info: {
    icon: 'ℹ',
    iconColor: 'text-nexus-info',
    iconBg: 'bg-nexus-info/10',
  },
};

interface ToastOptions {
  variant?: ToastVariant;
  title: string;
  description?: string;
  duration?: number;
}

/**
 * 显示自定义样式的 toast 通知
 *
 * 视觉风格与 RestartConfirm 一致：
 * 图标 + 标题 + 描述 + 关闭按钮
 */
export function showNotification({
  variant = 'success',
  title,
  description,
  duration = 3000,
}: ToastOptions) {
  const style = VARIANT_STYLES[variant];

  toast.custom(
    (t) => (
      <div className="flex items-center gap-3.5 bg-nexus-surface border border-nexus-border rounded-xl shadow-2xl pl-4 pr-3 py-3 min-w-[280px] max-w-[400px]">
        {/* 图标 */}
        <div className={`w-7 h-7 rounded-lg ${style.iconBg} flex items-center justify-center flex-shrink-0`}>
          <span className={`${style.iconColor} text-sm font-bold`}>{style.icon}</span>
        </div>

        {/* 文本 */}
        <div className="flex-1 min-w-0">
          <p className="text-[13px] text-nexus-text font-semibold truncate">{title}</p>
          {description && (
            <p className="text-[11px] text-nexus-muted mt-0.5 truncate">{description}</p>
          )}
        </div>

        {/* 关闭按钮 */}
        <button
          className="p-1 text-nexus-muted/50 hover:text-nexus-text rounded-md hover:bg-nexus-hover/50 flex-shrink-0"
          onClick={() => toast.dismiss(t)}
        >
          <svg width="12" height="12" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.5">
            <line x1="2" y1="2" x2="8" y2="8"/><line x1="8" y1="2" x2="2" y2="8"/>
          </svg>
        </button>
      </div>
    ),
    { duration },
  );
}
