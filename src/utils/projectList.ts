import type { Project } from '../services/service';

/**
 * 项目列表的显示规则（纯函数，便于直接测）。
 *
 * 注：这里曾有过 `groupProjects`（把收藏与其余拆成两组）与"分组可折叠"。
 * 两者已按用户要求移除，原因值得记下来：
 * - **排序已经说明了收藏**：后端 `ORDER BY pinned DESC, sort_index` 返回，收藏本来就在最前；
 *   分组渲染出来的顺序与直接渲染**完全一样**，所以标题一去掉，拆分就成了空壳。
 * - **折叠的收益不明显**：换来的是一个状态、一个持久化键、以及一条"搜索时忽略折叠"的例外规则。
 *
 * 现在收藏在列表里的表达只有两处：**顺序**（在最前）与**行上的 ★**（点击切换）。
 */

/**
 * 窄轨（32px 的 `ProjectRail`）里，第 `index` 格之前要不要画一条"收藏 / 其余"的分隔线。
 *
 * 为什么列表那边不需要而窄轨需要：列表的每一行都有 ★ 显示自己是不是收藏，
 * 而 32px 的方块上**没有任何单格标记**——只有一条边界线能说明"上面的都是收藏"。
 *
 * 边界条件（全收藏 / 全非收藏时都不该画）是这条判断存在的全部理由，故单独可测。
 */
export function needsFavoriteDivider(projects: Project[], index: number): boolean {
  return index > 0 && !projects[index].pinned && projects[index - 1].pinned;
}
