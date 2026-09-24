/**
 * 「测试套件不许空跑」的下限守卫（审计 NEW-23），由 package.json 的 `test` 脚本在
 * `node --test` **之前**调用。
 *
 * 为什么必须在 glob 之外单独跑一遍：`node --test "<glob>"` 在匹配到 0 个文件时**退出码是 0**
 * （输出 `# tests 0` / `# pass 0`），本项目实测确认。于是把测试文件挪出 `__tests__/`、
 * 或按常见习惯放到 `src/utils/foo.test.ts`，整个前端测试套件会静默归零而 CI 照样绿。
 * 而"写在 `src/**\/__tests__/*.test.ts` 里的自检用例"抓不到这一点——glob 一旦匹配不到文件，
 * 它自己也不会被执行（守卫自己也在被守的范围里）。所以文件数这一层只能放在 glob 外。
 *
 * 与 `src/utils/__tests__/testSuiteGuard.test.ts` 的关系：那个文件是同一口径的**内层**副本，
 * 额外钉住"几个已知测试文件确实在 glob 里"（文件名集合变化时它会红）。两处下限取严的那条生效，
 * 因此副本过期只会造成可见的红，不会悄悄放宽门禁。
 *
 * 基线是**下限不是目标**：清掉一个测试文件后要顺手把它调小——这步摩擦是刻意的，
 * 与 `cargo clippy` 的 `--max-warnings` 递减基线同一套做法。
 */
import { readdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const SRC = path.join(ROOT, 'src');

/** 约定目录：与 test 脚本的 glob、以及内层守卫同一口径 */
const CONVENTION_DIR = '__tests__';
/** 最低测试文件数（低于它就是"有整套用例离开了约定目录"） */
const MIN_TEST_FILES = 18;

/** 递归收集 `src/**\/__tests__/*.test.ts`：任意一层 `__tests__` 目录**直属**的 `.test.ts` */
function collectTestFiles(dir, out = []) {
  const inConventionDir = path.basename(dir) === CONVENTION_DIR;
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) collectTestFiles(full, out);
    // `*.test.ts` 不匹配 `.test.tsx`（与 glob 一致）
    else if (inConventionDir && entry.name.endsWith('.test.ts')) out.push(full);
  }
  return out;
}

const files = collectTestFiles(SRC)
  .map((p) => path.relative(ROOT, p).split(path.sep).join('/'))
  .sort();

if (files.length < MIN_TEST_FILES) {
  console.error(
    `[test-suite-guard] 只找到 ${files.length} 个 src/**/${CONVENTION_DIR}/*.test.ts，`
    + `低于下限 ${MIN_TEST_FILES}——多半有用例离开了约定目录（搬回去，或确实要减少时下调 `
    + `test/test-suite-guard.mjs 的 MIN_TEST_FILES 与 src/utils/__tests__/testSuiteGuard.test.ts 的下限）。`,
  );
  console.error('找到的文件：\n  ' + files.join('\n  '));
  process.exit(1);
}

console.log(`[test-suite-guard] src/**/${CONVENTION_DIR}/*.test.ts 共 ${files.length} 个（下限 ${MIN_TEST_FILES}）`);
