import { create } from 'zustand';
import { layoutApi } from '../services/service';
import { reportError } from '../utils/error';

/**
 * 布局持久化的**唯一出口**（ARCH-6）。
 *
 * 背景：此前三处各自 `layoutApi.load()`（MainLayout / ProjectDetail / aiStore），写侧有三套
 * 策略（两处 500ms 防抖 + aiStore 立即写），键名全是裸字符串——加一个布局项要记得在
 * 3 个文件里各写一遍，改名则没有任何编译期保护（拼错就是"设置不生效且无人报错"）。
 *
 * 现在：键集中在 `LAYOUT_KEYS`，读走 `useLayoutStore.getState().ensureLoaded()`（进程内只查库
 * 一次并缓存），写走 `saveLayout()`（统一防抖 + 合并同窗口内的多次修改 + 统一错误上报）。
 */

/** 全部布局键（改名/新增只在这里发生；`as const` 让拼写错误变成类型错误） */
export const LAYOUT_KEYS = {
  /** 当前选中的项目 id */
  selectedProjectId: 'selected_project_id',
  /** 左侧项目列宽度（px） */
  leftPanelWidth: 'left_panel_width',
  /** 左侧项目列是否收起（'1' / '0'） */
  leftPanelCollapsed: 'left_panel_collapsed',
  /** 服务列是否收起（'1' / '0'） */
  servicePanelCollapsed: 'service_panel_collapsed',
  /** 右侧上半区（项目服务）高度（px） */
  rightPanelTopHeight: 'right_panel_top_height',
  /** AI 面板宽度（px） */
  aiPanelWidth: 'ai_panel_width',
} as const;

export type LayoutKey = (typeof LAYOUT_KEYS)[keyof typeof LAYOUT_KEYS];

/** 防抖窗口：拖拽/折叠会连续触发，500ms 内的多次修改合并为一次写库 */
const SAVE_DEBOUNCE_MS = 500;

interface LayoutStore {
  /** 已从库读到的布局键值（未加载时为 null） */
  values: Record<string, string> | null;
  /** 读取在途的 Promise（并发调用共享同一次查库） */
  loading: Promise<Record<string, string>> | null;
  ensureLoaded: () => Promise<Record<string, string>>;
}

/** 待写入的键值（防抖窗口内累积合并） */
let pending: Record<string, string> = {};
let flushTimer: ReturnType<typeof setTimeout> | null = null;

function flush(): void {
  const patch = pending;
  pending = {};
  flushTimer = null;
  if (Object.keys(patch).length === 0) return;
  layoutApi.save(patch).catch((e) => reportError('保存布局失败', e, { silent: true }));
}

/** 立即把待写内容落库（退出前/需要确定性时用） */
export function flushLayoutWrites(): void {
  if (flushTimer !== null) {
    clearTimeout(flushTimer);
    flush();
  }
}

/**
 * 写入布局键值（统一入口）。
 *
 * 防抖 + 合并：同一窗口内的多次修改只发一次 IPC，且合并成单个 patch
 * （原实现每处各自 clearTimeout 自己那份定时器，三处互不知情，先后触发会写三次）。
 */
export function saveLayout(patch: Partial<Record<LayoutKey, string>>): void {
  Object.assign(pending, patch);
  if (flushTimer !== null) clearTimeout(flushTimer);
  flushTimer = setTimeout(flush, SAVE_DEBOUNCE_MS);
}

export const useLayoutStore = create<LayoutStore>((set, get) => ({
  values: null,
  loading: null,

  ensureLoaded: () => {
    const { values, loading } = get();
    if (values) return Promise.resolve(values);
    if (loading) return loading;
    const p = layoutApi
      .load()
      .then((v) => {
        // 读取期间的写入（用户已经改过布局）不能被库里的旧值覆盖：以 pending 为准合并
        const merged = { ...v, ...pending };
        set({ values: merged, loading: null });
        return merged;
      })
      .catch((e) => {
        // 读失败：当作"没有已保存布局"（用默认值），但不清 pending，避免吞掉刚发生的修改
        reportError('加载布局失败', e, { silent: true });
        set({ values: { ...pending }, loading: null });
        return { ...pending };
      });
    set({ loading: p });
    return p;
  },
}));

/** 便捷读取：确保已加载后取单个键（首次调用会触发一次查库） */
export async function readLayoutValue(key: LayoutKey): Promise<string | undefined> {
  const values = await useLayoutStore.getState().ensureLoaded();
  return values[key];
}
