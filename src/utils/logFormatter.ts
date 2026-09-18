// ── 日志格式化工具 ─────────────────────────────────────────────

const ANSI_COLORS: Record<string, string> = {
  '30':'#000000','31':'#e74c3c','32':'#2ecc71','33':'#f39c12','34':'#3498db','35':'#9b59b6','36':'#1abc9c','37':'#ecf0f1',
  '90':'#95a5a6','91':'#e74c3c','92':'#2ecc71','93':'#f39c12','94':'#3498db','95':'#9b59b6','96':'#1abc9c','97':'#ffffff',
  '40':'#000000','41':'#e74c3c','42':'#2ecc71','43':'#f39c12','44':'#3498db','45':'#9b59b6','46':'#1abc9c','47':'#ecf0f1',
  '100':'#95a5a6','101':'#e74c3c','102':'#2ecc71','103':'#f39c12','104':'#3498db','105':'#9b59b6','106':'#1abc9c','107':'#ffffff',
};

const LOG_LEVEL_COLORS: Record<string, { color: string; fontWeight?: string }> = {
  ERROR:{color:'#f87171',fontWeight:'bold'},FATAL:{color:'#f87171',fontWeight:'bold'},CRITICAL:{color:'#f87171',fontWeight:'bold'},
  PANIC:{color:'#f87171',fontWeight:'bold'},EXCEPTION:{color:'#f87171',fontWeight:'bold'},FAILED:{color:'#f87171',fontWeight:'bold'},
  ERR:{color:'#f87171',fontWeight:'bold'},WARN:{color:'#fbbf24',fontWeight:'bold'},WARNING:{color:'#fbbf24',fontWeight:'bold'},
  INFO:{color:'#4ade80',fontWeight:'bold'},INFORMATION:{color:'#4ade80',fontWeight:'bold'},SUCCESS:{color:'#34d399',fontWeight:'bold'},
  DONE:{color:'#34d399'},OK:{color:'#34d399'},STARTED:{color:'#34d399'},START:{color:'#60a5fa'},
  STOP:{color:'#f87171'},STOPPED:{color:'#f87171'},DEBUG:{color:'#60a5fa'},TRACE:{color:'#94a3b8'},VERBOSE:{color:'#94a3b8'},
};

const TIMESTAMP_PATTERNS = [
  /\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?/,
  /\[\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?\]/,
  /\d{2}:\d{2}:\d{2}(?:\.\d+)?/,
  /[A-Z][a-z]{2}\s+\d{1,2}\s+\d{2}:\d{2}:\d{2}/,
];

/** 转义 HTML 特殊字符 */
function escapeHtml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#039;');
}

/** 处理单个 ANSI 参数并更新样式数组 */
function applyAnsiParam(param: number, styles: string[]): void {
  if (param === 0) { styles.length = 0; return; }
  if (param === 1) { styles.push('font-weight:bold'); return; }
  if (param === 2) { styles.push('opacity:0.7'); return; }
  if (param === 3) { styles.push('font-style:italic'); return; }
  if (param === 4) { styles.push('text-decoration:underline'); return; }
  if (param >= 30 && param <= 37) { removeStyle(styles, 'color:'); styles.push(`color:${ANSI_COLORS[String(param)]}`); return; }
  if (param >= 90 && param <= 97) { removeStyle(styles, 'color:'); styles.push(`color:${ANSI_COLORS[String(param)]}`); return; }
  if (param >= 40 && param <= 47) { removeStyle(styles, 'background-color:'); styles.push(`background-color:${ANSI_COLORS[String(param)]}`); return; }
  if (param >= 100 && param <= 107) { removeStyle(styles, 'background-color:'); styles.push(`background-color:${ANSI_COLORS[String(param)]}`); }
}

/** 用当前样式包裹文本片段 */
function wrapWithStyles(text: string, styles: string[]): string {
  const escaped = escapeHtml(text);
  return styles.length > 0 ? `<span style="${styles.join(';')}">${escaped}</span>` : escaped;
}

