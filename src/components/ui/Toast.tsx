import { toast } from 'sonner';
import { setNotifier } from '../../utils/notify';


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
  /** 行动按钮（如保存冲突的「重新加载」）：点了就跑，并把这条通知收掉 */
  action?: { label: string; run: () => void };
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
  action,
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

        {action && (
          <button
            className="px-2 py-1 text-[11px] font-medium text-nexus-accent border border-nexus-accent/40 rounded-md hover:bg-nexus-accent/10 flex-shrink-0 transition-colors"
            onClick={() => { action.run(); toast.dismiss(t); }}
          >{action.label}</button>
        )}

        {copyDetail && (
          <button
            title="复制错误详情"
            className="p-1 text-nexus-muted/50 hover:text-nexus-text rounded-md hover:bg-nexus-hover/50 flex-shrink-0"
            onClick={() => {
              // 静默写入：复制失败**不能**弹新通知——那会盖住用户正要复制的这条错误通知。
              // 这里不调 utils/clipboard 的 copyText：它要弹提示就不得不 import 本文件，
              // 于是形成 `Toast → clipboard → Toast` 的循环依赖（构建期警告 + 运行期
              // 绑定可能是 undefined）。三行内联换一个无环的依赖图，值。
              void navigator.clipboard.writeText(`${title}: ${description}`)
                .catch((e) => console.warn('复制错误详情失败:', e));
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

// 注册为全局通知出口（ARCH-17）：`utils/` 与 `stores/` 不能 import 本文件，只能通过
// `utils/notify.ts` 的端口弹提示，由这里在模块加载时把实现接上去。
// 放在模块底部而不是某个组件里：任何 import 了本文件的地方（`MainLayout` 挂 <Toaster>）
// 都会把它带上，不需要谁记得额外调用一次注册。
setNotifier(showNotification);
