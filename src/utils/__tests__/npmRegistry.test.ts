import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  DEFAULT_NPM_REGISTRY,
  OFFICIAL_NPM_REGISTRY,
  normalizeNpmRegistry,
} from '../npmRegistry.ts';

/**
 * npm 源的校验与归一化。
 *
 * 这条规则前后端各有一份（Rust 侧是安装前的最终门禁），所以边界值要钉死：
 * 界面接受、后端拒绝（或反过来）会变成"保存成功但安装报错"的割裂体验。
 */

test('没配置 / 空串 / 纯空白 → 默认淘宝源', () => {
  for (const raw of [null, undefined, '', '   ']) {
    const got = normalizeNpmRegistry(raw);
    assert.equal(got.ok, true);
    assert.equal(got.ok && got.value, DEFAULT_NPM_REGISTRY);
  }
});

test('常见源原样通过，末尾斜杠归一化掉', () => {
  assert.deepEqual(normalizeNpmRegistry(' https://registry.npmmirror.com/ '), {
    ok: true,
    value: 'https://registry.npmmirror.com',
  });
  assert.deepEqual(normalizeNpmRegistry('https://registry.npmjs.org'), {
    ok: true,
    value: 'https://registry.npmjs.org',
  });
  assert.deepEqual(normalizeNpmRegistry('http://host:8080/repository/npm/'), {
    ok: true,
    value: 'http://host:8080/repository/npm',
  });
  // scheme 大小写不影响判断
  assert.deepEqual(normalizeNpmRegistry('HTTPS://HOST/'), {
    ok: true,
    value: 'HTTPS://HOST',
  });
});

test('非法协议 / 缺主机 / 带凭据 / 空白 → 拒绝', () => {
  for (const raw of [
    'ftp://host',
    'registry.npmmirror.com',
    'https://',
    'https:///repo',
    'https://user:pw@host',
    'https://host/a b',
  ]) {
    const got = normalizeNpmRegistry(raw);
    assert.equal(got.ok, false, `\`${raw}\` 不该被接受`);
  }
});

test('默认值与常用预设本身合法', () => {
  assert.ok(DEFAULT_NPM_REGISTRY.startsWith('https://'));
  assert.ok(DEFAULT_NPM_REGISTRY.includes('npmmirror'));
  assert.ok(OFFICIAL_NPM_REGISTRY.includes('registry.npmjs.org'));
  assert.equal(normalizeNpmRegistry(DEFAULT_NPM_REGISTRY).ok, true);
  assert.equal(normalizeNpmRegistry(OFFICIAL_NPM_REGISTRY).ok, true);
});
