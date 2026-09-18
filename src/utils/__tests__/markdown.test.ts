import { test } from 'node:test';
import assert from 'node:assert/strict';

import { isMarkdownFile, resolveMdLink, markdownImageSrc, headingSlug } from '../markdown.ts';

/**
 * 预览的纯逻辑。这里的每条断言都对应一处**真实风险**：
 * 点错链接会把 webview 导航走、相对路径算错会打开不存在的文件、图片读错路径会白跑 IPC。
 */

test('只有 md 系列扩展名走预览（大小写不敏感，裸名不算）', () => {
  assert.equal(isMarkdownFile('D:/p/README.md'), true);
  assert.equal(isMarkdownFile('D:/p/docs/guide.MARKDOWN'), true);
  assert.equal(isMarkdownFile('D:\\p\\a\\b.mkd'), true);
  assert.equal(isMarkdownFile('D:/p/index.ts'), false);
  assert.equal(isMarkdownFile('D:/p/md'), false, '没有扩展名不算');
  assert.equal(isMarkdownFile('D:/p/a.md.bak'), false);
});

test('外链只放行 http/https/mailto，其余协议一律忽略', () => {
  assert.deepEqual(resolveMdLink('https://example.com/a', 'D:/p/README.md'), { kind: 'external', url: 'https://example.com/a' });
  assert.deepEqual(resolveMdLink('http://a.b', 'D:/p/README.md'), { kind: 'external', url: 'http://a.b' });
  assert.deepEqual(resolveMdLink('mailto:x@y.com', 'D:/p/README.md'), { kind: 'external', url: 'mailto:x@y.com' });
  // 危险协议：点了不能让 webview 执行/导航
  assert.equal(resolveMdLink('javascript:alert(1)', 'D:/p/README.md'), null);
  assert.equal(resolveMdLink('data:text/html,<script>x</script>', 'D:/p/README.md'), null);
  assert.equal(resolveMdLink('file:///C:/Windows/System32/calc.exe', 'D:/p/README.md'), null);
  assert.equal(resolveMdLink('//evil.com/a', 'D:/p/README.md'), null, '协议相对 URL 也不放行');
});

test('锚点留在预览内滚动', () => {
  assert.deepEqual(resolveMdLink('#安装', 'D:/p/README.md'), { kind: 'anchor', id: '安装' });
  assert.equal(resolveMdLink('#', 'D:/p/README.md'), null);
});

test('相对链接按"md 文件所在目录"解析成绝对路径', () => {
  assert.deepEqual(
    resolveMdLink('./docs/guide.md', 'D:/work/p/README.md'),
    { kind: 'local', path: 'D:/work/p/docs/guide.md' },
  );
  assert.deepEqual(
    resolveMdLink('../other/x.md', 'D:/work/p/docs/README.md'),
    { kind: 'local', path: 'D:/work/p/other/x.md' },
  );
  assert.deepEqual(
    resolveMdLink('img/a.png', 'D:/work/p/README.md'),
    { kind: 'local', path: 'D:/work/p/img/a.png' },
    '裸相对路径同样按所在目录解析',
  );
});

test('反斜杠路径保持反斜杠；query/fragment 不参与路径', () => {
  assert.deepEqual(
    resolveMdLink('sub\\b.md', 'D:\\work\\p\\README.md'),
    { kind: 'local', path: 'D:\\work\\p\\sub\\b.md' },
  );
  assert.deepEqual(
    resolveMdLink('./a.md#小节?x=1#y', 'D:/p/README.md'),
    { kind: 'local', path: 'D:/p/a.md' },
    'fragment 与 query 都要剥掉',
  );
});

test('空链接 / 纯空白返回 null（渲染成不可点的空锚点）', () => {
  assert.equal(resolveMdLink('', 'D:/p/README.md'), null);
  assert.equal(resolveMdLink('   ', 'D:/p/README.md'), null);
});

test('图片：data:image 直接用，相对路径读盘，其余交给 CSP/忽略', () => {
  assert.deepEqual(markdownImageSrc('data:image/png;base64,AAA', 'D:/p/README.md'), { kind: 'inline' });
  assert.deepEqual(
    markdownImageSrc('./img/a.png', 'D:/p/README.md'),
    { kind: 'local', path: 'D:/p/img/a.png' },
  );
  assert.deepEqual(markdownImageSrc('https://cdn.example/a.png', 'D:/p/README.md'), { kind: 'skip' });
  assert.deepEqual(markdownImageSrc('', 'D:/p/README.md'), { kind: 'skip' });
});

test('标题 id 近似 GitHub 规则（中文保留、标点去掉、空格转连字符）', () => {
  assert.equal(headingSlug('安装步骤'), '安装步骤', '中文标题原样保留');
  assert.equal(headingSlug('Step 1: Setup'), 'step-1-setup', '大写转小写、冒号去掉、空格转连字符');
  assert.equal(headingSlug('  Hello   World  '), 'hello-world', '连续空格合并、首尾空白忽略');
  assert.equal(headingSlug('中文 (English)'), '中文-english', '括号等标点去掉');
  assert.equal(headingSlug('a_b-c'), 'a_b-c', '下划线与连字符保留');
  assert.equal(headingSlug('###'), '', '全是标点则得到空串（调用方据此不设 id）');
});
