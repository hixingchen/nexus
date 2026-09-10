import { create } from 'zustand';
import { aiService } from '../services/aiService';
import { layoutApi } from '../services/service';

/** 面板宽度范围（逻辑像素） */
export const AI_PANEL_MIN_W = 320;
export const AI_PANEL_MAX_W = 900;

/** AI 启用记忆在布局库的键前缀（历史遗留：新版本不再写入，仅启动时清残留行） */
const AI_ON_KEY_PREFIX = 'ai_on_';

// ── per-project 记忆缓存（仅本次运行内） ──────────────────────
// 切项目必须**同步**判定目标项目状态（异步读 DB 会制造面板闪烁窗口）。
// 记忆**不落盘**：本次运行内点过机器人头启用 → 切走再切回自动恢复；
// 重启后一律回到未启用——「上次开着」不构成下次自动开的理由，
// 用户没点过机器人头，面板就不该自己弹出来。
//
// 两条记忆分开存（会话归属 ≠ 面板显隐）：
// - aiOnCache：本项目是否占用 AI 会话（全局唯一占用制，决定切回是否重启/接管会话）
// - aiPanelCache：本项目面板上次是开着还是关着（关面板只是隐藏，会话照跑，但
//   "关着"也要跟着项目走——关掉面板再切走，切回来不该自己弹开）

const aiOnCache = new Map<string, boolean>();
const aiPanelCache = new Map<string, boolean>();

/** 同步读某项目记忆（本次运行内未启用过 → false） */
function aiOnOf(cwd: string): boolean {
  return aiOnCache.get(cwd) ?? false;
}

/** 同步记缓存（仅内存，不持久化） */
function rememberAiOn(cwd: string | null, on: boolean) {
  if (!cwd) return;
  aiOnCache.set(cwd, on);
}

/** 同步读某项目上次的面板显隐（本次运行内没开过 → false） */
function aiPanelWasOpen(cwd: string): boolean {
  return aiPanelCache.get(cwd) ?? false;
}

/** 同步记面板显隐记忆（仅内存，不持久化；当前项目以 cwd 为键） */
function rememberPanelOpen(cwd: string | null, open: boolean) {
  if (!cwd) return;
  aiPanelCache.set(cwd, open);
}

/**
 * AI 会话全局唯一「占用制」：启用项目 X 时，把所有其它项目的开启记忆清零。
 * 语义：一个项目用 AI 图标打开 = 占用会话，其它项目随之关闭（进程被停，
 * 记忆也被清）——切回它们时面板关闭、图标熄灭，需再点图标才启用。
 * 只清「会话占用」记忆，不动各项目的面板显隐偏好（那是用户自己的开关心意）。
 * 记忆仅本次运行内有效（缓存驱动），启动时缓存为空 = 全部未启用
 */
function occupyAiOn(cwd: string) {
  for (const [key, on] of aiOnCache) {
    if (on && key !== cwd) rememberAiOn(key, false);
  }
  rememberAiOn(cwd, true);
}

interface AiState {
  /** 面板是否显示（当前项目的展示状态） */
  panelOpen: boolean;
  /** 面板宽度（逻辑像素；拖拽分隔条调节并持久化） */
  panelWidth: number;
  /** dsh 会话进程是否在运行（物理真相，跨项目单例） */
  running: boolean;
  /** 会话 URL（子 WebView 导航目标） */
  url: string | null;
  /** 会话进程归属的目录（判断图标亮暗：亮 = running 且 sessionCwd == 当前项目） */
  sessionCwd: string | null;
  /** 当前展示的项目目录 */
  currentCwd: string | null;
  /** 当前展示的项目名（传给后端做 workspace 标题） */
  currentName: string | null;
  /** 正在请求启动（防重复请求 + 面板显示进度） */
  starting: boolean;
  /** dsh 安装/升级进行中：面板主体展示进度；期间禁止启动会话（防撞半安装的二进制） */
  installing: boolean;
  /** 最近一次启动错误（面板展示） */
  lastError: string | null;
  /** 在途请求期间目标变化：记录最新目标，完成后自动追启 */
  pendingTarget: { cwd: string; name: string | null } | null;

  /** 挂载引导：载入上次拖拽宽度 + 清理旧版本遗留的 AI 记忆行（记忆本身仅本次运行内） */
  bootstrap: () => Promise<void>;
  /** 切换项目：应用目标项目记忆——本项目占用会话且上次面板是开着的 → 自动恢复；
      否则面板关（同步判定，无异步窗口） */
  switchProject: (cwd: string | null, name: string | null) => void;
  /** 机器人头按钮：开→启用本项目 AI；关→隐藏面板（会话保留，图标仍亮；记下"关着"） */
  togglePanel: () => void;
  /** 打开/隐藏面板（只动可见性并记下该项目的显隐；隐藏保留会话与记忆） */
  setPanelOpen: (open: boolean) => void;
  /** 停止本项目 AI 会话并关闭（记忆写 0，图标转暗） */
  stop: () => Promise<void>;
  /** 设置面板宽度（限幅 + 立即持久化） */
  setPanelWidth: (width: number) => void;
  /** 确保会话运行在指定目录（后端幂等：同目录复用/目录变重启/崩溃自愈） */
  ensureRunning: (cwd: string | null, projectName?: string | null) => Promise<void>;
  setInstalling: (v: boolean) => void;
  clearError: () => void;
}

