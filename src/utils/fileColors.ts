/* === 目录语义着色（JetBrains File Colors 模型）===
 * 颜色只表达语义，不做彩虹配色：
 *   生成物/依赖 → 淡化（后退，表示"不该手动改"）
 * 规则用精确目录名匹配，任意层级生效（任何位置的 target 都算）。
 * 内置集只有一种淡化样式——这些目录语义完全相同，不区别对待；
 * 用户自定义规则（突出色）未来由设置面板加入，合并进 DIR_COLORS 即可。
 */

const DIM = 'text-nexus-muted';

/** 内置忽略集：跨项目语义一致的生成物/依赖目录 */
const DIR_COLORS: Record<string, string> = {
  // 构建产物
  target: DIM,
  dist: DIM,
  build: DIM,
  out: DIM,
  // 依赖
  node_modules: DIM,
  // VCS / IDE
  '.git': DIM,
  '.idea': DIM,
  '.vscode': DIM,
  // 语言生态缓存
  __pycache__: DIM,
  '.venv': DIM,
  venv: DIM,
  '.next': DIM,
  '.gradle': DIM,
  coverage: DIM,
};

/** 目录名 → 颜色类名；不在规则内返回 null */
export function getDirColorClass(name: string): string | null {
  return DIR_COLORS[name] ?? null;
}
