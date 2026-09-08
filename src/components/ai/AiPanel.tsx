import { useEffect, useRef, useState } from 'react';
import { useAiStore, AI_PANEL_MIN_W, AI_PANEL_MAX_W } from '../../stores/aiStore';
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

/**
 * AI 助手停靠面板：以 flex 布局的一列存在（主内容区自动让出宽度，不遮挡任何区域），
 * 点击机器人头开关。dsh web（DeepSeek Harness GUI）以 <iframe> 内嵌：
 * - 会话 cookie 为 SameSite=Strict，iframe 必须与父页同站 → 开发形态父页是
 *   http://localhost:1420（vite），后端已把会话 URL host 规范为 localhost
 * - 会话生命周期由 aiStore 管理：打开启动/切项目重启/关闭隐藏保留/停止关面板
 * - 左缘分隔条拖拽调宽（rAF 帧合并；拖拽期间 iframe 置 pointer-events:none，
 *   否则鼠标进入 iframe 文档后父窗口收不到事件）
 * - 显示状态机不闪帧：面板打开瞬间（starting 未及置位）一律落 spinner，
 *   绝不先渲染错误/引导卡再跳 spinner
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

  const dockRef = useRef<HTMLDivElement | null>(null);
  const dragStartRef = useRef<{ x: number; w: number } | null>(null);
  /** 拖拽排队帧 + 最新鼠标 X（rAF 帧合并：一帧最多一次布局，避免高频 reflow 卡顿） */
  const rafRef = useRef<number | null>(null);
  const pendingXRef = useRef(0);
  /** dsh iframe（拖拽期间置 pointer-events:none —— iframe 是独立文档，会吞掉
   *  父窗口的 mousemove/mouseup，导致拖拽进入其区域即失效） */
  const frameRef = useRef<HTMLIFrameElement | null>(null);
  /** 刷新计数：头部刷新按钮 → key 重建 iframe 重载页面 */
  const [refreshNonce, setRefreshNonce] = useState(0);

  // 挂载引导：恢复宽度 + 载入 per-project 记忆缓存（切换项目零闪烁的前提）
  useEffect(() => {
    void useAiStore.getState().bootstrap();
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

  // 仅当进程归属本项目时才展示其页面（sessionCwd 判别）
  const showFrame = runningHere && !!url;

  /** 由鼠标 X 计算夹取后的宽度（主内容至少留 420px） */
  const clampWidth = (startX: number, startW: number, curX: number) => {
    const maxW = Math.min(AI_PANEL_MAX_W, Math.max(AI_PANEL_MIN_W, Math.floor(window.innerWidth - 420)));
    return Math.min(Math.max(startW + (startX - curX), AI_PANEL_MIN_W), maxW);
  };

  // 左缘分隔条拖拽调宽
  // rAF 帧合并：mousemove 高频触发（可达 1000Hz），每帧至多应用一次宽度——
  // 每次宽度变化都会让主内容区与内嵌 dsh 页面整体重排，不合并会明显卡顿
  const onResizeStart = (e: React.MouseEvent) => {
    e.preventDefault();
    dragStartRef.current = { x: e.clientX, w: useAiStore.getState().panelWidth };
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';
    // 鼠标拖入 iframe 区域后事件归 iframe 文档所有，父窗口收不到 → 拖拽失效。
    // 期间让 iframe 对鼠标透明（事件穿透回父文档），松手恢复
    if (frameRef.current) frameRef.current.style.pointerEvents = 'none';

    const onMove = (ev: MouseEvent) => {
      if (!dragStartRef.current) return;
      pendingXRef.current = ev.clientX; // 只记最新坐标
      if (rafRef.current !== null) return; // 已有排队帧，等下一帧
      rafRef.current = requestAnimationFrame(() => {
        rafRef.current = null;
        const d = dragStartRef.current;
        if (!d || !dockRef.current) return;
        dockRef.current.style.width = clampWidth(d.x, d.w, pendingXRef.current) + 'px';
      });
    };

    const onUp = () => {
      const d = dragStartRef.current;
      dragStartRef.current = null;
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
      if (frameRef.current) frameRef.current.style.pointerEvents = '';
      if (rafRef.current !== null) {
        cancelAnimationFrame(rafRef.current);
        rafRef.current = null;
      }
      // 松手：用最终鼠标位置同步一次宽度并持久化（不依赖最后帧是否已执行）
      if (d && dockRef.current) {
        const finalW = clampWidth(d.x, d.w, pendingXRef.current);
        dockRef.current.style.width = finalW + 'px';
        useAiStore.getState().setPanelWidth(finalW);
      }
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    };

    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
  };

  /** 刷新/重启：后端幂等重启会话（崩溃自愈/切目录）→ URL 变化自动重载；
   *  同时重建 iframe（页面 JS 崩了但进程在时也能恢复） */
  const handleRefresh = () => {
    void useAiStore.getState().ensureRunning(cwd, projectName);
    setRefreshNonce((n) => n + 1);
  };

  /** 停止本项目 AI 会话并关闭（图标转暗；再点机器人头重新启用） */
  const handleStop = () => { void useAiStore.getState().stop(); };

  /** 关闭面板 = 隐藏（会话与进程保留，图标保持亮；真停止用方块按钮） */
  const handleHide = () => useAiStore.getState().setPanelOpen(false);

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

  return (
    <div
      ref={dockRef}
      className="relative h-full flex flex-shrink-0 bg-nexus-surface overflow-hidden"
      // 隐藏 = 宽度收成 0 而非卸载：iframe 保持挂载，dsh 页面（输入框/滚动/
      // 会话状态）原样保留——重开只是一次 resize，秒回不闪。
      // 真正卸载 iframe 会销毁页面文档，重开需整页重载（重 SPA，明显闪烁）
      // 安装 dsh 期间即使面板被 stop 收起也保持展开：进度 spinner 不能没地方显示
      style={{ width: panelOpen || installing ? panelWidth : 0 }}
    >
      {/* ── 左缘细分隔条：整高可见，按住可拖拽调宽 ── */}
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
             故一切「未就绪但有项目」的状态一律落 spinner */}
        <div className="relative flex-1 min-h-0 overflow-hidden bg-nexus-editor">
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
            <SessionFrame
              key={url + '#' + refreshNonce}  // 会话/刷新变化 → 重挂载 → loaded 复位（无帧序闪）
              url={url}
              width={panelWidth - SPLIT_W}
              frameRef={frameRef}
            />
          ) : cwd == null ? (
            <PanelMessage
              title="先打开一个项目"
              detail="AI 会话以项目目录为工作区，请先在左侧选择项目"
            />
          ) : (
            <div className="h-full flex flex-col items-center justify-center gap-3 text-nexus-muted">
              <div className="w-5 h-5 border-2 border-nexus-accent/30 border-t-nexus-accent rounded-full animate-spin" />
              {/* 文案不随 starting 跳变：打开瞬间第一帧 starting 未置位，
                  与启动中显示同一句话，避免文字闪变 */}
              <p className="text-[12px] select-none">正在启动 AI 会话…</p>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

/**
 * 会话 iframe + 加载遮罩。作为独立组件用 key 驱动：
 * url/刷新变化 → 组件重挂载 → loaded 初始 false → 遮罩与 iframe 同帧出现，
 * 不存在「新文档已开始加载但遮罩尚未复位」的帧序漏洞。
 * iframe 宽度固定为面板内容宽（px）：面板隐藏时父容器宽 0 只做裁切，
 * iframe 本身不被压缩 —— dsh 页面不重排、合成帧保留，恢复显示零闪烁
 */
function SessionFrame({
  url, width, frameRef,
}: {
  url: string;
  width: number;
  frameRef: React.Ref<HTMLIFrameElement>;
}) {
  /** 文档是否已加载完成；onLoad 后延迟 250ms 再揭开（等 dsh SPA 首屏稳定） */
  const [loaded, setLoaded] = useState(false);
  const revealTimer = useRef<number | null>(null);

  useEffect(() => () => {
    if (revealTimer.current !== null) clearTimeout(revealTimer.current);
  }, []);

  const onLoad = () => {
    if (revealTimer.current !== null) clearTimeout(revealTimer.current);
    revealTimer.current = window.setTimeout(() => setLoaded(true), 250);
  };

  return (
    <>
      {!loaded && (
        <div className="absolute inset-0 flex flex-col items-center justify-center gap-3 text-nexus-muted pointer-events-none bg-nexus-editor">
          <div className="w-5 h-5 border-2 border-nexus-accent/30 border-t-nexus-accent rounded-full animate-spin" />
          <p className="text-[12px] select-none">正在启动 AI 会话…</p>
        </div>
      )}
      <iframe
        ref={frameRef}
        src={url}
        title="AI 助手会话"
        className="h-full border-0 bg-nexus-editor"
        style={{ width }}
        onLoad={onLoad}
      />
    </>
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
