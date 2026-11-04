import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';

/**
 * pdf.js 版本兼容性守卫。
 *
 * 实测（Edge/WebView2 142）：pdfjs-dist 5.x / 6.x 的构建产物使用
 * `Map.prototype.getOrInsertComputed`，而该引擎还没有这个方法——打开任意 PDF
 * 都会在渲染阶段报 `getOrInsertComputed is not a function`。类型检查与构建都不会
 * 发现它，只有真跑到 WebView2 里才会暴露，所以在这里钉住。
 *
 * 若将来 WebView2 支持了该 API、想升级 pdf.js：先在本机（或 CI 的 WebView2）打开
 * 一个真实 PDF 验证，再删掉这条断言。
 */
test('pdfjs-dist 构建产物不得使用 WebView2 142 尚不支持的 getOrInsertComputed', () => {
  for (const file of ['pdf.min.mjs', 'pdf.worker.min.mjs']) {
    const source = readFileSync(path.resolve('node_modules/pdfjs-dist/build', file), 'utf8');
    assert.ok(
      !source.includes('getOrInsertComputed'),
      `${file} 使用了 Map.prototype.getOrInsertComputed：当前 WebView2 打开 PDF 会直接失败。`
      + '请回退到 pdfjs-dist 4.x，或先确认目标 WebView2 版本已支持该 API。',
    );
  }
});
