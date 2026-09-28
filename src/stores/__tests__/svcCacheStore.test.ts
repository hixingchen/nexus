import { test } from 'node:test';
import assert from 'node:assert/strict';

import { useSvcCacheStore } from '../svcCacheStore.ts';
import type { Service } from '../../services/service.ts';

/**
 * 项目服务缓存的「引用计数 + 待删标记」语义。
 *
 * 守的失败模式只有一条：**把界面正在显示的那份缓存删掉**。左列表展开着的项目行与
 * 详情页都从这份缓存取服务列表，删掉的表现是"展开着的项目卡片显示『暂无服务目录树』"
 * （UX-19，实测复现：折叠过再展开、然后切走别的项目就触发）。
 *
 * 另一半同样要守：**该释放的还是要释放**——折叠一个没人持有的项目时立即删、被详情页
 * 持有的推迟到释放之后。为了修 bug 把内存优化改没了，不算修好。
 */

function svc(id: string): Service {
  return {
    id, project_id: 'p', name: id, command: 'run', cwd: 'C:/x',
    watch_paths: '', watch_include: '*', watch_exclude: '', env_vars: '',
    restart_mode: 0, enabled: true, show_file_tree: true, sort_index: 0, tool_commands: '[]',
  };
}

/** 每个用例从空 store 开始（模块级单例：不清会互相串味） */
function reset(): void {
  useSvcCacheStore.setState({ cache: {}, refs: {}, pendingEvict: {} });
}

const store = () => useSvcCacheStore.getState();
const has = (id: string) => Object.prototype.hasOwnProperty.call(store().cache, id);

test('折叠一个没人持有的项目：立即删除', () => {
  reset();
  store().setCache('p-a', [svc('s1')]);
  store().invalidate('p-a');
  assert.equal(has('p-a'), false, '无人持有 → 折叠即释放');
});

test('折叠一个被详情页持有的项目：推迟到释放之后', () => {
  reset();
  store().setCache('p-a', [svc('s1')]);
  store().retain('p-a');
  store().invalidate('p-a');
  assert.equal(has('p-a'), true, '详情页还在看它，不能当场删（服务列会整片消失）');
  store().release('p-a');
  assert.equal(has('p-a'), false, '释放后按请求删除');
});

test('折叠后**重新写入**（重新展开 / 详情重新加载）：释放时不许删（UX-19 回归）', () => {
  reset();
  store().retain('p-a');
  store().setCache('p-a', [svc('s1')]);
  store().invalidate('p-a');            // 折叠 → 记为待删
  store().setCache('p-a', [svc('s1')]); // 重新展开：数据又回来了，旧请求已过期
  store().release('p-a');               // 切到别的项目
  assert.equal(has('p-a'), true, '展开着的项目行还要用它，删掉就显示「暂无服务目录树」');
});

test('重新写入后再折叠：待删标记重新生效', () => {
  reset();
  store().retain('p-a');
  store().setCache('p-a', [svc('s1')]);
  store().invalidate('p-a');
  store().setCache('p-a', [svc('s1')]);
  store().invalidate('p-a');            // 又折叠了一次
  store().release('p-a');
  assert.equal(has('p-a'), false, '最后一次折叠的请求仍然有效');
});

test('没被折叠过就切走：缓存保留（下次进详情还能当首帧数据）', () => {
  reset();
  store().retain('p-a');
  store().setCache('p-a', [svc('s1')]);
  store().release('p-a');
  assert.equal(has('p-a'), true);
});
