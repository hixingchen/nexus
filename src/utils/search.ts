/**
 * 无分配的大小写不敏感子串查找。
 *
 * 为什么不用 `haystack.toLowerCase().includes(needle.toLowerCase())`：
 * 日志面板的搜索过滤每 50ms 就要对最多 2000 行执行一次，`toLowerCase()` 会为**每一行**
 * 分配一个临时字符串（40 次/秒 × 2000 行 = 8 万次分配/秒），而匹配结果本身几乎不变。
 * 这里只在必要时按字符比较，不产生任何分配。
 *
 * 语义与 `toLowerCase().includes()` 在常见场景（ASCII 日志）一致；对 Unicode 的
 * 特殊大小写映射（如土耳其语 İ、德语 ß）不做完整模拟——日志搜索不需要那种精度。
 */
export function includesIgnoreCase(haystack: string, needle: string): boolean {
  if (needle === '') return true;
  const n = needle.length;
  const last = haystack.length - n;
  if (last < 0) return false;
  for (let i = 0; i <= last; i++) {
    let j = 0;
    while (j < n && sameCharIgnoreCase(haystack.charCodeAt(i + j), needle.charCodeAt(j))) j++;
    if (j === n) return true;
  }
  return false;
}

/** 单字符比较：相同直接通过；否则用 ASCII 折叠（+32）比较 */
function sameCharIgnoreCase(a: number, b: number): boolean {
  if (a === b) return true;
  // A-Z ↔ a-z：小写字母 = 大写字母 + 32
  if (a >= 65 && a <= 90) a += 32;
  if (b >= 65 && b <= 90) b += 32;
  return a === b;
}
