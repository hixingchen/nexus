import { test } from 'node:test';
import assert from 'node:assert/strict';

import { base64ToBytes } from '../base64.ts';

test('base64 → 字节：内容与长度都按原字节还原', () => {
  // 'Nexus' = 4E 65 78 75 73；PDF 文件头则是 %PDF-
  assert.deepEqual([...base64ToBytes('TmV4dXM=')], [0x4e, 0x65, 0x78, 0x75, 0x73]);
  assert.deepEqual([...base64ToBytes('JVBERi0=')], [0x25, 0x50, 0x44, 0x46, 0x2d]);
});

test('base64 → 字节：空串是空数组，高位字节不被符号扩展', () => {
  assert.equal(base64ToBytes('').length, 0);
  // 0xFF / 0x80：charCodeAt 是 0..255，写进 Uint8Array 不会变负数
  assert.deepEqual([...base64ToBytes('/4A=')], [0xff, 0x80]);
});
