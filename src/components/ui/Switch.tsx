/**
 * 滑动开关（`role="switch"` 的 button）。
 *
 * 为什么不用原生 checkbox：原生样式在各平台/主题下差异大，而且这一行的视觉重心
 * 应该在右侧那颗滑钮上。button 还能直接响应空格/回车，键盘可用性不打折。
 *
 * 设置面板（通用 / AI 运行时）共用这一份；服务编辑面板里那颗更小的是另一套视觉
 * （跟着表单行高走），不强行合并。
 */
export function Switch({
  checked, disabled, label, onChange,
}: {
  checked: boolean;
  disabled?: boolean;
  label: string;
  onChange: (checked: boolean) => void;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={`relative inline-flex h-[22px] w-[40px] flex-shrink-0 items-center rounded-full transition-colors duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-nexus-accent/60 disabled:cursor-not-allowed disabled:opacity-40 ${
        checked ? 'bg-nexus-accent' : 'bg-nexus-border'
      }`}
    >
      <span
        aria-hidden="true"
        className={`inline-block h-[16px] w-[16px] rounded-full bg-white shadow-sm transition-transform duration-150 ${
          checked ? 'translate-x-[21px]' : 'translate-x-[3px]'
        }`}
      />
    </button>
  );
}
