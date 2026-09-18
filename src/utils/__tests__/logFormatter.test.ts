import { test } from 'node:test';
import assert from 'node:assert/strict';

import { renderLine } from '../logFormatter.ts';

/**
 * renderLine 的返回值直接进 dangerouslySetInnerHTML，所以这里的每一组断言
 * 都在守一条真实的失败路径：XSS、ESC 泄漏、正则元字符、$ 替换模式、回溯卡死。
 */

/** 取出结果里所有 style 属性的值，用于检查"样式是替换不是叠加" */
function stylesOf(html: string): string[] {
  return [...html.matchAll(/style="([^"]*)"/g)].map((m) => m[1]);
}

// ── HTML 转义 ────────────────────────────────────────────────

test('尖括号被转义：日志内容不能变成可执行标签（innerHTML 注入）', () => {
  const html = renderLine('<img src=x onerror=alert(1)>', '');
  assert.ok(!html.includes('<img'), '原始标签不得出现在结果里');
  assert.ok(html.includes('&lt;img'));
  assert.ok(html.includes('&gt;'));
});

test('& 先于其它字符转义，已是实体的文本不会被二次解释', () => {
  assert.ok(renderLine('a & b', '').includes('a &amp; b'));
  assert.ok(renderLine('&lt;', '').includes('&amp;lt;'));
});

test('引号被转义，不能提前闭合 style 之类的属性', () => {
  const html = renderLine('a "b" and \'c\'', '');
  assert.ok(html.includes('&quot;b&quot;'));
  assert.ok(html.includes('&#039;c&#039;'));
  assert.ok(!/style="[^"]*"[^>]*"/.test(html), '结果里不应出现被引号截断的属性');
});

test('孤立代理项与控制字符不抛异常（日志可能来自任意字节流）', () => {
  const html = renderLine('ok \uD800 \uDFFF \u0000 \u0007 end', '');
  assert.equal(typeof html, 'string');
  assert.ok(html.includes('ok'));
  assert.ok(html.includes('end'));
});

// ── ANSI 转义 ────────────────────────────────────────────────

test('ANSI 序列被消费，原始 ESC 不泄漏到 DOM（文本保留、颜色生效）', () => {
  const html = renderLine('\x1b[31mred\x1b[0m', '');
  assert.ok(!html.includes('\x1b'), 'ESC 必须被吃掉');
  assert.ok(html.includes('color:#e74c3c'));
  assert.ok(html.includes('>red<'));
});

test('\\x1b[0m 之后样式真的被清掉（重置不是摆设）', () => {
  const html = renderLine('\x1b[31ma\x1b[0mb', '');
  assert.ok(html.endsWith('</span>b'), `期望 b 在 span 之外，实际：${html}`);
});

test('参数按顺序生效：\\x1b[31;0m 的颜色被后面的 0 清掉', () => {
  assert.equal(renderLine('\x1b[31;0mx', ''), 'x');
});

test('空参数 \\x1b[m 等价于重置（Number("") === 0 是既有行为，不是崩溃点）', () => {
  assert.ok(renderLine('\x1b[31ma\x1b[mb', '').endsWith('</span>b'));
});

test('复合参数（bold + 颜色）同时生效', () => {
  const html = renderLine('\x1b[1;31merr\x1b[0m', '');
  const style = stylesOf(html)[0] ?? '';
  assert.ok(style.includes('font-weight:bold'), `缺少加粗：${style}`);
  assert.ok(style.includes('color:#e74c3c'), `缺少颜色：${style}`);
});

test('同一段文本只保留一个前景色：重复设置是替换不是叠加', () => {
  const html = renderLine('\x1b[31ma\x1b[32mb', '');
  const styles = stylesOf(html);
  assert.equal(styles.length, 2, `期望两段各自成 span，实际：${html}`);
  for (const style of styles) {
    const colors = style.split(';').filter((part) => part.startsWith('color:'));
    assert.equal(colors.length, 1, `同一段出现多个前景色：${style}`);
  }
  assert.ok(styles[1].includes('#2ecc71'), '后设的颜色应生效');
});

test('前景色与背景色互不覆盖（两个样式槽独立）', () => {
  const style = stylesOf(renderLine('\x1b[41;32mx\x1b[0m', ''))[0] ?? '';
  assert.ok(style.includes('background-color:#e74c3c'), `缺少背景色：${style}`);
  assert.ok(style.includes('color:#2ecc71'), `缺少前景色：${style}`);
});

test('不支持的 SGR（256 色 / truecolor）被忽略：文本保留、无样式、不抛异常', () => {
  const html = renderLine('\x1b[38;5;196mhi\x1b[0m', '');
  assert.ok(html.includes('hi'));
  assert.ok(!html.includes('\x1b'));
  assert.ok(!html.includes('<span'), `不应凭空产生样式：${html}`);
});

