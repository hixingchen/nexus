/**
 * 编辑器标签与「已被删除的路径」的对应规则（纯函数，不 import 任何东西，可直接跑）。
 *
 * 为什么单独成文件：这是文件树与编辑器之间的一条规则——**删掉一个文件/目录后，指向它的
 * 标签必须一起关掉**。不关的话标签留在那儿，用户点回去得到的是"文件不存在"（读盘失败），
 * 而他刚刚才在 Nexus 里删掉它。写作 `FileTree` 里的一段内联 filter 也能跑，
 * 但它拉不出来、测不了，而这条规则的边界（目录按分量、两种分隔符、jar 虚拟路径）恰恰是最
 * 容易写错的地方。
 *
 * 与 `utils/fileTree.ts` 的 `isInside` 是两条不同的规则（那个管"定位该由哪棵树响应"），
 * 刻意没有合并：这边还多出大小写归一与 jar 虚拟路径两条，合并只会让两边都长出用不上的分支。
 */

/** 只取判定用得到的字段：不 import `FileTab`，本文件因此零依赖 */
interface PathHolder {
  path: string;
}

/**
 * 比较前的归一化：分隔符统一成 `/`，大小写统一成小写。
 *
 * 两条都不是洁癖，而是**同一个文件在本项目里确实会以多种写法出现**：
 * - 分隔符：树里的子路径是 `list_directory` 用「配置里的根**原样** + `/` + 子名」拼的，
 *   根上带的 `\` 会留在字符串里（`D:\work\proj/src/a.ts`）；而搜索结果一律是 `/`。
 * - 大小写：Windows 路径大小写不敏感，配置里写的 `d:\work` 与文件系统取回的 `D:\work`
 *   是同一个目录。
 */
function normalize(p: string): string {
  return p.replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase();
}

/**
 * 挑出指向 `path` 本身、或位于 `path` 目录之下的标签。
 *
 * 目录判定必须**按分量**：比的是 `base + '/'`，不是裸 `startsWith(base)` ——
 * 后者会让删 `src/a` 时把 `src/ab/c.ts` 的标签也一并关掉。
 */
export function tabsUnderPath<T extends PathHolder>(tabs: readonly T[], path: string): T[] {
  const base = normalize(path);
  // 空路径不做"任意前缀"式的匹配（base 为 '' 时 base + '/' 会命中所有绝对路径）
  if (!base) return [];
  return tabs.filter((t) => {
    const p = normalize(t.path);
    if (p === base || p.startsWith(base + '/')) return true;
    // jar 内条目：`jar://<jar 路径>!/<条目>`（见 stores/editor.ts 的 openJarEntry）。
    // 删掉磁盘上那个 jar，指向它内部条目的标签同样失效。
    //
    // 剥掉 `jar://` 之后必须按**同一套分量规则**再比一次（CQ-44）：原先只比
    // `jar://${base}!/`，于是"删掉装着 jar 的那个目录"命不中——`jar://D:/p/lib/app.jar!/a/B.class`
    // 既不以 `D:/p/lib` 开头、也不以 `D:/p/lib/` 开头，标签留着，用户点回去看到的正是
    // "文件不存在"——而"不留这种标签"就是本规则存在的全部理由
    if (!p.startsWith('jar://')) return false;
    // 取最外层那个 jar 的磁盘路径（嵌套 fat jar 形如 `jar://x.jar!/inner.jar!/a/B.class`）
    const jarPath = p.slice('jar://'.length).split('!/')[0] ?? '';
    return jarPath === base || jarPath.startsWith(base + '/');
  });
}

/**
 * 把一个路径按「oldPath 重命名为 newPath」映射到新路径；不受影响返回 `null`。
 *
 * 与 `tabsUnderPath` 共用同一套归一化（分隔符 + 大小写）与分量边界，但这里要**保留**
 * 标签原路径的后半段：重命名目录时，其下文件的标签要跟着换前缀，而不是被一并改成一个名字。
 *
 * jar 虚拟路径（`jar://<jar>!/<条目>`）只重写最外层 jar 的磁盘路径，条目名不变；
 * 重命名 jar 文件本身与重命名目录里的 jar 都走这里，否则标签点回去就是"文件不存在"。
 */
export function remapPath(
  path: string,
  oldPath: string,
  newPath: string,
  isDir: boolean,
): string | null {
  const p = normalize(path);
  const base = normalize(oldPath);
  if (!base) return null;
  if (p === base) return newPath;
  // 调用方可能带尾部斜杠（`tabsUnderPath` 允许）；切后缀时必须按去掉尾斜杠的长度，
  // 否则会把新前缀与后缀粘在一起
  const oldBase = oldPath.replace(/[/\\]+$/, '');
  if (isDir && p.startsWith(base + '/')) {
    // 用原字符串切后缀：标签路径可能混用两种分隔符，新前缀沿用调用方给的写法
    return newPath + path.slice(oldBase.length);
  }
  if (!p.startsWith('jar://')) return null;

  const rest = path.slice('jar://'.length);
  const sep = rest.indexOf('!/');
  if (sep < 0) return null;
  const jarPath = rest.slice(0, sep);
  const jarNorm = normalize(jarPath);
  if (jarNorm === base) {
    return `jar://${newPath}${rest.slice(sep)}`;
  }
  if (isDir && jarNorm.startsWith(base + '/')) {
    return `jar://${newPath}${jarPath.slice(oldBase.length)}${rest.slice(sep)}`;
  }
  return null;
}
