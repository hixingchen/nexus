import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readdirSync, readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

/**
 * 测试套件的**计数下限**守卫（审计 NEW-23）。
 *
 * 守的是什么：`node --test "src/**\/__tests__/*.test.ts"` 匹配到 0 个文件时退出码也是 0
 * （实测 `# tests 0` / EXIT=0）。于是把用例挪出 `__tests__/`、或按常见习惯放到
 * `src/utils/foo.test.ts`，它再也不会被执行——而 CI 的前端测试步骤照样绿。
 * Rust 侧对同类风险有防范（`spawn_guard.rs` / `contract.rs` 的 `assert!(len >= N, "解析器很可能失效了")`），
 * 前端这条是补上同一层保险。
 *
 * 已知边界：glob 若整体匹配不到文件，**这个文件自己也不会被执行**——那一层由
 * `test/test-suite-guard.mjs`（跑在 glob 之外，见 package.json 的 test 脚本）兜住；
 * 这里额外钉"几个已知测试文件确实在列表里"，因为文件名集合变化时文件数可能不变。
 */

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..');
const CONVENTION_DIR = '__tests__';

/** 与 test/test-suite-guard.mjs 同一口径：任意一层 `__tests__` 目录**直属**的 `.test.ts` */
function collectTestFiles(dir: string, out: string[] = []): string[] {
  const inConventionDir = path.basename(dir) === CONVENTION_DIR;
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) collectTestFiles(full, out);
    // `*.test.ts` 不匹配 `.test.tsx`（与 glob 一致）
    else if (inConventionDir && entry.name.endsWith('.test.ts')) out.push(full);
  }
  return out;
}

const files = collectTestFiles(path.join(ROOT, 'src'))
  .map((p) => path.relative(ROOT, p).split(path.sep).join('/'))
  .sort();

test('约定目录下的测试文件数不低于基线（低于它 = 有用例整套离开了 glob 覆盖范围）', () => {
  // 下限，不是目标：清掉一个测试文件后应当顺手调小（见 test/test-suite-guard.mjs 的说明）
  assert.ok(
    files.length >= 18,
    `只扫到 ${files.length} 个 src/**/${CONVENTION_DIR}/*.test.ts，低于基线 18。`
    + `实际扫到：\n  ${files.join('\n  ')}`,
  );
});

test('几个关键用例文件确实在 glob 覆盖范围内（文件数不变时靠这条拦住改名/搬家）', () => {
  for (const required of [
    'src/stores/__tests__/editor.test.ts', // 编辑器草稿状态机（P0-6 的回归测试就在这）
    'src/stores/__tests__/pasteActions.test.ts',
    'src/utils/__tests__/pasteResult.test.ts',
    'src/utils/__tests__/serviceFailure.test.ts',
    'src/utils/__tests__/logFollow.test.ts', // 本文件同批新增（NEW-25）
  ]) {
    assert.ok(files.includes(required), `未在 glob 覆盖范围内：${required}`);
  }
});

test('本守卫文件自己也在列表里（它在约定目录内，不该被算漏）', () => {
  assert.ok(files.includes('src/utils/__tests__/testSuiteGuard.test.ts'));
});

test('glob 之外的计数守卫确实挂在 test 脚本上（少挂一步，上面两层一起失效）', () => {
  // 这一层守的是"配置写了 ≠ 生效了"：`test` 脚本被改写时，"glob 匹配 0 个文件"会重新变成静默通过
  const pkg = JSON.parse(readFileSync(path.join(ROOT, 'package.json'), 'utf8')) as { scripts: { test: string } };
  assert.match(pkg.scripts.test, /test\/test-suite-guard\.mjs/, 'package.json 的 test 脚本里没有前置计数守卫');
});
