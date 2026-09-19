import { getCurrentWindow } from '@tauri-apps/api/window';
import { invoke } from '@tauri-apps/api/core';
import { open as openUrl } from '@tauri-apps/plugin-shell';
import { reportError } from '../../utils/error';
import { notify } from '../../utils/notify';
import { compareVersions } from '../../utils/version';
import { useCallback, useEffect, useState, useRef } from 'react';
import { useClickOutside } from '../../hooks/useClickOutside';

/** 仓库地址：关于与检查更新都从这里取（换仓库时只改这一处） */
const REPO_URL = 'https://github.com/hixingchen/nexus';
const LATEST_RELEASE_API = 'https://api.github.com/repos/hixingchen/nexus/releases/latest';

interface TitleBarProps {
  projectName?: string | null;
}

export function TitleBar({ projectName }: TitleBarProps) {
  const appWindow = getCurrentWindow();
  const [isMaximized, setIsMaximized] = useState(false);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    // 最大化状态查询失败不影响使用（保持默认 false），但必须消费掉 rejection——
    // 裸 .then 会抛未处理的 rejection，且窗口尺寸变化后状态会一直不同步
    const syncMaximized = () => {
      appWindow.isMaximized().then(v => { if (!disposed) setIsMaximized(v); })
        // 只留控制台：影响面是标题栏图标的最大化/还原外观，弹 toast 属噪音
        .catch(e => console.error('查询窗口最大化状态失败:', e));
    };
    syncMaximized();
    appWindow.onResized(syncMaximized)
      .then(fn => { if (disposed) { fn(); return; } unlisten = fn; })
      // 同上：订阅失败只是图标不再自动同步，用户仍能点按钮切换
      .catch(e => console.error('订阅窗口尺寸变化失败:', e));
    return () => { disposed = true; unlisten?.(); };
  }, [appWindow]);

  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    // 收窄而不是断言（CQ-21）：mousedown 的 target 可能是**文本节点**（点中的是标题栏
    // 里的文字），此时 `.closest` 为 undefined —— `as HTMLElement` 会让它一路跑到
    // "closest is not a function" 的 TypeError，把标题栏拖拽整个打断。
    // 非元素目标一律按"没点在按钮/菜单上"处理（与点空白处一致）。
    const target = e.target;
    if (target instanceof Element && target.closest('button, [data-menu]')) return;
    appWindow.startDragging().catch(() => { /* 非 mousedown 上下文等场景下拖动被拒绝，忽略 */ });
  }, [appWindow]);

  return (
    <div className="h-[32px] bg-nexus-titlebar flex items-center select-none flex-shrink-0" onMouseDown={handleMouseDown}>
      {/* ── 应用菜单 ── */}
      <AppMenu />

      {/* ── 项目名 ── */}
      <div className="flex-1 flex items-center justify-center h-full">
        <span className="text-[12px] text-nexus-text-muted">
          {projectName || ''}
        </span>
      </div>

      {/* ── 窗口控制 ── */}
      <div className="flex h-full">
        <button className="w-[46px] h-full flex items-center justify-center text-nexus-muted hover:bg-nexus-hover hover:text-nexus-text" aria-label="最小化" title="最小化" onClick={() => appWindow.minimize()}>
          <svg width="10" height="1" viewBox="0 0 10 1" fill="currentColor"><rect width="10" height="1"/></svg>
        </button>
        <button className="w-[46px] h-full flex items-center justify-center text-nexus-muted hover:bg-nexus-hover hover:text-nexus-text" onClick={() => appWindow.toggleMaximize()} title={isMaximized ? "还原" : "最大化"}>
          {isMaximized ? (
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1"><rect x="2" y="0" width="7" height="7"/><polyline points="0,3 0,10 7,10"/></svg>
          ) : (
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1"><rect x="0.5" y="0.5" width="9" height="9"/></svg>
          )}
        </button>
        <button className="w-[46px] h-full flex items-center justify-center text-nexus-muted hover:bg-nexus-error hover:text-white" aria-label="关闭窗口" title="关闭窗口" onClick={() => appWindow.close()}>
          <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2"><line x1="1" y1="1" x2="9" y2="9"/><line x1="9" y1="1" x2="1" y2="9"/></svg>
        </button>
      </div>
    </div>
  );
}

