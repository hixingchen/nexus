import { useEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from 'react';
import { createPortal } from 'react-dom';
import { useClickOutside } from '../../hooks/useClickOutside';

export interface SelectMenuOption<T extends string> {
  value: T;
  label: string;
}

interface SelectMenuProps<T extends string> {
  value: T;
  options: ReadonlyArray<SelectMenuOption<T>>;
  onChange: (value: T) => void;
  ariaLabel: string;
  title?: string;
  /** 触发器外观（尺寸/配色由调用方决定；菜单浮层统一用面板配色） */
  className?: string;
}

/**
 * 选择器（按钮 + 浮层列表）。
 *
 * 为什么不用原生 `<select>`：它的展开列表与箭头由系统绘制，在深色面板里风格割裂
 * （服务编辑面板的「打开方式」也为此自己实现了浮层）。这里沿用那套观感：触发器由
 * 调用方给尺寸/配色，浮层统一 `nexus-surface + shadow-2xl`。
 *
 * 浮层 portal 到 body：日志/服务面板的容器可能 overflow 裁剪内部 absolute 菜单。
 */
export function SelectMenu<T extends string>({
  value,
  options,
  onChange,
  ariaLabel,
  title,
  className = '',
}: SelectMenuProps<T>) {
  const [open, setOpen] = useState(false);
  const [pos, setPos] = useState<{ left: number; top: number; width: number } | null>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);

  // 触发器也算"内部"：点在它上面由它自己 toggle，避免 mousedown 关闭后 click 又打开
  useClickOutside(menuRef, () => setOpen(false), {
    refs: [triggerRef],
    escape: true,
    captureScroll: true,
  });

  // 打开后聚焦当前项：键盘用户可以直接回车确认
  useEffect(() => {
    if (!open) return;
    const items = menuItems(menuRef.current);
    const active = items.find(el => el.getAttribute('aria-checked') === 'true');
    (active ?? items[0])?.focus();
  }, [open]);

  const openMenu = () => {
    const el = triggerRef.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    const width = Math.max(rect.width, 132);
    setPos({
      left: Math.max(8, Math.min(rect.left, window.innerWidth - width - 8)),
      top: rect.bottom + 4,
      width,
    });
    setOpen(true);
  };

  const close = (restoreFocus: boolean) => {
    setOpen(false);
    if (restoreFocus) triggerRef.current?.focus();
  };

  const handleMenuKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    if (e.key === 'Escape') {
      // 阻止日志面板/弹窗的全局 Esc：这一下只关下拉
      e.preventDefault();
      e.stopPropagation();
      close(true);
      return;
    }
    if (e.key === 'Tab') {
      // Tab 不用于菜单内走位（方向键负责），直接收起并让焦点按页面顺序继续
      close(false);
      return;
    }
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp' && e.key !== 'Home' && e.key !== 'End') return;
    const items = menuItems(menuRef.current);
    if (items.length === 0) return;
    e.preventDefault();
    const index = items.findIndex(el => el === document.activeElement);
    const next = e.key === 'Home' ? 0
      : e.key === 'End' ? items.length - 1
      : e.key === 'ArrowDown' ? (index + 1 + items.length) % items.length
      : (index - 1 + items.length) % items.length;
    items[next]?.focus();
  };

  const current = options.find(o => o.value === value) ?? options[0];

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={ariaLabel}
        title={title}
        className={`flex items-center gap-1.5 transition-colors ${className}`}
        onClick={() => { if (open) close(false); else openMenu(); }}
        onKeyDown={e => {
          if (!open && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
            e.preventDefault();
            openMenu();
          }
        }}
      >
        <span className="truncate">{current?.label}</span>
        <svg
          width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.4"
          className={`flex-shrink-0 transition-transform ${open ? 'rotate-180' : ''}`}
        >
          <polyline points="2,3.5 5,6.5 8,3.5" />
        </svg>
      </button>

      {open && pos && createPortal(
        <div
          ref={menuRef}
          role="menu"
          aria-label={ariaLabel}
          onKeyDown={handleMenuKeyDown}
          className="fixed z-[70] bg-nexus-surface border border-nexus-border/60 rounded-lg shadow-2xl overflow-hidden"
          style={{ left: pos.left, top: pos.top, width: pos.width }}
        >
          <div className="max-h-[264px] overflow-auto py-1">
            {options.map(option => {
              const active = option.value === value;
              return (
                <button
                  key={option.value}
                  type="button"
                  role="menuitemradio"
                  aria-checked={active}
                  className={`w-full flex items-center gap-2 px-3 py-1.5 text-left transition-colors ${
                    active ? 'bg-nexus-accent/10' : 'hover:bg-nexus-hover/50'
                  }`}
                  onClick={() => {
                    onChange(option.value);
                    close(true);
                  }}
                >
                  <span className={`flex-1 text-[12px] ${active ? 'text-nexus-text font-medium' : 'text-nexus-text/90'}`}>
                    {option.label}
                  </span>
                  {active && (
                    <svg width="11" height="11" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.6"
                      className="text-nexus-accent flex-shrink-0">
                      <polyline points="1.5,5.5 4,7.5 8.5,2.5" />
                    </svg>
                  )}
                </button>
              );
            })}
          </div>
        </div>,
        document.body,
      )}
    </>
  );
}

function menuItems(root: HTMLElement | null): HTMLElement[] {
  return [...(root?.querySelectorAll<HTMLElement>('[role="menuitemradio"]') ?? [])];
}
