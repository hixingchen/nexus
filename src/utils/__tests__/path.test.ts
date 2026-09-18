import { test } from 'node:test';
import assert from 'node:assert/strict';

import { getExtension } from '../path.ts';

/**
 * getExtension 刻意保持了各处原先 `split('.').pop() ?? ''` 的语义，
 * 这里把那个"看起来像 bug、其实是既有口径"的语义钉死——下游的
 * `=== 'jar'` 比较与 MIME 查表都是按它写的，改动它等于改契约。
 */

test('常规路径取最后一段扩展名', () => {
  assert.equal(getExtension('src/utils/path.ts'), 'ts');
});

test('多重点号只取最后一段：a.tar.gz → gz（不是 tar.gz）', () => {
  assert.equal(getExtension('archive.tar.gz'), 'gz');
});

test('无点路径返回整串（连目录一起）而不是空串或 basename——别按"无扩展名 = 空串"去用', () => {
  assert.equal(getExtension('Makefile'), 'Makefile');
  assert.equal(getExtension('src/Makefile'), 'src/Makefile');
});

test('以点结尾返回空串（"a." 与 "" 都是空串，两者不可区分）', () => {
  assert.equal(getExtension('a.'), '');
  assert.equal(getExtension(''), '');
});

test('隐藏文件：.gitignore → gitignore，点号开头也被当成扩展名', () => {
  assert.equal(getExtension('.gitignore'), 'gitignore');
});

test('目录名里的点会漏进扩展名：dir.d/file → "d/file"，不是空串', () => {
  // 这是 split/pop 的真实后果：对"无扩展名文件"做类型判定时必须自己再校验结果，
  // 不能假设返回值里没有 / 和 .
  assert.equal(getExtension('dir.d/file'), 'd/file');
  assert.equal(getExtension('a/b.c/'), 'c/');
});

test('默认原样返回大小写（EditorTabs 把扩展名直接交给 getIconSvg）', () => {
  assert.equal(getExtension('Foo.TS'), 'TS');
  assert.equal(getExtension('Foo.TS', {}), 'TS');
  assert.equal(getExtension('Foo.TS', { lower: false }), 'TS');
});

test('lower: true 折叠为小写（=== "class" / 语言包 / MIME 查表的调用方显式开启）', () => {
  assert.equal(getExtension('Foo.TS', { lower: true }), 'ts');
  assert.equal(getExtension('A.CLASS', { lower: true }), 'class');
});

test('lower: true 对非 ASCII 也生效，不只是 ASCII 折叠', () => {
  assert.equal(getExtension('x.Ä', { lower: true }), 'ä');
});

test('lower: true 对土耳其语 İ 产出两个码元（"i" + U+0307），下游 === "i" 会判不中', () => {
  assert.notEqual(getExtension('x.İ', { lower: true }), 'i');
  assert.equal(getExtension('x.İ', { lower: true }), 'İ'.toLowerCase());
});

test('超长路径不会退化（下游对每个文件项都调一次）', () => {
  const long = `${'a/'.repeat(50_000)}file.ts`;
  assert.equal(getExtension(long, { lower: true }), 'ts');
});
