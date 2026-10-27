import { test } from 'node:test';
import assert from 'node:assert/strict';

import { watchPathsAfterCwdChange } from '../watchPaths.ts';

/**
 * 工作目录变化后的监听路径（`watchPathsAfterCwdChange`）。
 *
 * 守的是用户报过的真实场景：从模板快速添加服务 → 改工作目录 → 忘了改监听路径。
 * 模板带过来的 watch_paths 指向模板作者的目录，服务因此"看着在跑、改代码不重启"。
 */

test('未手动编辑过监听路径：工作目录一变就重置为跟随新目录', () => {
  assert.equal(
    watchPathsAfterCwdChange({ touched: false, nextCwd: 'D:/work/new-project' }),
    JSON.stringify(['D:/work/new-project']),
    '从模板带来的监听路径必须被新目录替换',
  );
});

test('模板的多个监听根同样被重置（不是只替换恰好等于旧 cwd 的那一种）', () => {
  // touched=false 的语义是"这值是随面板/模板来的，不是用户在本面板里配的"
  assert.equal(
    watchPathsAfterCwdChange({ touched: false, nextCwd: 'D:/work/b' }),
    JSON.stringify(['D:/work/b']),
  );
});

test('工作目录清空：监听路径回到空数组（交给后端回落到 cwd）', () => {
  assert.equal(watchPathsAfterCwdChange({ touched: false, nextCwd: '' }), '[]');
});

test('用户在本面板里手动编辑过监听路径：原样保留（返回 null = 不动）', () => {
  assert.equal(
    watchPathsAfterCwdChange({ touched: true, nextCwd: 'D:/work/b' }),
    null,
    '多根监听是明确意图，不能被一次目录改动清掉',
  );
});
