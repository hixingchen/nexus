import { test } from 'node:test';
import assert from 'node:assert/strict';

import { languageKeyOf } from '../editorLanguage.ts';

/**
 * 语言键解析：文件名特例优先，扩展名其次。
 *
 * 守的是一类**静默失效**：键查不到语言包时编辑器只是"没有高亮"，不报错、不提示，
 * 很容易在改表时把某个常用文件名漏掉而无人发现。
 */

test('扩展名照常小写归一', () => {
  assert.equal(languageKeyOf('D:/work/proj/src/app.TSX'), 'tsx');
  assert.equal(languageKeyOf('/home/u/a.Service'), 'service');
  assert.equal(languageKeyOf('README.md'), 'md');
});

test('systemd 单元按扩展名命中（此前完全没有高亮）', () => {
  assert.equal(languageKeyOf('/etc/systemd/system/nexus.service'), 'service');
  assert.equal(languageKeyOf('/etc/systemd/system/app.socket'), 'socket');
  assert.equal(languageKeyOf('/etc/systemd/system/app.timer'), 'timer');
});

test('Dockerfile / Makefile 的带后缀变体仍归到本体', () => {
  assert.equal(languageKeyOf('Dockerfile'), 'dockerfile');
  assert.equal(languageKeyOf('Dockerfile.dev'), 'dockerfile');
  assert.equal(languageKeyOf('dockerfile.prod'), 'dockerfile');
  assert.equal(languageKeyOf('app.dockerfile'), 'dockerfile');
  assert.equal(languageKeyOf('Makefile'), 'makefile');
  assert.equal(languageKeyOf('Makefile.in'), 'makefile');
  assert.equal(languageKeyOf('makefile.am'), 'makefile');
});

test('固定文件名 + 通用扩展名（CMakeLists.txt / nginx.conf）', () => {
  assert.equal(languageKeyOf('D:/proj/CMakeLists.txt'), 'cmake');
  assert.equal(languageKeyOf('/etc/nginx/nginx.conf'), 'nginx');
  // 其他 .conf 仍是通用配置，不该被 nginx 抢走
  assert.equal(languageKeyOf('/etc/app/app.conf'), 'conf');
});

test('dotenv 的多环境变体（.env.local 等）', () => {
  assert.equal(languageKeyOf('.env'), 'env');
  assert.equal(languageKeyOf('D:/proj/.env.local'), 'env');
  assert.equal(languageKeyOf('D:/proj/.env.production'), 'env');
  assert.equal(languageKeyOf('D:/proj/.env.example'), 'env');
  // 只是名字里带 env 的普通文件不受影响
  assert.equal(languageKeyOf('environment.ts'), 'ts');
});

test('两种路径分隔符都认，且没有分隔符时也能取到文件名', () => {
  assert.equal(languageKeyOf('D:\\work\\proj\\CMakeLists.txt'), 'cmake');
  assert.equal(languageKeyOf('Dockerfile.dev'), 'dockerfile');
});
