import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import './index.css';
import { logToBackend } from './utils/logSink';
import { isEditorContentTarget } from './utils/keyboard';

// 全局禁止右键菜单
document.addEventListener('contextmenu', (e) => e.preventDefault());

/**
 * 全局压住 Ctrl+F / Cmd+F 的**默认行为**（浏览器/WebView2 右上角自带的查找条）。
 *
 * 应用里有自己实现的查找：源码模式（CodeMirror 自定义面板）、Markdown 预览（查找条）、
 * 日志面板（聚焦查找框）。这些处理器都是在事件的冒泡阶段（或编辑器内部）工作的，
 * 因此这里必须**只 `preventDefault()`、不 `stopPropagation()`**——拦掉原生查找条的
 * 默认动作，同时让事件继续派发，应用自己的查找照常响应。
 *
 * 例外：事件目标在 CodeMirror 正文里时**不能**拦。CodeMirror 的 keymap 会跳过
 * `defaultPrevented` 的按键（见 `isEditorContentTarget` 的说明），先拦会让它的
 * `Mod-f` 彻底不执行——表现就是"光标在编辑器里时 Ctrl+F 没反应"。
 */
document.addEventListener('keydown', (e) => {
  if (!(e.ctrlKey || e.metaKey) || e.altKey || e.key.toLowerCase() !== 'f') return;
  if (isEditorContentTarget(e.target)) return;
  e.preventDefault();
}, true);

/**
 * 未捕获异常 / 未处理的 Promise rejection 也回传后端（`[frontend]` 落进 nexus.log）。
 *
 * 为什么必须有：此前只有显式调用 `reportError(...)` 的错误会进日志，而 React 在提交 /
 * 卸载阶段抛出的错只落到控制台——打包版既没有控制台也没有 devtools，这类故障
 * （例如"日志已经关了、DOM 却残留"）在现场就零线索。回传实现内部失败只 console.warn
 * 一次（见 services/system.ts），不会递归。
 */
function describeReason(reason: unknown): string {
  return reason instanceof Error ? (reason.stack ?? reason.message) : String(reason);
}
window.addEventListener('error', (e) => {
  logToBackend(
    'error',
    `未捕获异常: ${describeReason(e.error ?? e.message)}（${e.filename ?? '?'}:${e.lineno ?? '?'}:${e.colno ?? '?'}）`,
  );
});
window.addEventListener('unhandledrejection', (e) => {
  logToBackend('error', `未处理的 Promise rejection: ${describeReason(e.reason)}`);
});

const rootEl = document.getElementById('root');
if (!rootEl) throw new Error('找不到 #root 挂载点（index.html 被修改？）');

ReactDOM.createRoot(rootEl).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
