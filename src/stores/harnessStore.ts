import { create } from 'zustand';
import type { UnlistenFn } from '@tauri-apps/api/event';
import { listen } from '@tauri-apps/api/event';
import { harnessService } from '../services/harnessService';
import { layoutApi } from '../services/service';
import type {
  HarnessDshInstalledEvent,
  HarnessErrorEvent,
  HarnessRunningEvent,
  HarnessStatus,
} from '../services/harnessService';

interface HarnessState {
  /** 内嵌 WebView 是否显示（机器人头按钮开关） */
  panelOpen: boolean;
  /** AI Dock 宽度（逻辑像素；拖拽调节并持久化） */
  panelWidth: number;
  running: boolean;
  /** dsh web 启动地址（带 token） */
  url: string | null;
  pid: number | null;
  starting: boolean;
  /** dsh CLI 是否存在于 PATH；null = 尚未检测 */
  dshFound: boolean | null;
  /** 正在后台 npm 安装 dsh */
  installing: boolean;
  lastError: string | null;
  /** 当前运行会话的工作目录（判断归属项目；null = 未知/未指定） */
  sessionCwd: string | null;

  togglePanel: () => void;
  setPanelOpen: (open: boolean) => void;
  /** 设置 Dock 宽度（防抖持久化到布局库） */
  setPanelWidth: (width: number) => void;
  init: () => Promise<void>;
  dispose: () => void;
  refreshStatus: () => Promise<void>;
  /** 确保 dsh web 运行在给定项目目录：目录不符时自动重启会话（后端为准） */
  ensureRunning: (cwd: string | null) => Promise<boolean>;
  /** 后台安装 dsh（npm i -g），装好后自动 ensureRunning */
  installDsh: (cwd: string | null) => Promise<void>;
  stop: () => Promise<void>;
  clearError: () => void;
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Dock 宽度范围（逻辑像素） */
const PANEL_MIN_W = 340;
const PANEL_MAX_W = 900;

let widthSaveTimer: ReturnType<typeof setTimeout> | null = null;
function scheduleWidthSave(width: number) {
  if (widthSaveTimer) clearTimeout(widthSaveTimer);
  widthSaveTimer = setTimeout(() => {
    layoutApi.save({ ai_panel_width: String(Math.round(width)) }).catch((e) => console.error('保存 AI 面板宽度失败:', e));
  }, 500);
}

/** 与后端 start 超时（60s）对齐；略留余量避免同时超时 */
const WAIT_URL_TIMEOUT = 60_000;

let unlisteners: UnlistenFn[] | null = null;
let initPromise: Promise<void> | null = null;

function applyStatus(s: HarnessStatus) {
  useHarnessStore.setState({
    running: s.running,
    url: s.url,
    pid: s.pid,
    dshFound: s.dshFound,
    sessionCwd: s.cwd,
  });
}

function ensureListeners() {
  if (unlisteners) return;
  unlisteners = [];

  const register = async (event: string, handler: (payload: unknown) => void) => {
    const fn = await listen<unknown>(event, (e) => handler(e.payload));
    if (unlisteners) unlisteners.push(fn);
    else fn();
  };

  void register('harness-running', (payload) => {
    const p = payload as HarnessRunningEvent;
    const patch: Partial<HarnessState> = { running: p.running, starting: false };
    if (p.running) {
      if (p.url) patch.url = p.url;
    } else {
      patch.url = null;
      patch.pid = null;
    }
    useHarnessStore.setState(patch);
  });

  void register('harness-error', (payload) => {
    const p = payload as HarnessErrorEvent;
    useHarnessStore.setState({ starting: false, lastError: p.error });
  });

  void register('harness-dsh-installed', (payload) => {
    const p = payload as HarnessDshInstalledEvent;
    if (!p.ok) {
      useHarnessStore.setState({ installing: false, lastError: p.error ?? 'dsh 安装失败' });
    }
  });
}

async function waitForUrl(timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const st = useHarnessStore.getState();
    if (st.lastError) return false;
    if (st.running && st.url) return true;
    try {
      const s = await harnessService.status();
      applyStatus(s);
      if (s.running && s.url) return true;
      if (s.dshFound === false) return false;
    } catch {
      // 状态查询瞬态失败：继续轮询
    }
    await sleep(700);
  }
  return false;
}

