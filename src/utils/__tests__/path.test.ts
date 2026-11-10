import { test } from 'node:test';
import assert from 'node:assert/strict';

import { getExtension, parentDir, baseName } from '../path.ts';

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

/**
 * parentDir 服务的是「粘贴到同级」：右键一个**文件**时，粘贴目标是它所在的目录。
 * 切错了就会变成一个不存在的路径，粘贴直接失败——而这在界面上只表现为"点了没反应"。
 */
test('parentDir：全 `/` 的路径', () => {
  assert.equal(parentDir('D:/work/proj/src/a.ts'), 'D:/work/proj/src');
});

test('parentDir：全 `\\` 的路径', () => {
  assert.equal(parentDir('D:\\work\\proj\\a.ts'), 'D:\\work\\proj');
});

test('parentDir：混用分隔符时取**最后**一个（树里最常见的形态）', () => {
  // 树路径 = 配置里的根原样（带 \）+ "/" + 子名，切错就切到根上去了
  assert.equal(parentDir('D:\\work\\proj/src/a.ts'), 'D:\\work\\proj/src');
  // 反过来：根是 /，子级用了 \（手工拼出来的路径）
  assert.equal(parentDir('D:/work/proj\\src\\a.ts'), 'D:/work/proj\\src');
});

test('parentDir：树根下的第一层文件，父目录就是树根本身', () => {
  assert.equal(parentDir('D:\\work\\proj/a.ts'), 'D:\\work\\proj');
  assert.equal(parentDir('D:\\work\\proj\\a.ts'), 'D:\\work\\proj');
});

test('parentDir：没有分隔符 / 以分隔符开头都不返回空串（空串会被当成"当前目录")', () => {
  assert.equal(parentDir('a.ts'), 'a.ts', '没有分隔符 → 原样返回，让下游明确报错');
  assert.equal(parentDir('/a.ts'), '/a.ts', '切出来是空串时退回原值');
});

/**
 * baseName 服务「路径变化 → 默认名称」：项目/服务表单在路径变化时用它自动填名称。
 * 取错会把名称改成路径中间的一段，而界面上很难发现（名称本来就可改）。
 */
test('baseName：Windows 路径取最后一段（中文目录名原样保留）', () => {
  assert.equal(baseName('D:/work/SVNProject/福州-福大至诚学院'), '福州-福大至诚学院');
  assert.equal(baseName('D:\\work\\SVNProject\\福州-福大至诚学院'), '福州-福大至诚学院');
});

test('baseName：混用分隔符时取最后一段', () => {
  assert.equal(baseName('D:\\work\\proj/src/foo'), 'foo');
  assert.equal(baseName('D:/work/proj\\src\\foo'), 'foo');
});

test('baseName：末尾分隔符被忽略（选择器偶尔会带结尾斜杠）', () => {
  assert.equal(baseName('D:/work/foo/'), 'foo');
  assert.equal(baseName('D:\\work\\foo\\'), 'foo');
  assert.equal(baseName('D:\\work\\foo\\\\'), 'foo');
});

test('baseName：根路径没有名字段 → 空串（不会把 "D:" 当名称）', () => {
  assert.equal(baseName('D:/'), '');
  assert.equal(baseName('D:\\'), '');
  assert.equal(baseName('D:'), '');
  assert.equal(baseName('/'), '');
  assert.equal(baseName(''), '');
});

test('baseName：UNC 共享目录取共享名，`.` / `..` 不算名称', () => {
  assert.equal(baseName('\\\\server\\share'), 'share');
  assert.equal(baseName('\\\\server\\share\\'), 'share');
  assert.equal(baseName('D:/work/.'), '');
  assert.equal(baseName('D:/work/..'), '');
});

test('baseName：没有分隔符时原样返回；首尾空白被去掉（名称提交时会 trim）', () => {
  assert.equal(baseName('foo'), 'foo');
  assert.equal(baseName('  foo  '), 'foo');
});
