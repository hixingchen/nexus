import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  EDITOR_FONT_SIZE_DEFAULT,
  EDITOR_FONT_SIZE_MAX,
  EDITOR_FONT_SIZE_MIN,
  clampEditorFontSize,
  parseEditorFontSize,
  stepEditorFontSize,
} from '../editorFont.ts';

/**
 * 编辑器字号的解析。
 *
 * 为什么值得逐条钉：解析结果直接进 CodeMirror 的主题（`fontSize: "<n>px"`），
 * 而 `NaN` / `0` 这类值不会报错——浏览器只是丢弃那条声明（或把字缩成看不见），
 * 症状是"编辑器看着不对劲"而没有任何线索。这里把每条输入钉到确定输出。
 */

test('正常值原样通过', () => {
  assert.equal(parseEditorFontSize('12'), 12);
  assert.equal(parseEditorFontSize('14'), 14);
  assert.equal(parseEditorFontSize('18'), 18);
  assert.equal(parseEditorFontSize('20'), 20);
});

test('没设置过（null / undefined / 空串 / 纯空白）一律回落默认值，而不是被夹成下限', () => {
  for (const raw of [null, undefined, '', '   ']) {
    assert.equal(
      parseEditorFontSize(raw),
      EDITOR_FONT_SIZE_DEFAULT,
      `\`${String(raw)}\` 应回落默认值`,
    );
  }
  // 这条是回归钉子：`Number('')` 是 0（不是 NaN），少了空值判断就会得到 MIN
  assert.notEqual(parseEditorFontSize(''), EDITOR_FONT_SIZE_MIN);
});

test('非数字回落默认值（不是 NaN、也不是 0）', () => {
  for (const raw of ['abc', '14px', '十四', '--', 'Infinity', 'NaN']) {
    const got = parseEditorFontSize(raw);
    assert.equal(got, EDITOR_FONT_SIZE_DEFAULT, `\`${raw}\` 应回落默认值`);
    assert.ok(Number.isFinite(got), `\`${raw}\` 不该产出非有限值`);
  }
});

test('越界值夹到区间内（0 / 负数 / 超大值都不会漏出去）', () => {
  assert.equal(parseEditorFontSize('0'), EDITOR_FONT_SIZE_MIN);
  assert.equal(parseEditorFontSize('-5'), EDITOR_FONT_SIZE_MIN);
  assert.equal(parseEditorFontSize('11'), EDITOR_FONT_SIZE_MIN);
  assert.equal(parseEditorFontSize('21'), EDITOR_FONT_SIZE_MAX);
  assert.equal(parseEditorFontSize('1e9'), EDITOR_FONT_SIZE_MAX);
});

test('小数四舍五入到整数像素（半个像素的字号会让行高出现毛边）', () => {
  assert.equal(parseEditorFontSize('13.4'), 13);
  assert.equal(parseEditorFontSize('13.5'), 14);
  assert.equal(parseEditorFontSize('15.9'), 16);
});

test('区间常量本身自洽（下限 ≤ 默认值 ≤ 上限）', () => {
  assert.ok(EDITOR_FONT_SIZE_MIN <= EDITOR_FONT_SIZE_DEFAULT);
  assert.ok(EDITOR_FONT_SIZE_DEFAULT <= EDITOR_FONT_SIZE_MAX);
  assert.ok(EDITOR_FONT_SIZE_MIN < EDITOR_FONT_SIZE_MAX);
});

test('clampEditorFontSize：越界与小数统一收口（设置按钮 / Ctrl+滚轮共用）', () => {
  assert.equal(clampEditorFontSize(0), EDITOR_FONT_SIZE_MIN);
  assert.equal(clampEditorFontSize(999), EDITOR_FONT_SIZE_MAX);
  assert.equal(clampEditorFontSize(13.4), 13);
  assert.equal(clampEditorFontSize(13.5), 14);
});

test('stepEditorFontSize：向上滚 +1、向下滚 −1，两端不越界', () => {
  assert.equal(stepEditorFontSize(14, -100), 15);
  assert.equal(stepEditorFontSize(14, 100), 13);
  assert.equal(stepEditorFontSize(14, -1), 15);
  assert.equal(stepEditorFontSize(14, 0), 14, '横向滚轮/零增量不该改字号');
  assert.equal(stepEditorFontSize(EDITOR_FONT_SIZE_MAX, -100), EDITOR_FONT_SIZE_MAX);
  assert.equal(stepEditorFontSize(EDITOR_FONT_SIZE_MIN, 100), EDITOR_FONT_SIZE_MIN);
});