export const useHarnessStore = create<HarnessState>((set, get) => ({
  panelOpen: false,
  panelWidth: 460,
  running: false,
  url: null,
  pid: null,
  starting: false,
  dshFound: null,
  installing: false,
  lastError: null,
  sessionCwd: null,

  togglePanel: () => set((st) => ({ panelOpen: !st.panelOpen })),
  setPanelOpen: (open) => set({ panelOpen: open }),

  setPanelWidth: (width) => {
    const w = Math.min(Math.max(width, PANEL_MIN_W), PANEL_MAX_W);
    set({ panelWidth: w });
    scheduleWidthSave(w);
  },

  init: async () => {
    if (!initPromise) {
      initPromise = (async () => {
        ensureListeners();
        await useHarnessStore.getState().refreshStatus();
        // 恢复上次拖拽保存的 Dock 宽度
        try {
          const l = await layoutApi.load();
          const saved = Number(l.ai_panel_width);
          if (Number.isFinite(saved) && saved > 0) {
            set({ panelWidth: Math.min(Math.max(saved, PANEL_MIN_W), PANEL_MAX_W) });
          }
        } catch {
          // 读不到就用默认宽度
        }
      })();
    }
    return initPromise;
  },

  dispose: () => {
    if (widthSaveTimer) {
      clearTimeout(widthSaveTimer);
      widthSaveTimer = null;
    }
    if (!unlisteners) return;
    for (const fn of unlisteners) fn();
    unlisteners = null;
    initPromise = null;
  },

  refreshStatus: async () => {
    try {
      const s = await harnessService.status();
      applyStatus(s);
    } catch {
      // 后端不可用时静默
    }
  },

  /**
   * 确保 dsh web 以给定项目目录在运行。自愈循环：
   *  1. 状态未知（dshFound null）或运行中但无会话目录 → 先向后端校准；
   *  2. 已运行但目录与目标不符（切了项目）→ 停止后以新目录重启；
   *  3. 真正启动并等 URL（超时与后端 60s 对齐），返回前统一复位 starting。
   * 返回 true = 可用（running 且目录匹配或目标不限）。
   */
  ensureRunning: async (cwd) => {
    for (;;) {
      let st = get();
      if (st.dshFound === null || (st.running && st.sessionCwd === null)) {
        await useHarnessStore.getState().refreshStatus();
        st = get();
      }
      if (st.dshFound === false) {
        // 未安装：不尝试启动，交给界面引导安装
        set({ starting: false, lastError: null });
        return false;
      }
      if (st.running && st.url) {
        // 目录无约束（cwd 未知）或会话本就属于该目录 → 直接可用
        if (cwd === null || st.sessionCwd === null || st.sessionCwd === cwd) return true;
        // 会话运行在别的项目目录：停掉旧会话，以新目录重启
        await useHarnessStore.getState().stop();
        continue;
      }
      if (st.starting) {
        // 已有启动在途：等它出结果，回到循环顶部统一校验（含目录）
        const ok = await waitForUrl(WAIT_URL_TIMEOUT);
        if (!ok) set({ starting: false });
        continue;
      }
      // 真正启动
      set({ starting: true, lastError: null });
      try {
        await harnessService.start(cwd);
      } catch (e) {
        set({ starting: false, lastError: String(e) });
        return false;
      }
      const ok = await waitForUrl(WAIT_URL_TIMEOUT);
      if (!ok) {
        set({ starting: false });
        return false;
      }
      // 启动成功 → 回到循环顶部确认会话归属目录一致后再返回
    }
  },

  installDsh: async (cwd) => {
    if (get().installing) return;
    set({ installing: true, lastError: null });
    try {
      await harnessService.install();
    } catch (e) {
      set({ installing: false, lastError: String(e) });
      return;
    }
    // 轮询直至 dsh 出现在 PATH（npm 装完需重解析 PATH，重启应用才必然生效，
    // 这里先尝试继续；若仍找不到提示重启应用）
    const deadline = Date.now() + 240000;
    while (Date.now() < deadline) {
      await sleep(1500);
      try {
        const s = await harnessService.status();
        applyStatus(s);
        if (s.dshFound) {
          set({ installing: false });
          void get().ensureRunning(cwd);
          return;
        }
      } catch {
        // 忽略瞬态错误继续等
      }
    }
    set({
      installing: false,
      lastError: 'dsh 安装超时。若已安装成功，请重启 Nexus 后重试',
    });
  },

  stop: async () => {
    try {
      await harnessService.stop();
    } catch {
      // 进程可能已退出
    }
    set({ running: false, url: null, pid: null, starting: false, sessionCwd: null });
  },

  clearError: () => set({ lastError: null }),
}));
