import { test } from 'node:test';
import assert from 'node:assert/strict';
import { toAiError } from '../aiService';

/**
 * 后端错误 → 结构化的解析契约。
 *
 * 为什么值得单独钉：这条解析决定了错误卡上出现**哪个按钮**（「准备运行时」还是
 * 「重新安装」）。它的输入端是后端拼出来的字符串（`<码>::<人话>`）与对象
 * （`RuntimeFailure`），两端都是"改了不会报错"的东西——后端把 `【步骤】` 的括号换一种
 * 写法、或把 `\n\n` 换成单个换行，界面就会静默退化成"只有一个笼统摘要"，
 * 而这正是本轮事故里"重试多少次都一样、且不知道卡在哪"的形态。
 */

test('对象形态（ai_runtime_install 的 RuntimeFailure 载荷）', () => {
  const got = toAiError({
    step_index: 3,
    step: '安装 dsh（约 269MB）',
    summary: 'npm 安装 dsh 失败',
    raw: 'npm ERR! code ELIFECYCLE',
  });
  assert.equal(got.kind, 'runtime_broken');
  assert.equal(got.step, '安装 dsh（约 269MB）');
  assert.equal(got.summary, 'npm 安装 dsh 失败');
  assert.equal(got.raw, 'npm ERR! code ELIFECYCLE');
});

test('对象形态：step_index 为 null（与具体步骤无关的失败）不假装卡在某一步', () => {
  const got = toAiError({ step_index: null, step: '准备安装', summary: '已经在安装了', raw: '' });
  assert.equal(got.kind, 'runtime_broken');
  assert.equal(got.raw, '', '空原话要留空串，界面据此不显示"查看原始输出"');
});

test('字符串形态：运行时没准备（给「准备运行时」按钮的那一支）', () => {
  const got = toAiError('ai_runtime_missing::还没准备过，点下面的按钮下载一次即可。');
  assert.equal(got.kind, 'runtime_missing');
  assert.equal(got.step, null);
  assert.match(got.summary, /还没准备过/);
  assert.equal(got.raw, '');
});

test('字符串形态：装了但跑不起来（拆出步骤 + 人话 + 原话）', () => {
  const got = toAiError(
    'ai_runtime_broken::【验证能不能跑】装完了但 dsh 跑不起来，运行时未启用\n\n' +
    '命令成功退出但**没有任何版本号输出**（这正是 Node 版本不兼容时的形态）：\n（无输出）',
  );
  assert.equal(got.kind, 'runtime_broken');
  assert.equal(got.step, '验证能不能跑');
  assert.equal(got.summary, '装完了但 dsh 跑不起来，运行时未启用');
  assert.match(got.raw, /没有任何版本号输出/, '原话要完整保留，排障靠它');
  assert.ok(!got.raw.includes('【'), '原话里不该再夹着步骤标记');
});

test('字符串形态：没有原话时 raw 为空（而不是把摘要重复一遍）', () => {
  const got = toAiError('ai_runtime_broken::【下载 Node 运行时】所有下载源都失败了');
  assert.equal(got.step, '下载 Node 运行时');
  assert.equal(got.summary, '所有下载源都失败了');
  assert.equal(got.raw, '');
});

test('其余错误原样落 other，不误判成运行时问题', () => {
  for (const e of ['项目目录不存在: D:\\nope', new Error('IPC 断了'), '已取消：会话被更新的请求取代']) {
    const got = toAiError(e);
    assert.equal(got.kind, 'other', `${String(e)} 不该被当成运行时错误`);
    assert.equal(got.step, null);
    assert.equal(got.raw, '');
  }
});

test('没有码前缀但正文里含 :: 时不误判（码必须是已知的那两个）', () => {
  const got = toAiError('打开 http://x::y 失败');
  assert.equal(got.kind, 'other');
  assert.equal(got.summary, '打开 http://x::y 失败', '整句原样保留，不能被切掉前半截');
});
