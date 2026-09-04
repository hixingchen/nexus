import { create } from 'zustand';
import { opencodeApi } from '../services/service';

// 操作序号：一次启动/下载期间若被 close/新 open 取代（store 状态变了但 invoke 还在飞），
// 迟到的响应不得覆盖当前状态，且要把自己刚拉起的服务停掉（close 的语义 = 杀掉服务）
let seq = 0;

/** 会话目标：port 为 null 表示 serve 仍在启动中（npx 首启可能下载） */
export interface OpenCodeSession {
  cwd: string;
  name: string;
  port: number | null;
  /** 用的是应用自管二进制（可在面板头部「更新」） */
  managed: boolean;
}

export type OpenCodeErrorKind = 'start' | 'install' | 'missing';

export interface OpenCodeError {
  message: string;
  /** missing = 本机没有任何可用的 opencode/npx（面板显示「下载并启动」） */
  kind: OpenCodeErrorKind;
}

interface OpenCodeState {
  session: OpenCodeSession | null;
  /** 面板展开（会话可隐藏保活，同项目回来秒开） */
  visible: boolean;
  /** serve 启动中 */
  starting: boolean;
  /** 下载/更新二进制中（先于 starting） */
  installing: boolean;
  error: OpenCodeError | null;

  /** 打开/切换会话：目标项目与会话不同 → 重启服务（后端单例先杀旧的）；相同且就绪 → 展开 */
  open: (cwd: string, name: string, exePath?: string | null) => Promise<void>;
  /** 下载（force=false）或强制更新（force=true）自管二进制后启动到项目目录 */
  installAndOpen: (cwd: string, name: string, force: boolean) => Promise<void>;
  /** 收起面板（保活） */
  hide: () => void;
  /** 关闭：杀后端服务 + 清会话 */
  close: () => Promise<void>;
}

/** 后端错误转结构化：NEED_BOOTSTRAP 前缀 → missing，其余按启动失败处理 */
function parseStartError(e: unknown): OpenCodeError {
  const raw = String(e);
  if (raw.startsWith('NEED_BOOTSTRAP:')) {
    return { message: raw.slice('NEED_BOOTSTRAP:'.length).trim(), kind: 'missing' };
  }
  return { message: raw, kind: 'start' };
}

async function launch(set: (p: Partial<OpenCodeState>) => void, my: number, cwd: string, name: string, exePath: string | null) {
  try {
    const info = await opencodeApi.start(cwd, exePath);
    if (my !== seq) {
      // 期间被 close/切项目取代：撤掉本次拉起的服务（单例后端此刻跑的是新会话）
      await opencodeApi.stop().catch(() => {});
      return;
    }
    set({ session: { cwd, name, port: info.port, managed: info.managed }, starting: false });
  } catch (e) {
    if (my !== seq) return;
    const error = parseStartError(e);
    set({ starting: false, error });
  }
}

export const useOpenCodeStore = create<OpenCodeState>((set, get) => ({
  session: null,
  visible: false,
  starting: false,
  installing: false,
  error: null,

  open: async (cwd, name, exePath = null) => {
    const st = get();
    if (st.starting || st.installing) return; // 已在流转中，忽略重复点击
    const session = st.session;
    if (session?.cwd === cwd && session.port !== null) {
      // 同项目已就绪（可能隐藏保活中）→ 直接展开
      set({ visible: true, error: null });
      return;
    }
    const my = ++seq;
    set({ session: { cwd, name, port: null, managed: false }, visible: true, starting: true, installing: false, error: null });
    await launch(set, my, cwd, name, exePath);
  },

  installAndOpen: async (cwd, name, force) => {
    const st = get();
    if (st.starting || st.installing) return;
    const my = ++seq;
    // 更新场景旧服务还在跑：Windows 会锁定运行中的 exe，覆盖安装前必须先停
    if (st.session?.port !== null) {
      await opencodeApi.stop().catch(() => {});
    }
    if (my !== seq) return;
    set({
      installing: true,
      starting: false,
      visible: true,
      error: null,
      session: st.session?.cwd === cwd && st.session.port === null
        ? st.session
        : { cwd, name, port: null, managed: false },
    });
    try {
      const { path } = await opencodeApi.downloadLatest(force);
      if (my !== seq) return;
      set({ installing: false, starting: true });
      await launch(set, my, cwd, name, path);
    } catch (e) {
      if (my !== seq) return;
      set({ installing: false, error: { message: String(e), kind: 'install' } });
    }
  },

  hide: () => set({ visible: false }),

  close: async () => {
    seq++;
    set({ visible: false, starting: false, installing: false, error: null });
    try {
      await opencodeApi.stop();
    } catch (e) {
      console.error('停止 opencode 失败:', e);
    }
    set({ session: null });
  },
}));
