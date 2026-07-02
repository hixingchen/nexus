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

/** 匹配引号字符串、文件路径、括号并替换为带样式的占位符 */
function colorizeSyntax(text: string, addPh: PlaceholderFn): string {
  let result = text;
  result = result.replace(/(["'])(?:(?=(\\?))\2.)*?\1/g, match =>
    addPh(`<span style="color:#fcd34d">${escapeHtml(match)}</span>`));
  result = result.replace(/([a-zA-Z\/\\]+\.\w+:\d+)/g, match =>
    addPh(`<span style="color:#c4b5fd">${escapeHtml(match)}</span>`));
  result = result.replace(/([{}[\]])/g, match =>
    addPh(`<span style="color:#64748b">${escapeHtml(match)}</span>`));
  return result;
}

/** 无 ANSI 转义时的智能着色 */
function smartColorize(text: string): string {
  if (!text.trim()) return escapeHtml(text);

  const placeholders: { id: string; html: string }[] = [];
  let counter = 0;
  const addPh: PlaceholderFn = (html) => {
    const id = `\0${counter++}\0`;
    placeholders.push({ id, html });
    return id;
  };

  let result = colorizeUrls(text, addPh);
  result = colorizeIpAddresses(result, addPh);
  result = colorizeTimestamps(result, addPh);
  result = colorizeLogLevels(result, addPh);
  result = colorizeSyntax(result, addPh);

  result = escapeHtml(result);
  for (const { id, html } of placeholders) {
    result = result.replace(id, html);
  }

  return result;
}

/** 搜索高亮（使用函数替换器确保 HTML 安全） */
function highlightSearch(text: string, term: string): string {
  if (!term.trim()) return text;
  const escaped = term.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return text.replace(
    new RegExp(`(${escaped})`, 'gi'),
    (_match, captured) => `<mark style="background:rgba(251,191,36,0.2);color:#fcd34d;padding:1px 3px;border-radius:3px;border:1px solid rgba(251,191,36,0.3)">${escapeHtml(captured)}</mark>`
  );
}

/** 渲染单行日志 */
export function renderLine(line: string, searchTerm: string): string {
  const html = line.includes('\x1b[') ? ansiToHtml(line) : smartColorize(line);
  return searchTerm ? highlightSearch(html, searchTerm) : html;
}