// ─── 应用菜单 ──────────────────────────────────────────

/**
 * 左上角的应用菜单。
 *
 * 从「帮助」改名而来：那里此前只有一项「关于 Nexus」，没有任何帮助内容，名字与内容
 * 对不上；加上「检查更新」之后装的是清一色的**应用级信息**，用应用名更准。
 * 以后要放「使用文档」「反馈问题」这类真正的帮助入口，再加回来也不冲突。
 */
function AppMenu() {
  const [open, setOpen] = useState(false);
  /** 检查更新进行中（菜单项显示"检查中…"并禁用，避免连点发出一串请求） */
  const [checking, setChecking] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  // 点击菜单外部关闭（菜单内的点击由各自按钮处理）
  useClickOutside(ref, () => setOpen(false));

  /**
   * 检查更新：拉 GitHub 的最新 release，与本机版本比。
   *
   * 版本号问后端要（`CARGO_PKG_VERSION`）而不是读 `package.json`——后者是 npm 侧的，
   * 与这个二进制未必同步；比的是"用户装的这一份"有没有新版，就得用它自己的版本。
   *
   * 三种结果都要说清楚，尤其"比不出来"：把它和"已是最新"混为一谈的话，界面会把一个
   * 解析失败说成结论。
   */
  const handleCheckUpdate = async (e: React.MouseEvent) => {
    e.stopPropagation();
    setOpen(false);
    setChecking(true);
    try {
      const current = await invoke<string>('get_app_version');
      const res = await fetch(LATEST_RELEASE_API, { headers: { Accept: 'application/vnd.github+json' } });
      if (!res.ok) throw new Error(`GitHub 返回 ${res.status}`);
      const data = (await res.json()) as { tag_name?: string; html_url?: string };
      const latest = (data.tag_name ?? '').trim();
      const cmp = compareVersions(latest, current);

      if (cmp === null) {
        notify({
          variant: 'warning',
          title: '无法比较版本号',
          description: `本机 ${current}，线上 ${latest || '(没拿到 tag)'}`,
          duration: 6000,
        });
      } else if (cmp > 0) {
        notify({
          variant: 'info',
          title: `有新版本 ${latest}`,
          description: `当前 ${current}`,
          duration: 10000,
          // 提示里直接给出口：让用户读完再去别处找下载页，等于把"应用知道怎么办"
          // 降级成"用户自己去想"
          action: {
            label: '打开下载页',
            run: () => { void openUrl(data.html_url || `${REPO_URL}/releases`).catch((err) => reportError('打开网页失败', err)); },
          },
        });
      } else {
        notify({ title: '已是最新版本', description: `当前 ${current}`, duration: 2500 });
      }
    } catch (err) {
      // 网络不通 / GitHub 限流 / CSP 没放开——原因都在 err 里，别只说"失败"
      reportError('检查更新失败', err);
    } finally {
      setChecking(false);
    }
  };

  return (
    <div className="relative h-full" ref={ref}>
      <button
        className={`h-full px-2.5 text-[12.5px] transition-colors ${
          open ? 'bg-nexus-hover text-nexus-text' : 'text-nexus-text-muted hover:text-nexus-text hover:bg-nexus-hover/40'
        }`}
        onClick={(e) => { e.stopPropagation(); setOpen(!open); }}
      >
        Nexus
      </button>
      {open && (
        <div className="absolute top-full left-0 mt-0.5 min-w-[160px] bg-nexus-surface border border-nexus-border rounded-md shadow-xl py-1 z-50">
          <button
            className="w-full px-3 py-1.5 text-[12px] text-nexus-text hover:bg-nexus-accent hover:text-white text-left transition-colors"
            onClick={async (e) => {
              e.stopPropagation();
              setOpen(false);
              try {
                await openUrl(REPO_URL);
              } catch (err) {
                reportError('打开网页失败', err);
              }
            }}
          >
            关于 Nexus
          </button>
          <button
            className="w-full px-3 py-1.5 text-[12px] text-nexus-text hover:bg-nexus-accent hover:text-white text-left transition-colors disabled:opacity-50 disabled:hover:bg-transparent disabled:hover:text-nexus-text"
            disabled={checking}
            onClick={handleCheckUpdate}
          >
            {checking ? '检查中…' : '检查更新'}
          </button>
        </div>
      )}
    </div>
  );
}
