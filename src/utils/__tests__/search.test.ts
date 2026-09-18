import { test } from 'node:test';
import assert from 'node:assert/strict';

import { includesIgnoreCase } from '../search.ts';

/**
 * includesIgnoreCase 是日志面板搜索过滤的核心（每 50ms 对最多 2000 行执行），
 * 它的语义承诺是"与 toLowerCase().includes() 在常见场景一致"。
 * 这里既钉住它必须命中的场景，也钉住它**故意不命中**的 Unicode 场景，
 * 免得有人把"不命中"当 bug 去改成 toLowerCase()（那会重新引入每行的临时字符串分配）。
 */

test('空 needle 恒为真：与 includes("") 一致，空搜索框必须显示全部行', () => {
  assert.equal(includesIgnoreCase('', ''), true);
  assert.equal(includesIgnoreCase('anything', ''), true);
});

test('空 haystack 配非空 needle 为假：不能因为越界读成 undefined 就崩或误判', () => {
  assert.equal(includesIgnoreCase('', 'a'), false);
});

test('needle 比 haystack 长时直接为假（守卫 last < 0 分支）', () => {
  assert.equal(includesIgnoreCase('ab', 'abc'), false);
  assert.equal(includesIgnoreCase('abc', 'abc'), true);
});

test('ASCII 大小写不敏感：两个方向都要命中', () => {
  assert.equal(includesIgnoreCase('Hello World', 'hello'), true);
  assert.equal(includesIgnoreCase('hello world', 'WORLD'), true);
  assert.equal(includesIgnoreCase('HeLLo', 'hEllO'), true);
});

test('匹配位置不限于开头和结尾（末尾那一次必须被扫到）', () => {
  assert.equal(includesIgnoreCase('xxneedlexx', 'needle'), true);
  assert.equal(includesIgnoreCase('xxxneedle', 'needle'), true);
  assert.equal(includesIgnoreCase('needlexxx', 'needle'), true);
});

test('只差一个字符不算命中', () => {
  assert.equal(includesIgnoreCase('needle', 'needl'), true);
  assert.equal(includesIgnoreCase('needle', 'needles'), false);
  assert.equal(includesIgnoreCase('abcdef', 'abcdeg'), false);
});

test('大小写折叠只在 A-Z 生效：ASCII 标点不得被折叠（"[" (91) 与 "{" (123) 相差 32）', () => {
  // 若实现写成"无脑 ±32"，'[' 会等于 '{'、'\\' 会等于 '|'、']' 会等于 '}'。
  assert.equal(includesIgnoreCase('[', '{'), false);
  assert.equal(includesIgnoreCase('\\', '|'), false);
  assert.equal(includesIgnoreCase(']', '}'), false);
  assert.equal(includesIgnoreCase('^', '~'), false);
});

test('非 ASCII 完全相同的字符仍然命中（大小写无关不等于只认 ASCII）', () => {
  assert.equal(includesIgnoreCase('错误：找不到文件', '找不到'), true);
  assert.equal(includesIgnoreCase('café', 'café'), true);
  assert.equal(includesIgnoreCase('😀abc', 'abc'), true);
  assert.equal(includesIgnoreCase('😀😀', '😀'), true);
});

test('全角字母不命中半角（U+FF21 不在 65..90 内，不做 ±32 折叠）', () => {
  assert.equal(includesIgnoreCase('Ａ', 'ａ'), false);
});

test('非 ASCII 的大小写差异不命中：这是模块文档写明的取舍，不是 bug', () => {
  // 日志搜索不需要 Unicode 完整大小写映射；改成 toLowerCase() 会给每行
  // 增加一次字符串分配（40 次/秒 × 2000 行），是本函数存在的理由所在。
  assert.equal(includesIgnoreCase('Ä', 'ä'), false);
  assert.equal(includesIgnoreCase('İ', 'i'), false);
  assert.equal(includesIgnoreCase('STRASSE', 'straße'), false);
});

test('重叠出现的位置也会被扫到（i 每次只前进一位）', () => {
  assert.equal(includesIgnoreCase('aaaa', 'aa'), true);
  assert.equal(includesIgnoreCase('ababab', 'ABAB'), true);
});

test('needle 含大小写混合且横跨多段时仍命中', () => {
  assert.equal(includesIgnoreCase('xxAbCxx', 'aBc'), true);
});
