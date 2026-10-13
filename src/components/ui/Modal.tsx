import { useCallback, useEffect, useRef } from 'react';
import { newModalId, useUiStore } from '../../stores/uiStore';

interface ModalProps {
  open: boolean;
  title: string;
  onClose: () => void;
  children: React.ReactNode;
  width?: string;
  /**
   * 左侧分类栏（设置面板用）。给了它就切到「左分类 + 右内容」的面板布局：
   * 标题栏不变，内容区左侧多出一列，两列**各自滚动**。
   *
   * 为什么做成 `Modal` 的一个变体、而不是另写一个 `SettingsModal` 壳：焦点陷阱（UX-6）、
   * Esc 只关栈顶（`uiStore.modalStack`）、`pushModal` 让 AiPanel 把**原生子 WebView**
   * 移出屏幕——这三件事都在这里。复制一份壳就等于把它们复制一遍，而它们各自都有失败史。
   */
  sidebar?: React.ReactNode;
  /**
   * 面板形态的**固定高度**（只在给了 `sidebar` 时生效）。
   *
   * 为什么需要"固定"：面板内容随分类切换长短不一（「通用」三节、Node 版本一屏列表），
   * 让高度跟着内容走的话，点一下分类整个弹窗就跳一次——连同标题栏和关闭按钮一起挪位，
   * 用户的鼠标刚对准的按钮就跑了。固定高度之后内容区自己滚动，外框不动。
   *
   * 不传则退回"内容撑开、max-height 封顶"（适合内容长度稳定的面板）。
   * 建议值形如 `clamp(440px, calc(100vh - 120px), 700px)`：对内容固定，但对窗口自适应
   * ——写死像素会让它在小窗口（本项目 minHeight 600）里顶出屏幕。
   */
  height?: string;
}

/** 可聚焦元素（焦点陷阱与"打开时先聚焦谁"共用一份选择器） */
const FOCUSABLE = 'a[href], button:not([disabled]), textarea:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])';

export function Modal({ open, title, onClose, children, width = '420px', sidebar, height }: ModalProps) {
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

  const isPanel = sidebar != null;

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
        //
        // 关闭＝"先离开"而不是"放弃"（UX-14）：带表单的弹窗把未提交内容写进
        // stores/modalDraftStore（点遮罩 / Esc / ✕ 关闭后重新打开还在），
        // 清草稿只发生在提交成功或用户显式点「取消」时。新加表单弹窗请沿用这套分工
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
        className={`bg-nexus-surface border border-nexus-border rounded-lg shadow-2xl overflow-hidden focus:outline-none ${isPanel ? 'flex flex-col' : ''}`}
        style={{
          width,
          maxWidth: 'calc(100vw - 40px)',
          // 面板形态要限高：Node 版本那一屏（已装列表 + 搜索 + 输出）会超过一屏高度，
          // 而 Modal 此前只有 maxWidth——高度顶出去时底部按钮就点不到了。
          // 给了 `height` 就用它（固定外框，内容自己滚），否则只封顶、高度仍由内容决定。
          ...(isPanel ? { maxHeight: 'calc(100vh - 80px)', ...(height ? { height } : null) } : null),
        }}
      >
        <div className="flex items-center justify-between px-4 h-[36px] border-b border-nexus-border bg-nexus-bg/30 flex-shrink-0">
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
        {isPanel ? (
          /* flex-auto（`flex: 1 1 auto`）而不是 flex-1（`flex: 1 1 0%`）：这个对话框的高度是
             "内容撑开、max-height 封顶"，**不是**确定高度——列向 flex 容器里 basis=0 的子项
             假设高度为 0，容器又按假设高度算自己的高度，于是整个内容区会**塌成 0 高**。
             basis=auto 让"内容多高就多高"，被封顶时靠 flex-shrink + min-h-0 收缩并自己滚动。 */
          <div className="flex-auto min-h-0 flex">
            <div className="w-[140px] flex-shrink-0 overflow-y-auto border-r border-nexus-border bg-nexus-bg/20 py-2">
              {sidebar}
            </div>
            <div className="flex-1 min-w-0 overflow-y-auto p-4">
              {children}
            </div>
          </div>
        ) : (
          <div className="p-4">
            {children}
          </div>
        )}
      </div>
    </div>
  );
}
