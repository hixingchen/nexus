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
 * 按级别的默认停留时长（ms）：错误最长。
 *
 * 为什么分级：原先一律 3000ms，而后端错误文案普遍较长（"访问被拒绝：路径不在白名单……"），
 * 用户还没读完就消失，只能靠复现去猜；错误给 8s 并配「复制详情」，成功类保持 3s 不打扰。
 */
const VARIANT_DURATION: Record<ToastVariant, number> = {
  success: 3000,
  info: 4000,
  warning: 6000,
  error: 8000,
};

/**
 * 显示自定义样式的 toast 通知
 *
 * 视觉风格与 RestartConfirm 一致：
 * 图标 + 标题 + 描述 + 关闭按钮（错误另有「复制详情」）
 */
export function showNotification({
  variant = 'success',
  title,
  description,
  duration,
}: ToastOptions) {
  const style = VARIANT_STYLES[variant];
  const hideAfter = duration ?? VARIANT_DURATION[variant];
  // 错误详情可复制：用户能把原文贴进 issue/聊天，而不必凭记忆转述
  const copyDetail = variant === 'error' && !!description;

  toast.custom(
    (t) => (
      <div className="flex items-center gap-3.5 bg-nexus-surface border border-nexus-border rounded-xl shadow-2xl pl-4 pr-3 py-3 min-w-[280px] max-w-[400px]">
        {/* 图标 */}
        <div className={`w-7 h-7 rounded-lg ${style.iconBg} flex items-center justify-center flex-shrink-0`}>
          <span className={`${style.iconColor} text-sm font-bold`}>{style.icon}</span>
        </div>

        {/* 文本：标题单行截断；描述可换行完整显示（错误/诊断信息不能截断） */}
        <div className="flex-1 min-w-0">
          <p className="text-[13px] text-nexus-text font-semibold truncate">{title}</p>
          {description && (
            <p className="text-[11px] text-nexus-muted mt-0.5 break-words">{description}</p>
          )}
        </div>

        {copyDetail && (
          <button
            title="复制错误详情"
            className="p-1 text-nexus-muted/50 hover:text-nexus-text rounded-md hover:bg-nexus-hover/50 flex-shrink-0"
            onClick={() => {
              void navigator.clipboard.writeText(`${title}: ${description}`).catch(() => {});
            }}
          >
            <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.2">
              <rect x="4" y="4" width="6.5" height="6.5" rx="1"/>
              <path d="M8 4V2.5a1 1 0 00-1-1H2.5a1 1 0 00-1 1V7a1 1 0 001 1H4"/>
            </svg>
          </button>
        )}

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
    { duration: hideAfter },
  );
}