export const useAiStore = create<AiState>((set, get) => ({
  panelOpen: false,
  panelWidth: 560,
  running: false,
  url: null,
  sessionCwd: null,
  currentCwd: null,
  currentName: null,
  starting: false,
  installing: false,
  lastError: null,
  pendingTarget: null,

  bootstrap: async () => {
    // 只恢复拖拽宽度。AI 启用记忆已改为「仅本次运行内」：旧版本遗留在布局库的
    // ai_on_* 行不再读取，启动时清空，避免残留行继续误导
    try {
      const l = await layoutApi.load();
      const saved = Number(l.ai_panel_width);
      if (Number.isFinite(saved) && saved > 0) {
        set({ panelWidth: Math.min(Math.max(Math.round(saved), AI_PANEL_MIN_W), AI_PANEL_MAX_W) });
      }
      const legacy: Record<string, string> = {};
      for (const k of Object.keys(l)) {
        if (k.startsWith(AI_ON_KEY_PREFIX)) legacy[k] = '';
      }
      if (Object.keys(legacy).length > 0) {
        layoutApi.save(legacy).catch((e) => console.error('清理遗留 AI 状态失败:', e));
      }
    } catch {
      // 布局库读失败：宽度用默认；记忆本就为空（本次运行尚未启用过任何项目）
    }
  },

  switchProject: (cwd, name) => {
    const nextName = name?.trim() || null;
    // currentCwd 先切：展示层 runningHere 立即按新项目判定，旧项目页面
    // 不会残留闪现；记忆同步判定（缓存），无异步窗口
    set({ currentCwd: cwd, currentName: nextName });
    if (!cwd) {
      set({ panelOpen: false, lastError: null });
      return;
    }
    if (aiOnOf(cwd) && aiPanelWasOpen(cwd)) {
      set({ panelOpen: true, lastError: null });
      void get().ensureRunning(cwd, nextName);
    } else {
      // 未占用会话，或上次离开时面板是关着的 → 面板保持关（不覆盖显隐记忆：
      // 记忆只由用户动作改写，切项目本身不该改偏好）
      set({ panelOpen: false, lastError: null });
    }
  },

  togglePanel: () => {
    const { panelOpen, currentCwd, currentName } = get();
    if (panelOpen) {
      // 关闭 = 隐藏：会话与进程保留（图标仍亮），占用记忆保持；只记下"本项目面板关着"
      rememberPanelOpen(currentCwd, false);
      set({ panelOpen: false });
    } else {
      if (!currentCwd) return; // 未选项目不启用
      occupyAiOn(currentCwd); // 占用：清其它项目记忆 + 写本项目
      rememberPanelOpen(currentCwd, true);
      set({ panelOpen: true, lastError: null });
      void get().ensureRunning(currentCwd, currentName);
    }
  },

  setPanelOpen: (open) => {
    if (open === get().panelOpen) return;
    const { currentCwd, currentName } = get();
    if (open) {
      if (!currentCwd) return;
      occupyAiOn(currentCwd); // 占用：清其它项目记忆 + 写本项目
      rememberPanelOpen(currentCwd, true);
      set({ panelOpen: true, lastError: null });
      void get().ensureRunning(currentCwd, currentName);
    } else {
      // 隐藏：会话与进程保留；记下"本项目面板关着"，切走再切回不自动弹开
      rememberPanelOpen(currentCwd, false);
      set({ panelOpen: false });
    }
  },

  stop: async () => {
    const { currentCwd } = get();
    rememberAiOn(currentCwd, false);
    // 不设 starting 守卫：启动在途时停止 = 取消启动（后端 epoch 自杀，Err「已取消」当正常路径）
    set({ panelOpen: false, running: false, url: null, sessionCwd: null, lastError: null });
    try {
      await aiService.stop();
    } catch (e) {
      console.error('停止 dsh 会话失败:', e);
    }
  },

  setPanelWidth: (width) => {
    const w = Math.min(Math.max(width, AI_PANEL_MIN_W), AI_PANEL_MAX_W);
    set({ panelWidth: w });
    layoutApi.save({ ai_panel_width: String(Math.round(w)) }).catch((e) => console.error('保存 AI 面板宽度失败:', e));
  },

  ensureRunning: async (cwd, projectName = null) => {
    if (cwd == null) return;
    if (get().installing) return; // 安装 dsh 期间不启会话：npm 替换全局包时启动可能读半装文件
    if (get().starting) {
      // 在途请求：记录最新目标，完成后由下方收敛逻辑自动追启
      const p = get().pendingTarget;
      const name = projectName?.trim() || null;
      if (!p || p.cwd !== cwd || p.name !== name) set({ pendingTarget: { cwd, name } });
      return;
    }
    set({ starting: true, lastError: null });
    try {
      const s = await aiService.start(cwd, projectName);
      set({
        running: s.running,
        url: s.url,
        sessionCwd: s.cwd,
        starting: false,
        lastError: null,
        pendingTarget: null,
      });
    } catch (e) {
      const msg = String(e);
      // 「已取消」= 启动被 stop 打断（后端 epoch 自杀）：当正常路径，不展示错误
      if (!msg.startsWith('已取消')) set({ lastError: msg });
      set({ starting: false, running: false, url: null, sessionCwd: null });
    }
    // 收敛：请求期间目标又变了 → 补一次最新目标的启动
    const pending = get().pendingTarget;
    if (pending != null && pending.cwd !== cwd) {
      set({ pendingTarget: null });
      void get().ensureRunning(pending.cwd, pending.name);
    }
  },

  setInstalling: (v) => set({ installing: v }),

  clearError: () => set({ lastError: null }),
}));
