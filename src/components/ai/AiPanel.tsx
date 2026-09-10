import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Webview, getAllWebviews } from '@tauri-apps/api/webview';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { open as openUrl } from '@tauri-apps/plugin-shell';
import { LogicalPosition, LogicalSize } from '@tauri-apps/api/dpi';
import type { UnlistenFn } from '@tauri-apps/api/event';
import { useAiStore, AI_PANEL_MIN_W, AI_PANEL_MAX_W } from '../../stores/aiStore';
import { useUiStore } from '../../stores/uiStore';
import { aiService } from '../../services/aiService';
import { showNotification } from '../ui/Toast';
import { RobotIcon } from './RobotIcon';
import { UpdateControl } from './UpdateControl';

interface AiPanelProps {
  /** 当前项目的工作目录（dsh web 会话根）；null = 未选项目 */
  cwd: string | null;
  /** 当前项目显示名（dsh workspace 标题：GUI 工作区名 = Nexus 项目名） */
  projectName: string | null;
}

/** 左缘细分隔条宽度（DOM） */
const SPLIT_W = 6;
/** Dock 头部高度（DOM） */
const HEADER_H = 36;

/** 子 WebView 唯一标签源（模块级递增：组件重挂载/旧标签未释放也不会撞名报 already exists） */
let wvSeq = 0;
const nextWvLabel = () => `ai-panel-${++wvSeq}`;

/**
 * 全局弹窗打开期间子 WebView 的暂停标志（模块级：guardedRelayout 等模块函数共用）。
 * true 时一切重贴短路——WebView 应停留在屏外，防止窗口 resize/moved/focus 事件
 * 在弹窗打开期间把它贴回可见区（原生层不受 DOM 遮罩约束）。
 * 单实例组件（全局仅一个 AiPanel），模块级安全。
 */
let modalPause = false;

/**
 * 父页面最近一次交互时间戳（模块级：createEmbeddedWebview 回调与组件共享）。
 * 窗口重新聚焦时据此判定「用户最后是否在主界面」：2 秒内有交互 → 不把系统焦点
 * 交给 dsh（保护主界面输入）；否则认为用户最后在 dsh 面板，执行焦点恢复。
 * 单实例组件（全局仅一个 AiPanel），模块级安全。
 */
let lastParentFocusAt = 0;

/**
 * 焦点恢复命令节流（模块级）：窗口聚焦事件理论上一次触发一次，但 SetFocus 后
 * WebView2 的焦点流转可能连带窗口焦点事件再次派发——500ms 窗口内去重，
 * 防止恢复逻辑循环执行（反复重置焦点会打断中文输入法候选，见 ai_focus_restore.js）
 */
let lastFocusCmdAt = 0;

/**
 * 面板收起（显隐模式：只移屏不销毁）期间的重贴暂停标志：WebView 保留在屏外
 * （-20000,0），窗口事件/宽度变化触发的重贴一律短路，防把隐藏的 WebView 贴回
 * 可见区。单实例组件，模块级安全。
 */
let wvHidden = false;

interface Bounds { x: number; y: number; width: number; height: number }

/**
 * WebView 铺位同步器：高频调用（拖拽 60fps）下的 IPC 合入器。
 * - 一次至多一发在途 setPosition/setSize（串行，防堆积）
 * - 期间新目标只记最新（尾随合并）——上一发完成后自动补发，**最新矩形必达**
 * - dispose 后全部短路：销毁后任何迟到的 relayout/retry 都不会再把已关 WebView
 *   贴回可见区（防「关闭后页面复活」）
 */
interface BoundsSync { readonly disposed: boolean; apply(b: Bounds): void; dispose(): void }

function createBoundsSync(wv: Webview): BoundsSync {
  let inflight = false;
  let pending: Bounds | null = null;
  let disposed = false;
  const pump = async () => {
    if (inflight || pending === null) return;
    inflight = true;
    const b = pending;
    pending = null;
    try {
      await wv.setPosition(new LogicalPosition(b.x, b.y));
      await wv.setSize(new LogicalSize(b.width, b.height));
    } catch {
      /* 窗口/WebView 已销毁等瞬态：忽略 */
    }
    inflight = false;
    if (pending !== null) void pump(); // 在途期间又有新目标 → 补发最新
  };
  return {
    get disposed() { return disposed; },
    apply(b) {
      if (disposed) return;
      pending = b;
      void pump();
    },
    dispose() {
      disposed = true;
      pending = null;
    },
  };
}

