// Nexus AI 面板 · dsh 页面焦点记忆/恢复脚本（文档创建时注入，见 commands/ai.rs）
//
// 背景：dsh 页面以原生子 WebView 内嵌（第三方应用，无法改源码）。Alt+Tab 切走
// 再切回后输入框失去焦点。本脚本由 initialization_script 在每次文档创建时安装：
// - focusin 时记录最后聚焦的输入框及光标区间；
// - window.__nexusRestoreFocus()：恢复 DOM 焦点到输入框（窗口聚焦时由 Rust 侧
//   eval 主动调用，见 ai_focus_input 命令）；
// - window focus 监听：控件获得系统焦点（ai_panel_focus 命令 SetFocus）时自动恢复。
//
// 约束：幂等、全程 try/catch 静默、只监听不改写 dsh 页面逻辑。
(function () {
  if (window.__nexusFocusKeeperInstalled) return;
  window.__nexusFocusKeeperInstalled = true;

  var last = null; // 最后聚焦的输入元素
  var sel = null;  // 光标区间 [start, end]

  function isEditable(t) {
    return !!t && (t.tagName === 'TEXTAREA' || t.tagName === 'INPUT' || t.isContentEditable === true);
  }

  document.addEventListener('focusin', function (e) {
    var t = e.target;
    if (!isEditable(t)) return;
    last = t;
    sel = null;
    try {
      if (typeof t.selectionStart === 'number') {
        sel = [t.selectionStart, t.selectionEnd];
      }
    } catch (err) { /* 部分元素访问 selection 会抛错：忽略 */ }
  }, true);

  window.__nexusRestoreFocus = function () {
    try {
      if (!last || !document.contains(last)) {
        last = null;
        sel = null;
        return false;
      }
      // 幂等保护：焦点已在输入框上就不再碰它——反复执行 focus/setSelectionRange
      // 会重置光标并打断中文输入法（IME）的拼音候选状态
      if (document.activeElement === last) return true;
      last.focus();
      if (sel && typeof last.setSelectionRange === 'function') {
        last.setSelectionRange(sel[0], sel[1]);
      } else if (last.tagName === 'TEXTAREA' || last.tagName === 'INPUT') {
        var n = last.value ? String(last.value).length : 0;
        last.setSelectionRange(n, n);
      }
      return true;
    } catch (err) {
      return false;
    }
  };

  // 控件获得系统键盘焦点 → 页面 window focus 事件 → 自动恢复（先等一帧避竞争）
  window.addEventListener('focus', function () {
    setTimeout(function () { window.__nexusRestoreFocus(); }, 0);
  });
})();
