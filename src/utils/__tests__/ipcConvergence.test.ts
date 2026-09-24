import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

/**
 * "IPC 面收敛在 `services/*`"这条规则的**机械化守卫**（铁律 20）。
 *
 * 为什么值得有：这是本轮审计里唯一一条**能用一条 grep 判定**的架构规则
 * （`grep -rn "invoke[<(]" src/ | grep -v '^src/services/'`），而它被破坏的方式是
 * "新命令照抄隔壁组件的写法"——不会编译报错、不会测试失败，只会在若干轮之后变成
 * "想枚举前端能调哪些后端命令，得翻遍 components"。
 *
 * 判据与审计方用的那条一致，并补上审计自己踩过的坑：**带泛型的写法是 `invoke<string>(`**，
 * 只 grep `invoke(` 会漏掉一半（SEC-30 就是被漏掉的那一处）。
 *
 * 例外必须登记在下面的 `EXEMPT` 里并写明理由——豁免不会过期，所以宁可没有例外。
 */

const SRC = path.resolve(import.meta.dirname, '../..');

/** 允许直接 invoke 的地方：只有服务层自己 */
const ALLOWED_DIR = path.join(SRC, 'services');

/** 逐条登记的例外（当前为空；加之前先想清楚"为什么不放进 services/"） */
const EXEMPT: readonly string[] = [];

/** 递归收集 src 下的 .ts/.tsx（跳过测试文件自身） */
function sourceFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === '__tests__') continue;
      out.push(...sourceFiles(full));
    } else if (/\.tsx?$/.test(entry.name)) {
      out.push(full);
    }
  }
  return out;
}

/** 去掉行注释/块注释后再找 —— 注释里写 `invoke(...)` 是在说明，不是在调用 */
function stripComments(text: string): string {
  return text.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\/\/[^\n]*/g, '');
}

test('直接 invoke 只允许出现在 src/services/ 下（其余一律走服务层）', () => {
  const files = sourceFiles(SRC);
  // 解析器可能失效的下限断言（同 contract.rs / spawn_guard.rs 的做法）：扫不到文件时
  // 这条守卫会"静默通过"，那比没有守卫更危险
  assert.ok(files.length >= 50, `只扫到 ${files.length} 个源文件，解析器很可能失效了`);

  const offenders: string[] = [];
  let sawServiceInvoke = false;
  for (const file of files) {
    const text = stripComments(fs.readFileSync(file, 'utf8'));
    if (!/\binvoke\s*[<(]/.test(text)) continue;
    const rel = path.relative(SRC, file).replace(/\\/g, '/');
    if (file.startsWith(ALLOWED_DIR)) {
      sawServiceInvoke = true;
      continue;
    }
    if (EXEMPT.includes(rel)) continue;
    offenders.push(rel);
  }

  assert.deepEqual(offenders, [], `这些文件绕过了服务层直接调 IPC：${offenders.join(', ')}`);
  // 反向确认：服务层里确实有 invoke（否则上面那条"没找到违规"只是因为扫描坏了）
  assert.ok(sawServiceInvoke, 'services/ 下一个 invoke 都没扫到，解析器很可能失效了');
});
