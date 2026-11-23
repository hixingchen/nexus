import { test } from 'node:test';
import assert from 'node:assert/strict';

// dsh 会话插件是原生 ESM（不在前端 tsconfig 的 include 里）；这里直接跑它的真实逻辑
// @ts-expect-error 无类型声明的 .js 插件
import { apply as applyNexusAiRestart } from '../../../src-tauri/src/scripts/nexus_ai_restart.js';

type Handler = (...args: unknown[]) => void;

interface Harness {
  calls: Array<{ url: string; init: RequestInit }>;
  result: (name: string, agent: string, args: unknown, value: unknown, isError?: boolean) => void;
  stop: (agent: string) => void;
  restore: () => void;
}

function harness(): Harness {
  const handlers: Record<string, Handler> = {};
  const calls: Array<{ url: string; init: RequestInit }> = [];
  const originalFetch = globalThis.fetch;

  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    calls.push({ url: String(input), init: init ?? {} });
    return new Response('{}', { status: 200 });
  }) as typeof fetch;

  applyNexusAiRestart(
    {
      logger: { warn: () => {} },
      on: (event: string, fn: Handler) => { handlers[event] = fn; },
    },
    { url: 'http://127.0.0.1:9/ai/edited', token: 'tok', cwd: CWD },
  );

  return {
    calls,
    result: (name, agent, args, value, isError = false) => {
      handlers['tools/result'](
        { name, agent: { id: agent }, arguments: args },
        isError ? { isError: true } : { isError: false, value },
      );
    },
    stop: (agent) => handlers['agent/turn-stopping']({ agent: { id: agent }, turn: 1 }),
    restore: () => { globalThis.fetch = originalFetch; },
  };
}

const tick = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

// `C:/proj` 在 POSIX 上不是绝对路径；fixture 必须按平台给绝对路径，
// 否则 path.resolve 会把 runner 的工作目录拼到前面（CI 的 Ubuntu runner 曾因此失败）。
const CWD = process.platform === 'win32' ? 'C:/proj' : '/proj';
const abs = (rel: string) => `${CWD}/${rel}`;

test('回合结束只上报未处理的写文件（write/edit/apply_patch，view 与失败忽略）', async () => {
  const h = harness();
  try {
    h.result('write', 'a', { file_path: abs('src/a.ts') }, { path: abs('src/a.ts') });
    h.result('edit', 'a', { file_path: 'src/b.ts' }, { path: abs('src/b.ts') });
    h.result('apply_patch', 'a', { input: '*** Begin Patch\n*** Update File: src/c.ts\n*** End Patch' }, {});
    h.result('str_replace_editor', 'a', { command: 'view', path: abs('x.ts') }, {});
    h.result('edit', 'a', { file_path: abs('fail.ts') }, {}, true);
    h.stop('a');
    await tick();

    assert.equal(h.calls.length, 1, '一次回合只发一个请求');
    const init = h.calls[0].init;
    const body = JSON.parse(String(init.body)) as { changed_files: string[]; source: string };
    assert.deepEqual(body.changed_files.sort(), [
      abs('src/a.ts'),
      abs('src/b.ts'),
      abs('src/c.ts'),
    ].sort());
    assert.equal(body.source, 'dsh-turn');
    assert.equal((init.headers as Record<string, string>).authorization, 'Bearer tok');
  } finally {
    h.restore();
  }
});

test('模型已调 restart_services_after_edit 时只补发它没传的文件', async () => {
  const h = harness();
  try {
    h.result('write', 'b', { file_path: abs('src/a.ts') }, { path: abs('src/a.ts') });
    h.result('write', 'b', { file_path: abs('src/b.ts') }, { path: abs('src/b.ts') });
    h.result(
      'mcp__nexus__restart_services_after_edit',
      'b',
      { changed_files: [abs('src/a.ts')] },
      { ok: true },
    );
    h.result('write', 'b', { file_path: abs('src/c.ts') }, { path: abs('src/c.ts') });
    h.stop('b');
    await tick();

    assert.equal(h.calls.length, 1);
    const body = JSON.parse(String(h.calls[0].init.body)) as { changed_files: string[] };
    assert.deepEqual(body.changed_files.sort(), [abs('src/b.ts'), abs('src/c.ts')]);
  } finally {
    h.restore();
  }
});

test('没有改动 / 工具失败时不误发', async () => {
  const h = harness();
  try {
    h.stop('empty');
    h.result('write', 'd', { file_path: abs('src/d.ts') }, { path: abs('src/d.ts') });
    h.result(
      'restart_services_after_edit',
      'd',
      { changed_files: [abs('src/d.ts')] },
      { ok: false },
      true,
    );
    h.stop('d');
    await tick();

    assert.equal(h.calls.length, 1, '空回合不发；工具失败时文件仍会补发');
    const body = JSON.parse(String(h.calls[0].init.body)) as { changed_files: string[] };
    assert.deepEqual(body.changed_files, [abs('src/d.ts')]);
  } finally {
    h.restore();
  }
});
