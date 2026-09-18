import { test } from 'node:test';
import assert from 'node:assert/strict';
import vm from 'node:vm';

import { toMessage } from '../message.ts';

/**
 * toMessage 是用户看到的所有错误文案的唯一起点（error.ts / clipboard.ts /
 * 各 store 的 catch 都汇到它）。这里钉的是"抛出物 → 可读字符串"这条规则。
 */

test('字符串原样返回：后端错误就是字符串，不能加前缀、不能变成 [object Object]', () => {
  assert.equal(toMessage('文件不存在'), '文件不存在');
});

test('Error 取 message，而不是 String(e)：后者会给文案多加一个 "Error: " 前缀', () => {
  assert.equal(toMessage(new Error('保存失败')), '保存失败');
});

test('Error 子类同样取 message（TypeError/RangeError 是常见抛出物）', () => {
  assert.equal(toMessage(new TypeError('x is not a function')), 'x is not a function');
  assert.equal(toMessage(new RangeError('out of range')), 'out of range');
});

test('message 为空的 Error 返回空串——调用方必须自己兜底，否则会弹出空白提示', () => {
  assert.equal(toMessage(new Error('')), '');
});

test('空字符串原样返回（空串是 falsy，`msg || fallback` 挡不住它）', () => {
  assert.equal(toMessage(''), '');
});

test('普通对象走 String()，落到 "[object Object]"：这是本函数的失明区，不会去读 e.message', () => {
  assert.equal(toMessage({ message: '看起来像 Error' }), '[object Object]');
});

test('null / undefined 不抛异常，返回字面量文本（catch(e) 里 e 可能是任何东西）', () => {
  assert.equal(toMessage(null), 'null');
  assert.equal(toMessage(undefined), 'undefined');
});

test('number / boolean / bigint 归一为字符串', () => {
  assert.equal(toMessage(404), '404');
  assert.equal(toMessage(0), '0');
  assert.equal(toMessage(false), 'false');
  assert.equal(toMessage(10n), '10');
});

test('Symbol 不会让 String() 抛异常（模板字符串拼接会抛，String() 不会）', () => {
  assert.equal(toMessage(Symbol('boom')), 'Symbol(boom)');
});

test('自定义 toString 生效（后端偶尔抛带 toString 的类实例）', () => {
  assert.equal(toMessage({ toString: () => '自定义文案' }), '自定义文案');
});

test('跨 realm 的 Error 过不了 instanceof，退化为 "Error: xxx"：文案没丢，但前缀会多出来', () => {
  // 子 WebView / iframe / vm 里抛出的 Error 与宿主不是同一个构造函数，
  // 这类错误正是"错误归一化"最容易被忽略的输入。
  const foreign = vm.runInNewContext('new Error("跨 realm")');
  assert.equal(foreign instanceof Error, false, '前提：跨 realm 的 Error 确实过不了 instanceof');
  assert.equal(toMessage(foreign), 'Error: 跨 realm');
});
