/**
 * SVG 图标渲染组件
 *
 * 使用 dangerouslySetInnerHTML 渲染来自静态资源文件的 SVG。
 * SVG 来源为编译时导入的 ?raw 模块（FileIcons.tsx 中的 registry），
 * 不包含任何用户输入，因此不存在 XSS 风险。
 */

interface SvgIconProps {
  /** SVG 字符串（来自静态资源文件，非用户输入） */
  svg: string;
  className?: string;
  style?: React.CSSProperties;
}

export function SvgIcon({ svg, className, style }: SvgIconProps) {
  return (
    <span
      className={className}
      style={style}
      dangerouslySetInnerHTML={{ __html: svg }}
    />
  );
}