test('ANSI 之前的文本按无样式输出，不被吞掉也不被包进 span', () => {
  const html = renderLine('plain\x1b[31mred', '');
  assert.ok(html.startsWith('plain'), `实际：${html}`);
  assert.ok(html.includes('<span style="color:#e74c3c">red</span>'));
});

test('行尾只有 ANSI 序列时不产生空 span', () => {
  assert.equal(renderLine('\x1b[31m', ''), '');
});

test('空行 / 纯空白行原样返回，不进入着色流程', () => {
  assert.equal(renderLine('', ''), '');
  assert.equal(renderLine('   ', ''), '   ');
});

// ── 无 ANSI 时的智能着色 ──────────────────────────────────────

test('URL 被着色且文本完整（含 ? 与 = 的查询串不能丢字符）', () => {
  const html = renderLine('see https://example.com/a?b=1 now', '');
  assert.ok(html.includes('#7dd3fc'));
  assert.ok(html.includes('https://example.com/a?b=1'));
});

test('IP[:端口] 被着色', () => {
  const html = renderLine('connected 10.0.0.1:8080 ok', '');
  assert.ok(html.includes('#5eead4'));
  assert.ok(html.includes('10.0.0.1:8080'));
});

test('时间戳被着色', () => {
  assert.ok(renderLine('2024-01-02 03:04:05 start', '').includes('#94a3b8'));
  assert.ok(renderLine('03:04:05 start', '').includes('#94a3b8'));
});

test('日志级别着色不区分大小写，且保留原文大小写', () => {
  const err = renderLine('ERROR boom', '');
  assert.ok(err.includes('#f87171'));
  assert.ok(err.includes('>ERROR<'), '原文大小写不得被改写');

  assert.ok(renderLine('warn slow', '').includes('#fbbf24'));
  assert.ok(renderLine('info ok', '').includes('#4ade80'));
});

test('单词边界生效：ERRORS / WARNINGLY 这类更长单词不被误着色', () => {
  assert.ok(!renderLine('ERRORS happened', '').includes('#f87171'));
  assert.ok(!renderLine('DEBUGGER attached', '').includes('#60a5fa'));
});

test('引号字符串、文件:行号、括号各自着色（语法高亮）', () => {
  const html = renderLine('at foo.ts:12 { "k": 1 }', '');
  assert.ok(html.includes('#c4b5fd'), `缺少路径高亮：${html}`);
  assert.ok(html.includes('#64748b'), `缺少括号高亮：${html}`);
  assert.ok(html.includes('#fcd34d'), `缺少字符串高亮：${html}`);
});

