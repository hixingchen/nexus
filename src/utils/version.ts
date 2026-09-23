/**
 * 版本号比较（检查更新用）。
 *
 * 为什么要在前端再写一份：GitHub 的最新 release 是前端 `fetch` 拿的（CSP 只放开了
 * api.github.com 这一个域名，Rust 侧没有 HTTP 客户端），拿回来的 tag 得在本地比。
 * Rust 侧那份（`src-tauri/src/core/ai.rs` 的 `version_cmp`）用在 dsh 的升级检测上，
 * 语言不同没法直接复用——**但语义必须逐条对齐**，下面两条是重点：
 *
 * 1. **主段相等时，无预发布 > 有预发布**：`1.0.0 > 1.0.0-rc.1`。
 *    漏掉这条，"正式版发布了"永远判不出来。
 * 2. **预发布段内部要逐段比**：`1.0.5-rc.1 < 1.0.5-rc.2`。
 *    只看"有没有后缀"的话，同一个主段内的升级会被判成相等——这正是 dsh 那边踩过的坑
 *    （它发布的版本全是 rc/alpha，于是升级永远检测不到）。
 */

/** 解析失败时的返回值（与 Rust 的 `parse_version -> Option` 同义） */
const UNPARSABLE = Symbol('unparsable');

/** 预发布段的一节：`rc.2` → ['rc', 2] */
type PreId = number | string;

interface ParsedVersion {
  /** 主段，恒定补足 3 段（`1.2` → [1,2,0]） */
  core: [number, number, number];
  /** 预发布段（空数组 = 正式版） */
  pre: PreId[];
}

function parseVersion(raw: string): ParsedVersion | typeof UNPARSABLE {
  const s = raw.trim().replace(/^[vV]/, '');
  const dash = s.indexOf('-');
  const coreRaw = dash >= 0 ? s.slice(0, dash) : s;
  const preRaw = dash >= 0 ? s.slice(dash + 1) : '';
  if (!coreRaw) return UNPARSABLE;

  const parts = coreRaw.split('.');
  if (parts.some(p => p === '' || !/^\d+$/.test(p))) return UNPARSABLE;
  const nums = parts.map(Number);
  while (nums.length < 3) nums.push(0);

  // 预发布段：解析不出数字的当字母段（宽松——不为一个奇怪的后缀丢掉整个版本号）
  const pre = preRaw
    .split('.')
    .filter(p => p !== '')
    .map(p => (/^\d+$/.test(p) ? Number(p) : p.toLowerCase()));

  return { core: [nums[0], nums[1], nums[2]], pre };
}

/**
 * 规范化搜索词：去空白、去前导 `v`、去结尾的点、转小写。
 *
 * 单独导出是给界面用的——它要判"用户到底输没输东西"（只输了个 `v` 或一个点不算），
 * 而这个判断必须与筛选用的是**同一套口径**，否则会出现"看着没输什么，列表却只剩 200 条"。
 */
export function normalizeVersionQuery(raw: string): string {
  return raw.trim().toLowerCase().replace(/^[vV]/, '').replace(/\.+$/, '');
}

/**
 * 按搜索词筛「可安装的版本」（Node 版本面板的搜索框）。
 *
 * 规则按搜索词**看起来像什么**分，不是"两处都试试"：
 * - **像版本号**（`22` / `22.23` / `22.23.2`，允许前导 `v` 与结尾的点）→ 按**版本段前缀**匹配，
 *   且必须是整段：搜 `22` 命中 `v22.23.2`，搜 `23` **不**命中它——否则想看 23 系列的人
 *   得在一堆 22.23.x 里挑。
 * - **不像版本号**（`jod` / `Jod`）→ 按 LTS 代号匹配（代号是给人记的，包含即可、不分大小写）。
 *
 * 空搜索词返回全部（展示多少条由调用方决定）。
 */
export function matchNodeVersions<T extends { version: string; lts: string }>(
  items: T[],
  query: string,
): T[] {
  const q = normalizeVersionQuery(query);
  if (!q) return items;
  if (/^[\d.]+$/.test(q)) {
    return items.filter(it => {
      const v = it.version.replace(/^[vV]/, '');
      return v === q || v.startsWith(`${q}.`);
    });
  }
  return items.filter(it => it.lts.toLowerCase().includes(q));
}

/**
 * 比较两个版本号。
 *
 * @returns `>0` 表示 `a` 更新，`<0` 表示 `b` 更新，`0` 表示相等；
 *          **任一解析失败返回 `null`**（调用方据此提示"版本号无法识别"，
 *          而不是当成"没有新版本"——那会把解析失败伪装成"已是最新"）
 */
export function compareVersions(a: string, b: string): number | null {
  const pa = parseVersion(a);
  const pb = parseVersion(b);
  if (pa === UNPARSABLE || pb === UNPARSABLE) return null;

  for (let i = 0; i < 3; i++) {
    if (pa.core[i] !== pb.core[i]) return pa.core[i] - pb.core[i];
  }

  // 主段相等：无预发布 > 有预发布（见文件头第 1 条）
  if (pa.pre.length === 0 && pb.pre.length === 0) return 0;
  if (pa.pre.length === 0) return 1;
  if (pb.pre.length === 0) return -1;

  for (let i = 0; i < Math.max(pa.pre.length, pb.pre.length); i++) {
    const x = pa.pre[i];
    const y = pb.pre[i];
    // 公共前缀全同：段数少的更小（1.0.0-rc < 1.0.0-rc.1）
    if (x === undefined) return -1;
    if (y === undefined) return 1;
    if (typeof x === 'number' && typeof y === 'number') {
      if (x !== y) return x - y;
    } else if (typeof x === 'number') {
      return -1; // 数字段 < 字母段（语义化版本规则）
    } else if (typeof y === 'number') {
      return 1;
    } else if (x !== y) {
      return x < y ? -1 : 1;
    }
  }
  return 0;
}
