import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

/**
 * "读编辑器内容必须走唯一入口"这条规则的**机械化守卫**（ARCH-30 / 铁律 20）。
 *
 * 为什么值得有：内容的真相有两份存放处（活动标签在 store 的 `fileContent`、切走后进模块级
 * 草稿表），而**合帧窗口里的最新编辑两份都不在**（PERF-13，大文档攒 200ms 才写回）。
 * 于是"读内容"与"先把待物化内容结算掉"必须成对出现——原实现把这件事交给每个调用点自己记得，
 * 而漏一处的后果是**静默丢用户输入**（P0-6 正是这个结构失效的实例：不是漏了调用，
 * 而是"调用了但端口已被空结算吃掉"）。
 *
 * 收口之后：`readContent(tabId)` 是唯一读入口（内部先 flush），`flushPendingEdits()` 只留给
 * "没有读、纯落库"的两处（合帧定时器到点、编辑器销毁前抢救）。这条守卫盯两件事：
 * ① 已经不存在旧的 `settlePendingEdit`（改名后谁都别再用那个双职责的名字）；
 * ② 任何**在代码里读 `.fileContent`** 的文件，必须同时引用了读入口或 flush——
 *    否则就是"读了可能过期的内容"。
 *
 * 扫描是**启发式的**（先剥注释；发现即提醒，不是证明），与 `spawn_guard.rs`、`ipcConvergence`
 * 同一立场。例外必须登记在下面并写明理由。
 */

const SRC = path.resolve(import.meta.dirname, '../..');
/** 定义侧：它自己就是真相的持有者 */
const OWNER = path.join(SRC, 'stores', 'editor.ts');
/** 允许出现的例外（当前为空；加之前先想清楚"为什么这个读点不需要结算"） */
const EXEMPT: readonly string[] = [];

/** 递归收集 src 下的 .ts/.tsx（跳过测试目录） */
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

/** 剥掉注释：注释里提到 `.fileContent` 是在讲设计，不是在读它 */
function stripComments(code: string): string {
  return code
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/(^|[^:])\/\/[^\n]*/g, '$1');
}

test('旧的 settlePendingEdit 必须已消失（改名后它不再同时充当 flush 与读入口）', () => {
  const hits: string[] = [];
  for (const file of sourceFiles(SRC)) {
    if (stripComments(fs.readFileSync(file, 'utf8')).includes('settlePendingEdit')) {
      hits.push(path.relative(SRC, file));
    }
  }
  assert.deepEqual(hits, [], '这些文件还在用旧名字（读内容请用 readContent / 纯落库请用 flushPendingEdits）');
});

test('读 fileContent 的文件必须同时引用读入口或 flush', () => {
  const offenders: string[] = [];
  for (const file of sourceFiles(SRC)) {
    if (file === OWNER) continue;
    const rel = path.relative(SRC, file);
    if (EXEMPT.includes(rel)) continue;
    const code = stripComments(fs.readFileSync(file, 'utf8'));
    const readsContent = /\.fileContent\b/.test(code);
    if (!readsContent) continue;
    // 引用了入口（或它在同一文件里被定义）才算成对
    const settles = code.includes('readContent') || code.includes('flushPendingEdits');
    if (!settles) offenders.push(rel);
  }
  assert.deepEqual(
    offenders,
    [],
    '这些文件读了 .fileContent 却没有先结算未合帧的编辑——大文档的编辑还活在合帧窗口里，' +
      '读到的可能是旧内容（写盘/渲染都会错）。改法：走 stores/editor 的 readContent(tabId)，' +
      '或在读之前 flushPendingEdits() 并说明为什么这里是纯落库',
  );
});
