import { test } from 'node:test';
import assert from 'node:assert/strict';

import { getDirColorClass } from '../fileColors.ts';

/**
 * 目录语义着色：内置集的目录语义完全相同（都是"生成物/依赖，不该手改"），
 * 因此共享同一个淡化样式。这里钉的是"分类正确"与"不误伤正常目录"，
 * 而不是具体类名——换主题改类名不该让测试红，误把 src/ 淡化就该红。
 */

const DIM_DIRS = [
  // 构建产物
  'target', 'dist', 'build', 'out',
  // 依赖
  'node_modules',
  // VCS / IDE
  '.git', '.idea', '.vscode',
  // 语言生态缓存
  '__pycache__', '.venv', 'venv', '.next', '.gradle', 'coverage',
];

test('内置生成物/依赖目录统一返回同一个非空样式（语义相同的目录不区别对待）', () => {
  const values = DIM_DIRS.map((name) => getDirColorClass(name));
  for (const [i, value] of values.entries()) {
    assert.equal(typeof value, 'string', `${DIM_DIRS[i]} 应命中内置集`);
    assert.ok(value!.length > 0, `${DIM_DIRS[i]} 不应返回空类名`);
  }
  assert.equal(new Set(values).size, 1, '内置集必须共用同一种淡化样式');
});

test('未收录的目录名返回 null，调用方据此不加类名（src/ 不能被淡化）', () => {
  const normal = ['src', 'components', 'utils', 'docs', 'assets', 'public', 'scripts', 'test'];
  for (const name of normal) {
    assert.equal(getDirColorClass(name), null, `${name} 不应命中内置集`);
  }
});

test('匹配是精确目录名、区分大小写：Target / NODE_MODULES / Dist 都不算命中', () => {
  // 规则是"精确目录名匹配"，一旦改成大小写不敏感或子串匹配，
  // 名为 Target 的业务目录会被误淡化。
  for (const name of ['Target', 'TARGET', 'Dist', 'NODE_MODULES', 'Node_Modules', 'Build', 'Coverage']) {
    assert.equal(getDirColorClass(name), null, `${name} 不应命中内置集`);
  }
});

test('前后缀不算命中：node_modules.bak / dist-old / mybuild 不淡化', () => {
  for (const name of ['node_modules.bak', 'node_modules2', 'dist-old', 'mybuild', 'outbox', 'venv2', 'git']) {
    assert.equal(getDirColorClass(name), null, `${name} 不应命中内置集`);
  }
});

test('空目录名返回 null（空串不是任何规则）', () => {
  assert.equal(getDirColorClass(''), null);
});

test('Object 原型上的名字不算命中（真缺陷回归：constructor / toString / __proto__）', () => {
  // 原实现 `DIR_COLORS[name] ?? null` 会沿原型链取到 Object 上的成员：`constructor` 返回
  // 构造函数（真值）、`__proto__` 返回 Object.prototype —— 调用点把它们当类名拼进
  // className，静默产生垃圾类名。这类目录名在真实工程里存在（如 hasOwnProperty.ts 所在目录）。
  for (const name of ['constructor', 'toString', 'valueOf', '__proto__', 'hasOwnProperty']) {
    assert.equal(getDirColorClass(name), null, `${name} 是 Object 原型成员，不能当命中`);
  }
});

test('返回值是可以直接塞进 className 的类名标记（不能是颜色值、选择器或空串）', () => {
  // 调用点是 className={getDirColorClass(name) ?? ''}，返回 ".text-nexus-muted"、
  // "#94a3b8" 这种"看着对但塞进去无效"的值会静默失效。
  for (const name of DIM_DIRS) {
    assert.match(getDirColorClass(name)!, /^[A-Za-z0-9_-]+$/, `${name} 的返回值不是合法类名`);
  }
});