test('内容里的 $& / $` 不被当成替换模式（String.replace 会把它展开成整段匹配）', () => {
  const html = renderLine('url https://a.example/p and $& plus $` end', '');
  assert.ok(html.includes('$&amp;'), '内容里的 $& 应被转义保留，而不是被展开');
  assert.ok(html.includes('$`'), '内容里的 $` 应原样保留');
  assert.equal((html.match(/#7dd3fc/g) ?? []).length, 1, 'URL 只应着色一次');
});

// ── 搜索高亮 ─────────────────────────────────────────────────

test('搜索词按字面量匹配："." 不能当通配符把每个字符都高亮', () => {
  assert.ok(!renderLine('abc', '.').includes('<mark'));
  assert.equal((renderLine('a.b', '.').match(/<mark/g) ?? []).length, 1);
});

test('搜索词含正则元字符时不抛 SyntaxError（"+" 未转义会让 new RegExp 直接抛）', () => {
  assert.doesNotThrow(() => renderLine('a+b', '+'));
  assert.ok(renderLine('a+b', '+').includes('<mark'));
  assert.doesNotThrow(() => renderLine('a[b', '['));
});

test('搜索高亮忽略大小写，但输出保留原文大小写', () => {
  const html = renderLine('Hello World', 'world');
  assert.match(html, /<mark[^>]*>World<\/mark>/, `实际：${html}`);
});

test('每一处出现都高亮，不只是第一处', () => {
  assert.equal((renderLine('a b a b a', 'a').match(/<mark/g) ?? []).length, 3);
});

test('搜索词为空或纯空白时不产生高亮（否则每行都会被塞满 mark）', () => {
  assert.ok(!renderLine('anything', '').includes('<mark'));
  assert.ok(!renderLine('anything', '   ').includes('<mark'));
});

test('空行配非空搜索词返回空串，不抛异常', () => {
  assert.equal(renderLine('', 'needle'), '');
});

test('高亮不会注入到 style 属性内部（对已生成的 HTML 做正则高亮就会这样）', () => {
  // "color" 只存在于生成出来的 style="color:#7dd3fc" 里，文本层没有它。
  // 直接对 HTML 做高亮会产出 <span style="<mark ...>color</mark>:#7dd3fc"> 这种破标签。
  const html = renderLine('see https://example.com now', 'color');
  assert.ok(!html.includes('<mark'), `不应有高亮，实际：${html}`);
  assert.ok(!/<span style="[^"]*<mark/.test(html), '标签结构被破坏');
  assert.ok(html.includes('#7dd3fc'), 'URL 仍应正常着色');
});

test('高亮与着色共存：命中搜索词的普通文本被 mark，同行其它关键词仍着色', () => {
  const html = renderLine('ERROR: disk full', 'disk');
  assert.ok(html.includes('#f87171'), `ERROR 应仍被着色：${html}`);
  assert.match(html, /<mark[^>]*>disk<\/mark>/);
});

test('搜索词自身含 $& 时，mark 里的内容不被展开', () => {
  const html = renderLine('a $& b', '$&');
  assert.match(html, /<mark[^>]*>\$&amp;<\/mark>/, `实际：${html}`);
});

// ── 超长输入 ─────────────────────────────────────────────────

test('未闭合引号 + 超长行不触发灾难性回溯（原惰性回溯模式会卡死主线程）', () => {
  // 实测 10k 字符约 70ms；这里的 2s 上限是"退回指数级回溯"的探针，
  // 不是性能指标（该路径实为 O(n²)，已单独记录）。
  const line = `"${'a'.repeat(10_000)}`;
  const started = Date.now();
  const html = renderLine(line, '');
  const elapsed = Date.now() - started;
  assert.ok(elapsed < 2000, `渲染耗时 ${elapsed}ms，疑似回溯`);
  assert.ok(html.includes('a'.repeat(100)), '内容不得被丢弃');
});

test('长行 + 单字符搜索：全部命中且保持线性（占位符还原必须是单趟）', () => {
  // 原实现每个命中各做一次 `split(id).join(html)`，是「命中数 × 行长」：实测 4k 字符
  // 单字符搜索 **1230ms/行**，日志面板搜索时会卡死。改为单趟还原后 ~2.6ms。
  // 下面的阈值对二次实现是必红的（同输入旧实现 >1000ms），对线性实现有百倍余量。
  const line = 'x'.repeat(4_000);
  const started = Date.now();
  const html = renderLine(line, 'x');
  const elapsed = Date.now() - started;
  assert.ok(elapsed < 300, `渲染耗时 ${elapsed}ms，占位符还原疑似退回逐趟替换`);
  assert.equal((html.match(/<mark/g) ?? []).length, 4_000, '每一处都应高亮');
  assert.ok(!html.includes('\x01'), '占位符不得残留到结果里');
});

test('file:line 着色保持线性：一长串路径字符但没有点时不能退化成 O(n²)', () => {
  // 正则 `([a-zA-Z\/\\]+\.\w+:\d+)` 的贪婪回溯在每个起始位置都要扫到行尾：实测 32KB
  // 全字母输入 **2.2 秒**（8KB 单行 ≈140ms，正是日志单行上限）。手写扫描后 ~0.2ms。
  const line = 'a'.repeat(32_000);
  const started = Date.now();
  const html = renderLine(line, '');
  const elapsed = Date.now() - started;
  assert.ok(elapsed < 300, `渲染耗时 ${elapsed}ms，file:line 匹配疑似退回正则回溯`);
  assert.ok(html.includes('a'.repeat(100)), '内容不得被丢弃');
});

test('file:line 匹配边界与正则语义一致（差分模糊测试发现过的形状）', () => {
  const purple = (s: string) => renderLine(s, '').includes(`>${s}</span>`);
  // 命中
  assert.ok(renderLine('at foo.ts:12', '').includes('>foo.ts:12</span>'), '标准形状应命中');
  assert.ok(renderLine('C:\\src\\A.java:9', '').includes('A.java:9</span>'), '反斜杠路径应命中');
  // `\w` 不含 `/` 与 `\`：`Z.\:1` 这类形状在原正则下**不**命中，手工扫描必须一致
  // （差分模糊测试第一轮就是在这里对不上：把路径字符也算进 `\w` 会多匹配）
  const html = renderLine('/.Z.\\:1\\.90/\\:', '');
  assert.ok(!html.includes('#c4b5fd'), `不该命中却着色了：${html}`);
  // 分隔符不齐的形状同样不命中
  assert.ok(!renderLine('no-colon.ts', '').includes('#c4b5fd'));
  assert.ok(!renderLine('digits.ts:', '').includes('#c4b5fd'));
  assert.ok(!renderLine('.ts:1', '').includes('#c4b5fd'), '点左侧必须有路径字符');
  assert.ok(purple('a.b:1'), '单字符路径名也应命中（原正则允许）');
});
