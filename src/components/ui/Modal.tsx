import { useCallback, useEffect, useRef } from 'react';
import { newModalId, useUiStore } from '../../stores/uiStore';

interface ModalProps {
  open: boolean;
  title: string;
  onClose: () => void;
  children: React.ReactNode;
  width?: string;
}

/** 可聚焦元素（焦点陷阱与"打开时先聚焦谁"共用一份选择器） */
const FOCUSABLE = 'a[href], button:not([disabled]), textarea:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])';

export function Modal({ open, title, onClose, children, width = '420px' }: ModalProps) {
  const overlayRef = useRef<HTMLDivElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const mouseDownTarget = useRef<EventTarget | null>(null);
  /** 本实例在弹窗栈里的 id（同一个 Modal 实例跨开关复用，故用 ref 而不是 state） */
  const modalIdRef = useRef<number>(0);
  if (modalIdRef.current === 0) modalIdRef.current = newModalId();

  // ── 焦点管理（UX-6）：不聚焦、不困住、不还原，是"声明了 role=dialog 却没实现它" ──
  //
  // 为什么必须困住：删除确认弹窗开着时，Tab 会走到**遮罩后面**的服务卡片上，
  // 回车即触发「启动」——一次误操作。为什么必须还原：关闭后焦点落在 body 上，
  // 键盘用户要从头 Tab 一遍才能回到刚才的位置。
  useEffect(() => {
    if (!open) return;
    const previouslyFocused = document.activeElement as HTMLElement | null;
    // 优先聚焦第一个**输入类**控件（表单弹窗的诉求），否则聚焦对话框自身
    const dialog = dialogRef.current;
    const firstInput = dialog?.querySelector<HTMLElement>('input, textarea, select');
    (firstInput ?? dialog)?.focus();

    return () => {
      // 元素可能已经不在文档里（触发它的按钮随列表刷新消失了）：`isConnected` 兜一下
      if (previouslyFocused?.isConnected) previouslyFocused.focus();
    };
  }, [open]);

  /** Tab 循环：焦点不许离开对话框（Shift+Tab 反向同理） */
  const onKeyDown = useCallback((e: React.KeyboardEvent) => {
    if (e.key === 'Escape') {
      // 只关**最上面**那一层（弹窗栈）：叠加时（工具库 → 删除确认）两个监听器都挂在
      // document 上，各写各的会按一次 Esc 关掉两个弹窗（UX-6）
      const stack = useUiStore.getState().modalStack;
      if (stack[stack.length - 1] !== modalIdRef.current) return;
      e.stopPropagation();
      onClose();
      return;
    }
    if (e.key !== 'Tab') return;
    const dialog = dialogRef.current;
    if (!dialog) return;
    const items = Array.from(dialog.querySelectorAll<HTMLElement>(FOCUSABLE))
      .filter(el => el.offsetParent !== null); // 排除被 CSS 藏起来的（不可见的按钮不该进循环）
    if (items.length === 0) return;
    const first = items[0]!;
    const last = items[items.length - 1]!;
    const active = document.activeElement;
    if (!e.shiftKey && active === last) { e.preventDefault(); first.focus(); }
    else if (e.shiftKey && (active === first || active === dialog)) { e.preventDefault(); last.focus(); }
  }, [onClose]);

  // 全局弹窗计数 + 弹窗栈：打开期间通知 AiPanel 移走原生子 WebView（原生层不受 DOM
  // 遮罩约束，不移走则弹窗打开时 dsh 页面仍可点击）；关闭后恢复原位。
  // 多个弹窗叠加计数，全部关闭才恢复。
  useEffect(() => {
    if (!open) return;
    const id = modalIdRef.current;
    useUiStore.getState().pushModal();
    useUiStore.getState().pushModalId(id);
    return () => {
      useUiStore.getState().popModal();
      useUiStore.getState().popModalId(id);
    };
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
      onKeyDown={onKeyDown}
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
        ref={dialogRef}
        // tabIndex=-1：没有可聚焦控件时（纯提示弹窗）焦点仍有落点，
        // 否则 `:focus` 留在 body 上、Tab 会从页面开头重新开始
        tabIndex={-1}
        className="bg-nexus-surface border border-nexus-border rounded-lg shadow-2xl overflow-hidden focus:outline-none"
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