/**
 * AI 助手停靠面板：以 flex 布局的一列存在（主内容区自动让出宽度，不遮挡任何区域），
 * 点击机器人头开关。dsh web（DeepSeek Harness GUI）以「原生子 WebView」内嵌：
 * - 为什么不用 iframe：打包后父页 origin 是 tauri.localhost，与 dsh 的 localhost
 *   跨站——父页 CSP（default-src 'self'，无 frame-src）直接拦截帧，且 dsh 会话
 *   cookie 为 SameSite=Strict，跨站 iframe 内不收不发 → 401。子 WebView 对 dsh
 *   是顶层导航：不受父页 CSP 约束，Strict cookie 正常携带（dev/打包同一路径）
 * - 会话生命周期由 aiStore 管理：打开启动/切项目重启/关闭隐藏保留/停止关面板；
 *   WebView 本体随「本项目会话在跑 && 面板展开」创建/销毁。原生层不参与 DOM
 *   裁切，关闭时先移出屏幕再 close——close 即使异步/失败也无残留画面
 * - 宽度单一事实来源：静止期 = store.panelWidth（持久化）；拖拽期 = 手动宽度 ref。
 *   React 渲染统一从 ref 取「当前事实」，任何中间渲染都不会用手动宽度打架
 */
export function AiPanel({ cwd, projectName }: AiPanelProps) {
  const panelOpen = useAiStore((s) => s.panelOpen);
  const panelWidth = useAiStore((s) => s.panelWidth);
  const url = useAiStore((s) => s.url);
  const starting = useAiStore((s) => s.starting);
  const lastError = useAiStore((s) => s.lastError);
  /** dsh 安装/升级进行中（错误卡与更新弹窗共用的安装入口，进度统一显示在面板主体） */
  const installing = useAiStore((s) => s.installing);
  /** 本项目的会话是否活跃（进程 running 且归属本项目 —— 物理进程跨项目单例，
   *  展示层必须用 sessionCwd 判别，避免「进程在别的项目」时误亮/误显示） */
  const runningHere = useAiStore((s) => s.running && s.sessionCwd === s.currentCwd);
  /** 是否有全局弹窗打开（添加服务/工具库/更新等）——打开时子 WebView 移出屏幕 */
  const anyModalOpen = useUiStore((s) => s.modalCount > 0);

  const dockRef = useRef<HTMLDivElement | null>(null);
  const dragStartRef = useRef<{ x: number; w: number } | null>(null);
  /** 拖拽排队帧 + 最新鼠标 X（rAF 帧合并：一帧最多一次布局，避免高频 reflow 卡顿） */
  const rafRef = useRef<number | null>(null);
  const pendingXRef = useRef(0);
  /** 拖拽期的实时宽度（DOM 手动宽度的事实来源，防 React 渲染回跳）；非拖拽期 null */
  const manualWRef = useRef<number | null>(null);

  // ── 子 WebView 生命周期（原生控件层，铺在内容区 DOM 槽的真实矩形上）──
  const wvRef = useRef<Webview | null>(null);
  /** 当前 WebView 的铺位同步器（dispose 后一切迟到写入短路） */
  const syncRef = useRef<BoundsSync | null>(null);
  const createLockRef = useRef<Promise<void> | null>(null);
  /** 窗口事件监听集合（resize/moved/focus）：恢复最小化/移动/缩放后都触发重贴 */
  const unlistenWinRef = useRef<UnlistenFn[]>([]);
  /** 内容区 DOM 槽：WebView 以它的真实矩形定位铺位（与头部无错位、随布局变化） */
  const slotRef = useRef<HTMLDivElement | null>(null);
  const [wvReady, setWvReady] = useState(false);
  const [wvError, setWvError] = useState<string | null>(null);
  /** 当前 WebView 对应内容键（url#nonce）与「本 effect 想要的内容键」：
   *  在途创建完成时对照——键不一致 = 创建期间已被关闭/切换/刷新 → 自毁 */
  const createdForRef = useRef('');
  const desiredKeyRef = useRef('');
  /** 刷新计数：头部刷新按钮 → 重建 WebView 重载页面 */
  const [refreshNonce, setRefreshNonce] = useState(0);

  const destroyWv = () => {
    const sync = syncRef.current;
    syncRef.current = null;
    if (sync) sync.dispose(); // 先短路：迟到的 relayout/retry 一律不再写入
    wvHidden = false;
    const wv = wvRef.current;
    wvRef.current = null;
    createdForRef.current = '';
    desiredKeyRef.current = '';
    if (wv) {
      // 先移出可视区再 close：close 是异步的（且可能失败/卡顿），
      // 移屏保证「关闭」在视觉上立即成立，残留只可能是屏外无感句柄
      void wv.setPosition(new LogicalPosition(-20000, 0)).catch(() => {});
      void wv.close().catch(() => {});
    }
    for (const un of unlistenWinRef.current) un();
    unlistenWinRef.current = [];
    setWvReady(false);
    setWvError(null);
  };

  /** 显隐模式的「隐藏」：只移屏保留实例（不 close 不销毁）——重开零重建零闪烁 */
  const hideWv = () => {
    wvHidden = true;
    const wv = wvRef.current;
    if (wv) void wv.setPosition(new LogicalPosition(-20000, 0)).catch(() => {});
  };

  /** 显隐模式的「显示」：移回槽位（两帧后重贴，等面板宽度过渡布局稳定） */
  const showWv = () => {
    wvHidden = false;
    const sync = syncRef.current;
    const slot = slotRef.current;
    if (sync && slot && !sync.disposed) {
      requestAnimationFrame(() => requestAnimationFrame(() => void guardedRelayout(sync, slot)));
    }
  };

  // 挂载引导：恢复宽度 + 载入 per-project 记忆缓存（切换项目零闪烁的前提）；
  // 同时记录父页面交互时间（焦点保护判定用，见 lastParentFocusAt）
  useEffect(() => {
    void useAiStore.getState().bootstrap();
    const onParentFocusIn = () => { lastParentFocusAt = Date.now(); };
    document.addEventListener('focusin', onParentFocusIn);
    return () => {
      document.removeEventListener('focusin', onParentFocusIn);
      destroyWv();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 项目切换/挂载 → 应用该项目记忆的 AI 状态（开→自动恢复会话；关→面板收起）。
  // 每个项目独立记忆，点击项目卡片按记忆展示，不互相打扰
  useEffect(() => {
    void useAiStore.getState().switchProject(cwd, projectName);
  }, [cwd, projectName]);

  // 崩溃自愈：面板开着、本项目应运行（无 error）但进程不在了 → 自动重启
  // （覆盖 dsh 崩溃/被杀后无项目切换动作的场景，避免卡在假 spinner）
  useEffect(() => {
    const st = useAiStore.getState();
    if (panelOpen && !st.starting && !runningHere && !url && cwd && !lastError) {
      void st.ensureRunning(cwd, projectName);
    }
  }, [panelOpen, runningHere, url, cwd, lastError, starting, projectName]);

  // 关闭兜底：无论什么状态（含 installing 撑宽分支/拖拽残留），关闭必达——
  // 以 DOM 为最终裁决强制收 0，杜绝「点了关闭面板还在」
  useEffect(() => {
    if (!panelOpen) {
      manualWRef.current = null;
      if (dockRef.current) dockRef.current.style.width = '0px';
    }
  }, [panelOpen]);

  // 仅当进程归属本项目时才展示其页面（sessionCwd 判别）
  const showFrame = runningHere && !!url;

  // WebView 生命周期：本项目会话在跑 && 面板展开 → 创建/重建（url 或刷新键变化）；
  // 收起（panelOpen=false）或会话不属于本项目（showFrame=false）→ 显隐优先：
  // 面板收起时只移屏保留 WebView（hideWv），重开直接移回（showWv）——不重建、
  // 不重新加载，零闪烁；仅当内容键变化（会话切换/刷新/停止，页面已过期）才真正
  // 销毁。销毁不伤会话：dsh 进程与 cookie 保留，重建时秒载
  useEffect(() => {
    const key = url ? `${url}#${refreshNonce}` : '';
    desiredKeyRef.current = key;
    if (!panelOpen || !showFrame || !url) {
      if (wvRef.current && createdForRef.current !== key) {
        destroyWv(); // 内容键变化（会话切换/刷新/停止）→ 页面过期，真实销毁
      } else if (wvRef.current) {
        hideWv(); // 仅显隐：保留实例移屏，重开零重建
      }
      return;
    }
    if (wvRef.current) {
      if (createdForRef.current === key) {
        showWv(); // 已保留的 WebView：移回原位，无需重建
        return;
      }
      destroyWv(); // url/刷新键变化 → 先销毁再重建
    }
    if (createLockRef.current) return; // 上一次创建仍在途，等它完成（其自查会自毁）
    const slot = slotRef.current;
    if (!slot) return;
    setWvError(null);
    createLockRef.current = (async () => {
      try {
        const { wv, sync, unlistens } = await createEmbeddedWebview(url, slot);
        if (desiredKeyRef.current !== key || !useAiStore.getState().panelOpen) {
          // 创建期间被收起/切项目/刷新 → 不展示过期页面，自毁
          sync.dispose();
          for (const un of unlistens) un();
          await wv.close().catch(() => {});
          return;
        }
        wvRef.current = wv;
        syncRef.current = sync;
        createdForRef.current = key;
        unlistenWinRef.current = unlistens;
        // 弹窗打开期间创建的 WebView：创建后立即贴屏外（初始定位会瞬时可见，
        // 创建完成即移走；弹窗期间的重建场景极少，可接受）
        if (modalPause) void wv.setPosition(new LogicalPosition(-20000, 0)).catch(() => {});
        setWvReady(true);
      } catch (e) {
        setWvError(e instanceof Error ? e.message : String(e));
      } finally {
        createLockRef.current = null;
      }
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [panelOpen, showFrame, url, refreshNonce]);

  // 全局弹窗打开：子 WebView 移出屏幕——原生子 WebView 不受 DOM 遮罩（z-index）
  // 约束，不移走则弹窗打开期间 dsh 页面仍可点击操作。纯移动不销毁：会话进程与
  // 页面状态保留（聊天记录/生成中内容不受影响），弹窗全部关闭后两帧重贴回槽位。
  // modalPause 同步暂停一切重贴，防窗口事件在弹窗期间把它贴回可见区。
  useEffect(() => {
    modalPause = anyModalOpen;
    if (anyModalOpen) {
      const wv = wvRef.current;
      if (wv) void wv.setPosition(new LogicalPosition(-20000, 0)).catch(() => {});
    } else {
      const sync = syncRef.current;
      const slot = slotRef.current;
      if (sync && slot && !sync.disposed) {
        // 等两帧让 DOM 布局稳定后再贴回（与 relayoutNow 同策略）
        requestAnimationFrame(() => requestAnimationFrame(() => void guardedRelayout(sync, slot)));
      }
    }
  }, [anyModalOpen]);

  // 面板宽度变化（拖拽松手落 store）→ WebView 重贴槽位（守卫版：窗口事件/低频率）
  useEffect(() => {
    if (panelOpen && wvReady && syncRef.current && slotRef.current) {
      void guardedRelayout(syncRef.current, slotRef.current);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [panelOpen, wvReady, panelWidth]);

  /** 由鼠标 X 计算夹取后的宽度（主内容至少留 420px） */
  const clampWidth = (startX: number, startW: number, curX: number) => {
    const maxW = Math.min(AI_PANEL_MAX_W, Math.max(AI_PANEL_MIN_W, Math.floor(window.innerWidth - 420)));
    return Math.min(Math.max(startW + (startX - curX), AI_PANEL_MIN_W), maxW);
  };

  // 左缘分隔条拖拽调宽
  // rAF 帧合并：mousemove 高频触发（可达 1000Hz），每帧至多应用一次宽度。
  // 拖拽期宽度只写 manualWRef + DOM（不经 store，避免每帧 React 渲染）；
  // WebView 走同步器尾随合并，最新宽度必达且不堆积
  const onResizeStart = (e: React.MouseEvent) => {
    e.preventDefault();
    dragStartRef.current = { x: e.clientX, w: useAiStore.getState().panelWidth };
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';

    const onMove = (ev: MouseEvent) => {
      if (!dragStartRef.current) return;
      pendingXRef.current = ev.clientX; // 只记最新坐标
      if (rafRef.current !== null) return; // 已有排队帧，等下一帧
      rafRef.current = requestAnimationFrame(() => {
        rafRef.current = null;
        const d = dragStartRef.current;
        if (!d || !dockRef.current) return;
        const w = clampWidth(d.x, d.w, pendingXRef.current);
        manualWRef.current = w;
        dockRef.current.style.width = w + 'px';
        // WebView 同步跟随（原生层不随 DOM 重排；同步器保证拖拽中合入到最新值）
        const sync = syncRef.current;
        const slot = slotRef.current;
        if (sync && slot && !sync.disposed) {
          const b = boundsFromSlot(slot);
          if (b) sync.apply(b);
        }
      });
    };

    const onUp = () => {
      const d = dragStartRef.current;
      dragStartRef.current = null;
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
      if (rafRef.current !== null) {
        cancelAnimationFrame(rafRef.current);
        rafRef.current = null;
      }
      // 松手：以最终鼠标位置落 store 并清除手动宽度（渲染回归 store 事实源）
      if (d && dockRef.current) {
        const finalW = clampWidth(d.x, d.w, pendingXRef.current);
        manualWRef.current = null;
        dockRef.current.style.width = finalW + 'px';
        useAiStore.getState().setPanelWidth(finalW);
      }
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    };

    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
  };

  /** 刷新/重启：后端幂等重启会话（崩溃自愈/切目录）→ URL 变化自动重建 WebView；
   *  页面 JS 崩了但进程在时，刷新键递增同样重建页面 */
  const handleRefresh = () => {
    void useAiStore.getState().ensureRunning(cwd, projectName);
    setRefreshNonce((n) => n + 1);
  };

  /** 停止本项目 AI 会话并关闭（图标转暗；再点机器人头重新启用） */
  const handleStop = () => { void useAiStore.getState().stop(); };

  /**
   * 在系统默认浏览器中打开当前会话。
   * 会话 URL 形如 http://localhost:<port>/?token=…，token 由浏览器自己握手换成
   * 会话 cookie（内嵌 WebView 走的是同一条路），因此外链同样直接登录；
   * 与内嵌面板是同一个 dsh 会话，两边可同时开着。URL 未就绪时为空操作。
   */
  const handleOpenInBrowser = () => {
    const target = useAiStore.getState().url;
    if (!target) return;
    openUrl(target).catch((e) => {
      showNotification({ variant: 'error', title: '在浏览器中打开失败', description: String(e), duration: 5000 });
    });
  };

  /** 关闭面板 = 隐藏（会话与进程保留，图标保持亮；真停止用方块按钮）。
   *  installing 期间也收（进度卡让位）：后端升级不可中断，完成后仍弹成功通知 */
  const handleHide = () => {
    const st = useAiStore.getState();
    // 先移屏再收宽度：原生 WebView 盖在 DOM 之上，宽度收起的瞬间主内容区立即
    // 扩展，若 WebView 还贴在原位置会短暂浮在扩展区上（闪烁）——先让它离开视线
    const wv = wvRef.current;
    if (wv) void wv.setPosition(new LogicalPosition(-20000, 0)).catch(() => {});
    if (st.installing) st.setInstalling(false);
    st.setPanelOpen(false);
  };

  /** 未检测到 dsh（AI 引擎未安装）：错误卡换成「安装」引导而非终端命令提示 */
  const dshMissing = lastError != null && lastError.includes('未检测到 dsh');

  /**
   * 安装/升级 dsh 的唯一执行入口（错误卡「安装 dsh」与更新弹窗「升级到 vX」
   * 都调它）：进度统一显示在面板主体（store.installing 撑住 dock 展开）。
   * 会话在跑 → 先停（Windows 文件锁；stop 会收面板，但 installing 分支盖住
   * 内容区并保持宽度，视觉连续）；装完自动启动「当前项目」的会话；
   * 升级场景 stop 清了启用记忆 → 会话照常启动，但切走再切回需重开（既有语义）
   */
  const handleInstallDsh = async () => {
    if (useAiStore.getState().installing) return;
    const wasOpen = useAiStore.getState().panelOpen; // 安装前记住面板状态（升级需停会话会收面板）
    useAiStore.getState().setInstalling(true);
    try {
      const st = useAiStore.getState();
      if (st.running || st.starting) await st.stop();
      await aiService.upgrade();
      useAiStore.getState().setInstalling(false);
      showNotification({ variant: 'success', title: 'dsh 安装完成', duration: 3000 });
      const s2 = useAiStore.getState();
      if (wasOpen && !s2.panelOpen) {
        // 升级场景：stop() 收过面板 → 恢复展开（含占用记忆）并启会话
        void s2.setPanelOpen(true);
      } else if (s2.currentCwd) {
        // 面板一直开着（错误卡场景）：直接启会话——ensureRunning 开头会清掉
        // 启动失败留下的 lastError，错误卡自然过渡到会话，不残留旧错误
        void s2.ensureRunning(s2.currentCwd, s2.currentName);
      }
    } catch (e) {
      useAiStore.getState().setInstalling(false);
      showNotification({ variant: 'error', title: 'dsh 安装/升级失败', description: String(e), duration: 8000 });
    }
  };

  // 宽度唯一事实来源：拖拽期 = manualWRef（DOM 已应用，React 渲染读到同一值，
  // 不覆盖不跳变）；静止期 = store（持久化）。关闭/无面板 → 0
  const widthFact = manualWRef.current ?? (panelOpen || installing ? panelWidth : 0);

  return (
    <div
      ref={dockRef}
      className="relative h-full flex flex-shrink-0 bg-nexus-surface overflow-hidden"
      // 隐藏 = 宽度收成 0 而非卸载：DOM 头部/状态卡保持挂载；内容区的原生
      // WebView 被移出屏幕（保留实例，见 hideWv，重开移回零重建）。
      // 会话进程保留，重开秒显。安装 dsh 期间即使面板被 stop 收起也保持展开：
      // 进度 spinner 不能没地方显示（点 X 可随时放弃进度显示，见 handleHide）。
      // 注意：不做宽度过渡动画——原生子 WebView 与 CSS 动画天然不同步
      // （重贴会写入过渡中间宽度导致错位）；显隐零闪烁由「保留实例」保证
      style={{ width: widthFact }}
    >
      {/* ── 左缘细分隔条：整高可见（在 WebView 之外，可直接拖拽调宽） ── */}
      <div
        className="relative h-full flex-shrink-0 cursor-col-resize select-none group flex flex-col items-center"
        style={{ width: SPLIT_W }}
        title="拖拽调整宽度"
        onMouseDown={onResizeStart}
      >
        <div className="absolute inset-y-0 left-1/2 -translate-x-1/2 w-px bg-nexus-border group-hover:bg-nexus-accent/70 transition-colors" />
      </div>

      {/* ── 面板内容列 ── */}
      <div className="relative flex-1 min-w-0 flex flex-col">
        {/* ── 头部 ── */}
        <div className="flex items-center gap-1.5 px-3 border-b border-nexus-border flex-shrink-0 select-none" style={{ height: HEADER_H }}>
          <span className="w-5 h-5 flex items-center justify-center text-nexus-accent">
            <RobotIcon size={14} />
          </span>
          <span className="flex-1 truncate text-[12px] text-nexus-text-muted" title="AI 助手（DeepSeek Harness）">
            AI 助手
            {runningHere && <span className="ml-2 text-[10.5px] text-emerald-500/90">● 会话中</span>}
          </span>
          <button
            className="w-6 h-6 flex items-center justify-center rounded text-nexus-muted hover:text-nexus-text hover:bg-nexus-hover/60"
            title="刷新 / 重启会话"
            onClick={handleRefresh}
          >
            <svg width="11" height="11" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round">
              <path d="M10 6a4 4 0 1 1-1.17-2.83" />
              <polyline points="10,1.5 10,4 7.5,4" />
            </svg>
          </button>
          {/* 检查更新（dsh 引擎版本）：确认升级后由 onInstall 统一走面板主体进度 */}
          <UpdateControl onInstall={() => void handleInstallDsh()} />
          {/* 在系统浏览器中打开当前会话（URL 自带 token，握手后即登录）：
              无会话时点击为空操作、title 说明原因（不用 disabled，Chromium 对 disabled 元素不弹 title） */}
          <button
            className="w-6 h-6 flex items-center justify-center rounded text-nexus-muted hover:text-nexus-text hover:bg-nexus-hover/60"
            title={url ? '在浏览器中打开' : 'AI 会话未启动，暂无可打开的页面'}
            onClick={handleOpenInBrowser}
          >
            <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
              <path d="M9 7v2.5a1 1 0 0 1-1 1H2.5a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1H5" />
              <polyline points="7.5,1.5 10.5,1.5 10.5,4.5" />
              <line x1="5.2" y1="6.8" x2="10.5" y2="1.5" />
            </svg>
          </button>
          {/* 不用 disabled（Chromium 对 disabled 元素不触发 title 提示）：
              无会话时点击为空操作，守卫写在 onClick 内 */}
          <button
            className="w-6 h-6 flex items-center justify-center rounded text-nexus-error/80 hover:text-nexus-error hover:bg-nexus-error/10"
            title="停止 AI 会话"
            onClick={() => { if (runningHere || starting) handleStop(); }}
          >
            <svg width="10" height="10" viewBox="0 0 10 10" fill="currentColor" aria-hidden>
              <rect x="1" y="1" width="8" height="8" rx="1" />
            </svg>
          </button>
          <button
            className="w-6 h-6 flex items-center justify-center rounded text-nexus-muted hover:text-nexus-text hover:bg-nexus-hover/60"
            title="关闭面板"
            onClick={handleHide}
          >
            <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.2">
              <line x1="1.5" y1="1.5" x2="10.5" y2="10.5" /><line x1="10.5" y1="1.5" x2="1.5" y2="10.5" />
            </svg>
          </button>
        </div>

        {/* ── 内容区 ──
             分支顺序即防闪烁策略：lastError → 会话内容 → 项目引导 → spinner。
             打开面板瞬间（ensure 未及置位）running/starting 均为 false——
             此时若按旧顺序会命中「未检测到 dsh」等引导分支闪一帧再跳 spinner，
             故一切「未就绪但有项目」的状态一律落 spinner。
             会话内容分支：原生 WebView 铺满整个槽位（slotRef）；未创建成功前
             显示 DOM 覆盖层（WebView 是原生层，一旦创建就盖在 DOM 之上） */}
        <div ref={slotRef} className="relative flex-1 min-h-0 overflow-hidden bg-nexus-editor">
          {installing ? (
            <div className="h-full flex flex-col items-center justify-center gap-3 text-nexus-muted">
              <div className="w-5 h-5 border-2 border-nexus-accent/30 border-t-nexus-accent rounded-full animate-spin" />
              <p className="text-[12px] select-none">正在安装 dsh…（需联网，约 1~2 分钟）</p>
            </div>
          ) : lastError ? (
            <PanelMessage
              title={dshMissing ? '未找到 dsh' : 'AI 会话启动失败'}
              detail={dshMissing
                ? '未检测到 dsh（AI 助手引擎）。点下方按钮联网安装最新版，装好后自动启动会话。'
                : lastError}
              action={dshMissing ? (
                <RetryButton onClick={() => void handleInstallDsh()}>安装 dsh</RetryButton>
              ) : (
                <RetryButton onClick={handleRefresh}>重试</RetryButton>
              )}
            />
          ) : showFrame ? (
            <>
              {wvError ? (
                <PanelMessage
                  title="AI 会话加载失败"
                  detail={wvError}
                  action={<RetryButton onClick={handleRefresh}>重试</RetryButton>}
                />
              ) : !wvReady && (
                <div className="absolute inset-0 flex flex-col items-center justify-center gap-3 text-nexus-muted pointer-events-none">
                  <div className="w-5 h-5 border-2 border-nexus-accent/30 border-t-nexus-accent rounded-full animate-spin" />
                  {/* 文案不随 starting 跳变：打开瞬间第一帧 starting 未置位，
                      与启动中显示同一句话，避免文字闪变 */}
                  <p className="text-[12px] select-none">正在启动 AI 会话…</p>
                </div>
              )}
            </>
          ) : cwd == null ? (
            <PanelMessage
              title="先打开一个项目"
              detail="AI 会话以项目目录为工作区，请先在左侧选择项目"
            />
          ) : (
            <div className="h-full flex flex-col items-center justify-center gap-3 text-nexus-muted">
              <div className="w-5 h-5 border-2 border-nexus-accent/30 border-t-nexus-accent rounded-full animate-spin" />
              <p className="text-[12px] select-none">正在启动 AI 会话…</p>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

/** 面板消息区（错误/引导共用排版） */
function PanelMessage({
  title, detail, action,
}: {
  title: string;
  detail?: string;
  action?: React.ReactNode;
}) {
  return (
    <div className="h-full flex flex-col items-center justify-center gap-2 px-8 text-center">
      <span className="text-nexus-accent/70"><RobotIcon size={28} /></span>
      <p className="text-[13px] text-nexus-text font-medium">{title}</p>
      {detail && <p className="text-[11.5px] text-nexus-muted leading-relaxed break-all">{detail}</p>}
      {action && <div className="mt-1">{action}</div>}
    </div>
  );
}

function RetryButton({ onClick, children }: { onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      className="px-3 py-1 text-[12px] bg-nexus-accent/15 text-nexus-accent rounded-md hover:bg-nexus-accent/25 transition-colors"
      onClick={onClick}
    >{children}</button>
  );
}

/**
 * 创建并定位子 WebView（以内容区 DOM 槽的真实矩形为准，与头部/分隔条无错位）；
 * 返回实例 + 铺位同步器 + 窗口事件监听（resize/moved/focus 三路触发重贴，
 * 低频路径走守卫版 relayout：最小化/无效矩形不写入，错位值进不来）。
 * WebView 对 dsh 是顶层导航，不受父页 CSP 帧策略约束，SameSite=Strict cookie 正常携带
 */
async function createEmbeddedWebview(url: string, slot: HTMLElement) {
  const appWindow = getCurrentWindow();
  const b = boundsFromSlot(slot);
  // 经 Rust 命令创建（带 initialization_script：dsh 页面焦点记忆/恢复脚本安装——
  // JS 侧 new Webview() 无注入脚本入口，原因与实现见 src-tauri/src/commands/ai.rs）
  const label = nextWvLabel();
  await invoke('create_ai_panel_webview', {
    windowLabel: appWindow.label,
    label,
    url,
    x: b?.x ?? 0,
    y: b?.y ?? 0,
    width: b?.width ?? 1,
    height: b?.height ?? 1,
  });
  const all = await getAllWebviews();
  const wv = all.find((w) => w.label === label);
  if (!wv) throw new Error('AI 面板 WebView 创建后未找到实例');
  const sync = createBoundsSync(wv);
  // 三路事件都触发重贴：resize 覆盖窗口缩放/最小化恢复，moved 覆盖跨屏移动，
  // focus 兜底最小化恢复（部分 Windows 组合下恢复不派发 resize，仅焦点回归）。
  // 每路先等两帧让 DOM 布局跟随窗口变化；守卫版 relayout 无效矩形不写入
  const relayoutNow = () => {
    requestAnimationFrame(() => {
      requestAnimationFrame(() => void guardedRelayout(sync, slot));
    });
  };
  const unlistens: UnlistenFn[] = [];
  unlistens.push(await appWindow.onResized(relayoutNow));
  unlistens.push(await appWindow.onMoved(relayoutNow));
  // focus 恢复（true）与失去（false）都进来，guardedRelayout 内部守卫过滤无效写入。
  // 切回窗口时条件恢复 dsh 输入焦点（延迟等窗口焦点流转完成）：
  // - 保护：父页面最近 2 秒内有交互（用户正在主界面输入）→ 不抢焦点；
  // - 否则：调 Rust 命令对子 WebView 的 WebView2 控件窗口 SetFocus（正确语义的
  //   系统焦点交接，见 commands/ai.rs::ai_panel_focus——wry 的 set_focus 底层是
  //   MoveFocus，语义错误会破坏输入，刻意不用）+ 页面内 DOM 恢复脚本自动兜底
  unlistens.push(await appWindow.onFocusChanged(({ payload: focused }) => {
    relayoutNow();
    if (!focused) return;
    setTimeout(() => {
      if (Date.now() - lastFocusCmdAt < 500) return; // 节流：防焦点流转链重复触发
      lastFocusCmdAt = Date.now();
      if (Date.now() - lastParentFocusAt < 2000) return; // 主界面刚被操作，不抢
      void invoke('ai_panel_focus', { windowLabel: appWindow.label, label }).catch((e) => console.error('恢复 dsh 焦点失败:', e));
    }, 150);
  }));
  return { wv, sync, unlistens };
}

/**
 * 守卫版重贴（窗口事件/低频路径）：窗口最小化中或槽位矩形无效（<40px，
 * 最小化过渡期 DOM 布局未就绪会读到 0）→ 跳过并稍后重试——绝不写坏矩形。
 * dispose 后短路（destroyWv 先 dispose，迟到的重试不会复活已关页面）
 */
async function guardedRelayout(sync: BoundsSync, slot: HTMLElement, retries = 4) {
  if (sync.disposed) return;
  if (modalPause) return; // 全局弹窗打开期间禁止重贴（WebView 应停留在屏外）
  if (wvHidden) return;   // 面板收起（显隐保留）期间禁止重贴（WebView 停留在屏外）
  const appWindow = getCurrentWindow();
  let minimized = false;
  try {
    minimized = await appWindow.isMinimized();
  } catch {
    /* 窗口已关闭等瞬态，按未最小化处理让下方 bounds 校验兜底 */
  }
  if (minimized) return;
  const b = boundsFromSlot(slot);
  if (b === null) {
    // 布局未就绪（恢复过渡/隐藏中）：稍后重试，不写入坏值
    if (retries > 0) {
      setTimeout(() => void guardedRelayout(sync, slot, retries - 1), 80);
    }
    return;
  }
  sync.apply(b);
}

/**
 * 读取内容区 DOM 槽的真实矩形（CSS 像素 = 逻辑像素），作为原生 WebView 的铺位。
 * 宽或高 < 40px 视为无效（面板收起/窗口最小化过渡期）→ null，调用方跳过写入
 */
function boundsFromSlot(slot: HTMLElement): Bounds | null {
  const r = slot.getBoundingClientRect();
  if (r.width < 40 || r.height < 40) return null;
  return {
    x: Math.round(r.left),
    y: Math.round(r.top),
    width: Math.round(r.width),
    height: Math.round(r.height),
  };
}
