import { useEffect, useRef, useState } from 'react';
import { Webview } from '@tauri-apps/api/webview';
import { getCurrentWindow } from '@tauri-apps/api/window';
import type { UnlistenFn } from '@tauri-apps/api/event';
import { LogicalPosition, LogicalSize } from '@tauri-apps/api/dpi';
import { useHarnessStore } from '../../stores/harnessStore';

interface HarnessEmbedProps {
  /** 当前项目的工作目录（dsh web workspace 根）；可为空 */
  cwd: string | null;
}

/** 内嵌 WebView 固定标签 */
const WV_LABEL = 'harness-ai';
/** Dock 宽度下限（逻辑像素；实际宽度存 harnessStore，默认 460 可拖拽/持久化） */
const PANEL_MIN_W = 340;
/** 左缘细分隔条宽度（DOM，位于原生 WebView 之外：整高可见 + 可直接拖拽调宽） */
const SPLIT_W = 6;
/** Dock 头部高度（DOM，逻辑像素） */
const HEADER_H = 36;

/**
 * DeepSeek Harness Web GUI 停靠栏：以 flex 布局的一列存在（不悬浮、不遮挡内容），
 * 内容区（编辑器/文件等）打开时自动让出宽度。WebView 是原生控件，
 * 停靠栏顶部保留 DOM 头部（关闭/刷新），原生 WebView 精确铺在其下区域。
 */