/** 将 ANSI 转义序列转换为 HTML */
function ansiToHtml(text: string): string {
  if (!text.includes('\x1b[')) return smartColorize(text);

  // eslint-disable-next-line no-control-regex -- ESC(\x1b) 就是 ANSI 转义序列的起始字节，不是笔误
  const ansiRegex = /\x1b\[([0-9;]*)m/g;
  let result = '';
  let lastIndex = 0;
  const styles: string[] = [];
  let match: RegExpExecArray | null;

  while ((match = ansiRegex.exec(text)) !== null) {
    if (match.index > lastIndex) {
      result += wrapWithStyles(text.slice(lastIndex, match.index), styles);
    }
    for (const param of match[1].split(';').map(Number)) {
      applyAnsiParam(param, styles);
    }
    lastIndex = ansiRegex.lastIndex;
  }

  if (lastIndex < text.length) {
    result += wrapWithStyles(text.slice(lastIndex), styles);
  }

  return result;
}

/** 移除指定前缀的样式 */
function removeStyle(styles: string[], prefix: string) {
  const idx = styles.findIndex(s => s.startsWith(prefix));
  if (idx !== -1) styles.splice(idx, 1);
}

type PlaceholderFn = (html: string) => string;

/** 匹配 URL 并替换为带样式的占位符 */
function colorizeUrls(text: string, addPh: PlaceholderFn): string {
  return text.replace(/(https?:\/\/[^\s<>"]+)/g, match =>
    addPh(`<span style="color:#7dd3fc;text-decoration:underline">${escapeHtml(match)}</span>`));
}

/** 匹配 IP 地址并替换为带样式的占位符 */
function colorizeIpAddresses(text: string, addPh: PlaceholderFn): string {
  return text.replace(/\b(\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}(?::\d+)?)\b/g, match =>
    addPh(`<span style="color:#5eead4">${escapeHtml(match)}</span>`));
}

/** 匹配时间戳并替换为带样式的占位符 */
function colorizeTimestamps(text: string, addPh: PlaceholderFn): string {
  let result = text;
  for (const pattern of TIMESTAMP_PATTERNS) {
    result = result.replace(pattern, match =>
      addPh(`<span style="color:#94a3b8">${escapeHtml(match)}</span>`));
  }
  return result;
}

/** 匹配日志级别关键字并替换为带样式的占位符 */
function colorizeLogLevels(text: string, addPh: PlaceholderFn): string {
  return text.replace(
    /\b(ERROR|FATAL|CRITICAL|PANIC|EXCEPTION|WARN(?:ING)?|INFO(?:RMATION)?|DEBUG|TRACE|VERBOSE|SUCCESS|OK|DONE|START(?:ED)?|STOP(?:PED)?|FAILED|ERR)\b/gi,
    (match) => {
      const config = LOG_LEVEL_COLORS[match.toUpperCase()];
      if (!config) return match;
      const style = [`color:${config.color}`];
      if (config.fontWeight) style.push(`font-weight:${config.fontWeight}`);
      return addPh(`<span style="${style.join(';')}">${escapeHtml(match)}</span>`);
    }
  );
}

/**
 * 路径字符 / 单词字符判定（`file:line` 扫描用，避免正则回溯）。
 *
 * 注意 `\w` 只含字母数字下划线——**不含** `/` 与 `\`（这不是笔误：`isWordChar` 不能复用
 * `isPathChar`）。差分模糊测试第一轮就抓到了这个错误：把 `/` `\` 也算进 `\w` 后，
 * `/.Z.\:1\.90/\:` 这类输入会多匹配出一个 `Z.\:1`，与原正则不等价。
 */
const isPathChar = (c: string): boolean =>
  (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || c === '/' || c === '\\';
const isWordChar = (c: string): boolean =>
  (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c === '_';

/**
 * 高亮 `路径/文件.ext:行号`（如 `src/App.tsx:42`）。
 *
 * 为什么不用正则 `([a-zA-Z\/\\]+\.\w+:\d+)`：它在"一长串路径字符但不含点"上会退化——
 * 贪婪的 `[a-zA-Z/\\]+` 在每个起始位置都要扫到行尾再回溯找 `\.`，实测 32KB 全字母输入
 * 耗时 **2.2 秒**（日志单行上限 8KB 时约 140ms/行，足以让日志面板在搜索时卡死）。
 * 手工扫描是严格线性的：只在遇到 `.` 时向左/向右看，左侧一旦定界就不再回退。
 * 与正则的等价性由 `__tests__/logFormatter.test.ts` 的差分用例守住。
 */
function colorizeFileLineRefs(text: string, addPh: PlaceholderFn): string {
  let out = '';
  let copied = 0; // 已原样带出的前缀长度（也是左侧扫描的下界：已处理过的文本不参与新匹配）
  let i = 0;
  while (i < text.length) {
    if (text[i] !== '.') { i++; continue; }
    // 左：至少一个路径字符
    let s = i;
    while (s > copied && isPathChar(text[s - 1])) s--;
    if (s === i) { i++; continue; }
    // 右：\w+ 后必须紧跟 `:数字`
    let j = i + 1;
    while (j < text.length && isWordChar(text[j])) j++;
    if (j === i + 1 || text[j] !== ':') { i++; continue; }
    let k = j + 1;
    while (k < text.length && text[k] >= '0' && text[k] <= '9') k++;
    if (k === j + 1) { i++; continue; }
    out += text.slice(copied, s) + addPh(`<span style="color:#c4b5fd">${escapeHtml(text.slice(s, k))}</span>`);
    copied = k;
    i = k;
  }
  return copied === 0 ? text : out + text.slice(copied);
}

/** 匹配引号字符串、文件路径、括号并替换为带样式的占位符 */
function colorizeSyntax(text: string, addPh: PlaceholderFn): string {
  let result = text;
  // 引号字符串：线性复杂度的转义感知匹配（原惰性回溯模式在引号未闭合的超长行上会卡死主线程）
  result = result.replace(/"[^"\\]*(?:\\.[^"\\]*)*"/g, match =>
    addPh(`<span style="color:#fcd34d">${escapeHtml(match)}</span>`));
  result = result.replace(/'[^'\\]*(?:\\.[^'\\]*)*'/g, match =>
    addPh(`<span style="color:#fcd34d">${escapeHtml(match)}</span>`));
  result = colorizeFileLineRefs(result, addPh);
  result = result.replace(/([{}[\]])/g, match =>
    addPh(`<span style="color:#64748b">${escapeHtml(match)}</span>`));
  return result;
}

/**
 * 两种占位符各用一套前缀，**必须分开还原**：
 * - `\0N\0`  着色内部（`smartColorize`）
 * - `\x01N\x01` 搜索高亮（`renderLine`，由 renderLine 自己还原）
 * 若共用一个正则，`smartColorize` 的还原会把搜索结果占位符也吃掉（索引串了数组）。
 */
// 哨兵选 \x00/\x01 是**设计**：它们不可能出现在日志文本里（读取管线会跳过含 NUL 的行），
// 因此与日志内容天然不冲突——用普通字符串做哨兵就得处理"日志里恰好出现同一串"的逃逸问题
/* eslint-disable no-control-regex -- 占位符哨兵用控制字符是设计，见上 */
const COLOR_PLACEHOLDER_RE = /\x00(\d+)\x00/g;
const MARK_PLACEHOLDER_RE = /\x01(\d+)\x01/g;
/* eslint-enable no-control-regex */

/**
 * 单趟还原占位符。
 *
 * 为什么不是逐个 `split(id).join(html)`（原实现）：那是「匹配数 × 行长」——一行的匹配越多
 * 越慢。实测单字符搜索词在 4KB 行上有 4000 个匹配 → **1.2 秒/行**，日志面板会直接卡死；
 * 无搜索时的括号/URL 等着色同理（JSON dump 行有上千个括号）。单趟扫一遍即还原完。
 *
 * 用 `replace(正则, 函数)` 而非替换串：函数返回值按**字面**插入，不解释 `$&`/`` $` ``/`$'`
 * ——日志内容（如 echo 出来的 sed 命令）含 `$` 时才不会被破坏，这也是原实现不敢用替换串的原因。
 */
function restorePlaceholders(html: string, out: string[], re: RegExp): string {
  return html.replace(re, (m, idx: string) => out[Number(idx)] ?? m);
}

/** 无 ANSI 转义时的智能着色 */
function smartColorize(text: string): string {
  if (!text.trim()) return escapeHtml(text);

  const placeholders: string[] = [];
  const addPh: PlaceholderFn = (html) => {
    const id = `\0${placeholders.length}\0`;
    placeholders.push(html);
    return id;
  };

  let result = colorizeUrls(text, addPh);
  result = colorizeIpAddresses(result, addPh);
  result = colorizeTimestamps(result, addPh);
  result = colorizeLogLevels(result, addPh);
  result = colorizeSyntax(result, addPh);

  result = escapeHtml(result);
  return restorePlaceholders(result, placeholders, COLOR_PLACEHOLDER_RE);
}

const SEARCH_MARK_HTML = '<mark style="background:rgba(251,191,36,0.2);color:#fcd34d;padding:1px 3px;border-radius:3px;border:1px solid rgba(251,191,36,0.3)">';

/** 转义正则特殊字符 */
function escapeRegExp(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/**
 * 渲染单行日志。
 * 搜索高亮在**文本层**插入占位符（\x01N\x01，不与 smartColorize 内部的 \x00N\x00
 * 占位符冲突，且不参与着色正则匹配、不被 escapeHtml 转义），着色完成后再恢复为
 * <mark>——避免直接对已生成的 HTML 做正则高亮时把 mark 注入到
 * <span style="..."> 标签内部（单字符/短词搜索会污染渲染）。
 */
export function renderLine(line: string, searchTerm: string): string {
  if (!searchTerm?.trim()) {
    return line.includes('\x1b[') ? ansiToHtml(line) : smartColorize(line);
  }
  // 文本层高亮 → 占位符 → 着色 → 恢复 mark
  const marks: string[] = [];
  const re = new RegExp(escapeRegExp(searchTerm), 'gi');
  const marked = line.replace(re, (m) => {
    const id = `\x01${marks.length}\x01`;
    marks.push(`${SEARCH_MARK_HTML}${escapeHtml(m)}</mark>`);
    return id;
  });
  const html = marked.includes('\x1b[') ? ansiToHtml(marked) : smartColorize(marked);
  // 单趟还原（同上：字面插入，避免搜索词/日志内容里的 $ 模式污染）
  return restorePlaceholders(html, marks, MARK_PLACEHOLDER_RE);
}
