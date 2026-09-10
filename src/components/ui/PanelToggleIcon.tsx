/**
 * 侧栏开关图标：三条横线（沿用原服务列表收起态的样式）。
 *
 * 说明：三条横线在通用语义里偏"菜单"，不表示折叠方向；同一图标同时用于项目列与服务列，
 * 所以"管的是哪一侧"由它所在的位置（各自面板的头部/窄轨内）与 `title` 提示承担，
 * 不靠图标本身区分。原先试过的"竖线+箭头"方向更明确，但视觉上被判定不如这个干净。
 */
export function PanelToggleIcon({ size = 14 }: { size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 14 14"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.3"
      strokeLinecap="round"
      aria-hidden="true"
    >
      <line x1="2.5" y1="3" x2="11.5" y2="3" />
      <line x1="2.5" y1="7" x2="11.5" y2="7" />
      <line x1="2.5" y1="11" x2="11.5" y2="11" />
    </svg>
  );
}