export function HarnessEmbed({ cwd }: HarnessEmbedProps) {
  const panelOpen = useHarnessStore((s) => s.panelOpen);
  const panelWidth = useHarnessStore((s) => s.panelWidth);
  const running = useHarnessStore((s) => s.running);
  const url = useHarnessStore((s) => s.url);
  const starting = useHarnessStore((s) => s.starting);
  const dshFound = useHarnessStore((s) => s.dshFound);
  const installing = useHarnessStore((s) => s.installing);
  const lastError = useHarnessStore((s) => s.lastError);

  const wvRef = useRef<Webview | null>(null);
  const createLockRef = useRef<Promise<void> | null>(null);
  const unlistenResizeRef = useRef<UnlistenFn | null>(null);
  const dragStartRef = useRef<{ x: number; w: number } | null>(null);
  /** 内容区 DOM 槽：原生 WebView 以它的真实矩形为准（保证与上方头部等宽对齐） */
  const bodySlotRef = useRef<HTMLDivElement | null>(null);
  const [wvReady, setWvReady] = useState(false);
  const [localError, setLocalError] = useState<string | null>(null);
  /** 手动重载/重试后强制重建 WebView（依赖不变时 effect 不会重跑） */
  const [nonce, setNonce] = useState(0);

  // store 生命周期 + 卸载清理
  useEffect(() => {
    void useHarnessStore.getState().init();
    return () => {
      void useHarnessStore.getState().dispose();
      destroyRefs();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 打开时确保 runtime 就绪
  useEffect(() => {
    if (!panelOpen) return;
    const st = useHarnessStore.getState();
    if (!st.running && !st.starting && st.dshFound !== false) {
      void useHarnessStore.getState().ensureRunning(cwd);
    }
  }, [panelOpen, cwd]);

  // running + url 就绪后创建子 WebView（以内容区 DOM 槽矩形定位，保证与头部等宽对齐）
  useEffect(() => {
    if (!panelOpen || !running || !url) return;
    if (wvRef.current || createLockRef.current) return;
    const slot = bodySlotRef.current;
    if (!slot) {
      // 槽位尚未就绪（极少见），等一帧再试
      const t = setTimeout(() => setNonce((n) => n + 1), 60);
      return () => clearTimeout(t);
    }
    createLockRef.current = (async () => {
      try {
        const { wv, unlisten } = await createEmbeddedWebview(url, slot);
        if (!useHarnessStore.getState().panelOpen) {
          await wv.close();
          unlisten();
          return;
        }
        wvRef.current = wv;
        unlistenResizeRef.current = unlisten;
        setWvReady(true);
      } catch (e) {
        setLocalError(e instanceof Error ? e.message : String(e));
      } finally {
        createLockRef.current = null;
      }
    })();
  }, [panelOpen, running, url, nonce]);

  // 关闭 → 销毁子 WebView
  useEffect(() => {
    if (!panelOpen) {
      destroyRefs();
    }
  }, [panelOpen]);

  // 宽度/布局变化后：WebView 重贴内容区槽（始终与上方头部同宽对齐）
  useEffect(() => {
    if (panelOpen && wvRef.current && bodySlotRef.current) {
      void relayoutWebview(wvRef.current, bodySlotRef.current);
    }
  }, [panelOpen, panelWidth]);

  // 左缘拖拽调宽
  const onResizeStart = (e: React.MouseEvent) => {
    e.preventDefault();
    dragStartRef.current = { x: e.clientX, w: useHarnessStore.getState().panelWidth };
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';
    const onMove = (ev: MouseEvent) => {
      const d = dragStartRef.current;
      if (!d) return;
      // 给主内容留出最小宽度
      const maxW = Math.max(PANEL_MIN_W, Math.floor(window.innerWidth - 420));
      const w = Math.min(Math.max(d.w + (d.x - ev.clientX), PANEL_MIN_W), maxW);
      useHarnessStore.getState().setPanelWidth(w);
    };
    const onUp = () => {
      dragStartRef.current = null;
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    };
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
  };

  const dshMissing = dshFound === false;
  const showOverlay = !dshMissing && panelOpen && !wvReady && (starting || !running || !!lastError || !!localError || !url);

  const recheckAndStart = async () => {
    setLocalError(null);
    useHarnessStore.getState().clearError();
    await useHarnessStore.getState().refreshStatus();
    const st = useHarnessStore.getState();
    if (st.dshFound) void st.ensureRunning(cwd);
  };

  // 未打开时不占布局
  if (!panelOpen) return null;

  return (
    <div className="relative h-full flex flex-shrink-0 bg-nexus-surface" style={{ width: panelWidth }}>
      {/* ── 左缘细分隔条：整高可见（不在 WebView 之下），按住可拖拽调宽 ── */}
      <div
        className="relative h-full flex-shrink-0 cursor-col-resize select-none group flex flex-col items-center"
        style={{ width: SPLIT_W }}
        title="拖拽调整宽度"
        onMouseDown={onResizeStart}
      >
        <div className="absolute inset-y-0 left-1/2 -translate-x-1/2 w-px bg-nexus-border group-hover:bg-nexus-accent/70 transition-colors" />
      </div>

      {/* ── Dock 内容列（头部为 DOM；其下区域承载原生 WebView，铺满 Dock） ── */}
      <div className="relative flex-1 min-w-0 flex flex-col">
      {/* ── Dock 头部（DOM，可点击） ── */}
      <div className="flex items-center gap-1.5 px-2 border-b border-nexus-border flex-shrink-0 select-none" style={{ height: HEADER_H }}>
        <span className="w-6 h-6 flex items-center justify-center text-nexus-accent">
          <RobotIcon />
        </span>
        <span className="flex-1 truncate text-[11.5px] text-nexus-text-muted" title="DeepSeek Harness">
          DeepSeek Harness
        </span>
        <button
          className="w-6 h-6 flex items-center justify-center rounded text-nexus-muted hover:text-nexus-text hover:bg-nexus-hover/60"
          title="重新加载界面"
          onClick={() => {
            destroyRefs();
            setLocalError(null);
            useHarnessStore.getState().clearError();
            setNonce((n) => n + 1);
            void useHarnessStore.getState().ensureRunning(cwd);
          }}
        >
          <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round">
            <path d="M13.5 8a5.5 5.5 0 1 1-1.6-3.9" />
            <polyline points="14.2 1.8 14.2 5.2 10.8 5.2" />
          </svg>
        </button>
        <button
          className="w-6 h-6 flex items-center justify-center rounded text-nexus-muted hover:text-nexus-text hover:bg-nexus-hover/60"
          title="关闭 AI 面板"
          onClick={() => useHarnessStore.getState().setPanelOpen(false)}
        >
          <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.2">
            <line x1="1.5" y1="1.5" x2="10.5" y2="10.5" /><line x1="10.5" y1="1.5" x2="1.5" y2="10.5" />
          </svg>
        </button>
      </div>

      {/* ── 内容区：原生 WebView 覆盖此区域；启动前显示 DOM 覆盖层 ── */}
      <div ref={bodySlotRef} className="relative flex-1 overflow-hidden">
        {/* 未安装 dsh：引导一键安装 */}
        {dshMissing && (
          <div className="absolute inset-0 flex flex-col items-center justify-center gap-3 px-8 text-center">
            <span className="text-nexus-accent opacity-90">
              <RobotIcon size={38} />
            </span>
            <p className="text-[13px] text-nexus-text font-medium select-none">未检测到 dsh</p>
            <p className="text-[11.5px] text-nexus-text-muted leading-relaxed select-none">
              AI 面板依赖 DeepSeek Harness 运行环境（dsh）。
              <br />
              点击下方按钮将自动安装：
            </p>
            <code className="text-[11px] text-nexus-info bg-nexus-editor border border-nexus-border rounded px-2 py-1 select-all font-mono">
              npm i -g @deepseek-ai/dsh
            </code>
            <div className="flex items-center gap-2 mt-1">
              <button
                className="px-4 h-8 rounded bg-nexus-accent text-white text-[12px] hover:bg-nexus-accent-hover disabled:opacity-50 inline-flex items-center gap-2"
                disabled={installing}
                onClick={() => void useHarnessStore.getState().installDsh(cwd)}
              >
                {installing && <span className="w-3 h-3 border-2 border-white/70 border-t-transparent rounded-full animate-spin" />}
                {installing ? '安装中…' : '一键安装 dsh'}
              </button>
              <button
                className="px-3 h-8 rounded bg-nexus-hover text-nexus-text text-[12px] hover:bg-nexus-hover/70 disabled:opacity-50"
                disabled={installing}
                onClick={() => void recheckAndStart()}
              >
                重新检测
              </button>
            </div>
            <p className="text-[10.5px] text-nexus-muted select-none max-w-[300px] leading-relaxed">
              需本机已安装 Node.js / npm。若你已手动安装 dsh，点「重新检测」即可继续。
            </p>
          </div>
        )}

        {/* 启动中 / 失败（已装 dsh 的正常路径） */}
        {showOverlay && (
          <div className="absolute inset-0 flex flex-col items-center justify-center gap-3">
            {(starting || (!running && !lastError && !localError)) ? (
              <>
                <span className="w-4 h-4 border-2 border-nexus-accent border-t-transparent rounded-full animate-spin" />
                <p className="text-[11.5px] text-nexus-text-muted select-none">
                  {dshFound === null ? '正在检测环境…' : '正在启动 DeepSeek Harness…'}
                </p>
              </>
            ) : (
              <>
                <p className="text-[11.5px] text-nexus-error select-none px-4 text-center whitespace-pre-wrap">⚠ {lastError || localError || '启动失败'}</p>
                <div className="flex items-center gap-2">
                  <button
                    className="px-3 h-7 rounded bg-nexus-accent text-white text-[11.5px] hover:bg-nexus-accent-hover"
                    onClick={() => {
                      setLocalError(null);
                      useHarnessStore.getState().clearError();
                      setNonce((n) => n + 1);
                      void useHarnessStore.getState().ensureRunning(cwd);
                    }}
                  >
                    重试
                  </button>
                  <button
                    className="px-3 h-7 rounded bg-nexus-hover text-nexus-text text-[11.5px] hover:bg-nexus-hover/70"
                    onClick={() => void recheckAndStart()}
                  >
                    重新检测
                  </button>
                </div>
              </>
            )}
          </div>
        )}

        {/* 兜底：环境正常、即将创建 WebView 的极短间隙 */}
        {!dshMissing && !showOverlay && !wvReady && (
          <div className="absolute inset-0 flex items-center justify-center">
            <span className="text-[11px] text-nexus-muted select-none">加载中…</span>
          </div>
        )}
      </div>
      </div>
    </div>
  );

  function destroyRefs() {
    const wv = wvRef.current;
    wvRef.current = null;
    if (wv) void wv.close().catch(() => {});
    const un = unlistenResizeRef.current;
    unlistenResizeRef.current = null;
    if (un) un();
    setWvReady(false);
  }
}

/** 创建并定位子 WebView（以内容区 DOM 槽的真实矩形为准，杜绝与头部错位）；返回实例与 resize 监听 */
async function createEmbeddedWebview(url: string, slot: HTMLElement) {
  const appWindow = getCurrentWindow();
  const b = boundsFromSlot(slot);
  const wv = new Webview(appWindow, WV_LABEL, {
    url,
    x: b.x,
    y: b.y,
    width: b.width,
    height: b.height,
    dragDropEnabled: false,
  });
  await new Promise<void>((resolve, reject) => {
    const onCreated = () => resolve();
    const onError = (e: unknown) => {
      const p = (e as { payload?: unknown })?.payload;
      reject(new Error(typeof p === 'string' ? p : 'WebView 创建失败'));
    };
    void wv.once('tauri://created', onCreated);
    void wv.once('tauri://error', onError);
  });
  const unlisten = await appWindow.onResized(() => {
    // 等一帧：让 DOM 布局先跟随窗口尺寸变化，再取新矩形
    requestAnimationFrame(() => void relayoutWebview(wv, slot));
  });
  await wv.setFocus();
  return { wv, unlisten };
}

/** 窗口缩放 / Dock 调宽后：把 WebView 重贴到内容区槽（同宽同高，无错位） */
async function relayoutWebview(wv: Webview, slot: HTMLElement) {
  const b = boundsFromSlot(slot);
  await wv.setPosition(new LogicalPosition(b.x, b.y)).catch(() => {});
  await wv.setSize(new LogicalSize(b.width, b.height)).catch(() => {});
}

/** 读取内容区 DOM 槽的真实矩形（CSS 像素 = 逻辑像素），作为原生 WebView 的铺位 */
function boundsFromSlot(slot: HTMLElement) {
  const r = slot.getBoundingClientRect();
  return {
    x: Math.round(r.left),
    y: Math.round(r.top),
    width: Math.max(60, Math.round(r.width)),
    height: Math.max(60, Math.round(r.height)),
  };
}

/** 机器人头图标：入口按钮与 Dock 头部共用（描边风格，继承 currentColor） */
export function RobotIcon({ size = 16 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <path d="M5 5.2V4" />
      <path d="M11 5.2V4" />
      <circle cx="5" cy="3.1" r="1" fill="currentColor" stroke="none" opacity="0.9" />
      <circle cx="11" cy="3.1" r="1" fill="currentColor" stroke="none" opacity="0.9" />
      <rect x="3.1" y="5.2" width="9.8" height="7.4" rx="2.5" />
      <circle cx="6.1" cy="9.1" r="1" fill="currentColor" stroke="none" />
      <circle cx="9.9" cy="9.1" r="1" fill="currentColor" stroke="none" />
      <path d="M6.4 11.8h3.2" />
    </svg>
  );
}
