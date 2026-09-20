import { test } from 'node:test';
import assert from 'node:assert/strict';

import { failureLabel, failureDetail } from '../serviceFailure.ts';

/**
 * 这两个函数是"服务失败了"这句话的全部措辞来源（卡片徽章 / 日志面板头部 / 收起态 tooltip）。
 * 钉住的是两类真实误读：把"起不来"当成"跑起来又挂了"，以及"退出码 0"被当成成功。
 */

test('有退出码时原样写出：用户据此区分"起不来"与"跑起来又挂了"', () => {
  assert.equal(failureLabel({ service_id: 's', exit_code: 1, timestamp: '' }), '已失败 · 退出码 1');
});

test('退出码 0 也照写：0 表示进程自己退了（服务本来不该退），不是成功', () => {
  assert.equal(failureLabel({ service_id: 's', exit_code: 0, timestamp: '' }), '已失败 · 退出码 0');
});

test('spawn 失败（exit_code 为 null）不提退出码——进程根本没起来', () => {
  assert.equal(failureLabel({ service_id: 's', exit_code: null, timestamp: '' }), '启动失败');
});

test('悬停说明带本地时刻', () => {
  // 不带 Z 的 ISO 串按本地时间解析 → 断言与运行机器的时区无关（CI 在 UTC 也过）
  assert.equal(
    failureDetail({ service_id: 's', exit_code: 1, timestamp: '2026-09-20T18:22:31' }),
    '进程意外退出（退出码 1） · 18:22:31',
  );
});

test('时间戳缺失或不可解析时只少说时间，不显示 Invalid Date', () => {
  assert.equal(failureDetail({ service_id: 's', exit_code: null, timestamp: 'not-a-date' }), '启动失败：进程没有起来');
  assert.equal(
    failureDetail({ service_id: 's', exit_code: 3, timestamp: '' }),
    '进程意外退出（退出码 3）',
  );
});
