/** 机器人头图标：AI 助手入口按钮与弹窗头部共用（描边风格，继承 currentColor） */
export function RobotIcon({ size = 16 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <path d="M5 5.2V4" />
      <path d="M11 5.2V4" />
      <circle cx="5" cy="3.1" r="1" fill="currentColor" stroke="none" opacity="0.9" />
      <circle cx="11" cy="3.1" r="1" fill="currentColor" stroke="none" opacity="0.9" />
      <rect x="3.1" y="5.2" width="9.8" height="7.4" rx="2.5" />
      <circle cx="6.1" cy="9.1" r="1" fill="currentColor" stroke="none" />
      <circle cx="9.9" cy="9.1" r="1" fill="currentColor" stroke="none" />
      <path d="M6.4 11.8h3.2" />
    </svg>
  );
}
