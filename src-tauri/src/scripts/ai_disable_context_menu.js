// Nexus AI 面板 · 屏蔽 dsh 页面自带的右键菜单（文档创建时注入，见 commands/ai.rs）
//
// 背景：面板里的 dsh 页面跑在**独立子 WebView** 里，不属于主界面文档——主界面的
// 「全局禁止右键菜单」（src/main.tsx）覆盖不到它。右键会弹出 WebView2 自带的
// 上下文菜单（刷新 / 另存为 / 检查…）：在停靠面板里没有用途且会误触
// （刷新让页面状态回退、检查元素把面板变成调试器）。
//
// 做法：contextmenu 事件 preventDefault —— Chromium 只在事件未被取消时才弹自带
// 菜单，与主界面同一套机制（src/main.tsx），不引入平台专有 API。
// 用捕获阶段：dsh 页面内部若对某元素 stopPropagation，冒泡阶段的监听收不到，
// 菜单仍会弹出；捕获阶段先于页面自身处理执行，屏蔽必达。
//
// 影响：面板内不再能用鼠标右键取「复制 / 粘贴」；Ctrl+C / Ctrl+V 不受影响
// （快捷键由 WebView2 直接处理，不经过本监听）。
//
// 约束：幂等、全程 try/catch 静默、不改写 dsh 页面自身逻辑。
(function () {
  if (window.__nexusContextMenuBlocked) return;
  window.__nexusContextMenuBlocked = true;

  try {
    document.addEventListener('contextmenu', function (e) {
      e.preventDefault();
    }, true);
  } catch (err) {
    /* 安装失败只是右键菜单照旧，不影响页面本身 */
  }
})();
