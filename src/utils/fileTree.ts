/**
 * 目录树的定位规则（纯函数，便于直接测）。
 *
 * 背景：左侧可能同时挂着多棵树——项目自己的目录 + 各服务的工作目录。**服务目录通常是项目
 * 目录的子目录**，于是同一个文件天然出现在多棵树里。点「在目录树中定位」时如果每棵树都响应，
 * 结果是：每棵树各自展开一整条链、各自高亮，而**左面板最后停在哪棵树上取决于谁最后调用
 * `scrollIntoView`**——用户看到的是"有时跳到服务里、有时跳到项目里"。
 *
 * 所以定位只由**一棵**树响应，规则是「根最长（最具体）者胜出」：
 * - 文件在某个服务目录下 → 服务树的根更长 → 服务树响应（服务才是主角）
 * - 文件不在任何服务目录下 → 只有项目树包含它 → 项目树兜底（不会"定位不到"）
 * - 两个服务目录嵌套 → 更深的那个响应
 */

/** 参与定位竞争的一棵树：根路径 + 平局优先级 */
export interface RevealTarget {
  /** 树根。与 `FileTree` 的 `rootPath`、以及文件树里各节点的 path 同源同形 */
  root: string;
  /** 根长度相同时比它：服务树 > 项目树（同根时该由"主角"响应） */
  priority: number;
}

/** 平局优先级：服务树优先于项目树 */
export const REVEAL_PRIORITY = { project: 0, service: 1 } as const;

/** `path` 是否位于 `root` 之内（按**路径分量**判断，`/a/b` 不命中 `/a/bc`） */
function isInside(path: string, root: string): boolean {
  if (!root) return false;
  // 根末尾可能带分隔符（用户配置的路径常见），去掉后再比
  const r = root.replace(/[\\/]+$/, '');
  if (r === '') return false;
  return path === r || path.startsWith(r + '/') || path.startsWith(r + '\\');
}

/**
 * 这次定位该由哪棵树响应：根最长者胜出；根等长时取优先级高的；都不包含则返回 null。
 *
 * 返回的是**传入数组里的那个对象**（调用方据此做引用比较判断"是不是我"）。
 */
export function pickRevealTarget(targets: readonly RevealTarget[], path: string): RevealTarget | null {
  if (!path) return null;
  let best: RevealTarget | null = null;
  for (const t of targets) {
    if (!isInside(path, t.root)) continue;
    if (
      best === null ||
      t.root.length > best.root.length ||
      (t.root.length === best.root.length && t.priority > best.priority)
    ) {
      best = t;
    }
  }
  return best;
}
