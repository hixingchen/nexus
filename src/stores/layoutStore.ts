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
  /**
   * 文本字号（px）：代码视图 + Markdown 预览（见 `utils/editorFont.ts`）。
   *
   * 它不是"布局"，但存在这张表里：本表是**唯一的 KV 持久化**（见 `layout` 表），
   * 为它单开一张设置表要带上一整套 schema 迁移与读写命令的面。代价是它必须被
   * `reset_layout` 排除在清除范围之外——见 `commands/layout.rs` 的 `KEEP_ON_RESET`。
   */
  editorFontSize: 'editor_font_size',
  /**
   * AI 面板打开方式：`'new'`（默认，每次打开新建会话）/ `'resume'`（恢复上次使用的会话）。
   *
   * 与字号同理：它不是几何布局，但本表是唯一的 KV 持久化；`reset_layout` 的保留清单
   * （`commands/layout.rs` 的 `KEEP_ON_RESET`）里必须带上它，否则"恢复默认布局"会
   * 顺手改掉用户的 AI 使用偏好。
   */
  aiSessionMode: 'ai_session_mode',
  /**
   * 用户点过「跳过此版本」的版本号（GitHub tag，如 `v1.9.1`）。
   *
   * 只影响**启动时的自动提示**：手动「检查更新」永远显示结果；新版本发布后照常提示。
   * 与字号同理，`reset_layout` 不清它（见 `commands/layout.rs` 的 `KEEP_ON_RESET`）。
   */
  updateSkippedVersion: 'update_skipped_version',
} as const;

export type LayoutKey = (typeof LAYOUT_KEYS)[keyof typeof LAYOUT_KEYS];

/** 防抖窗口：拖拽/折叠会连续触发，500ms 内的多次修改合并为一次写库 */
const SAVE_DEBOUNCE_MS = 500;

interface LayoutStore {
  /** 已从库读到的布局键值（未加载时为 null） */
  values: Record<string, string> | null;
  /** 读取在途的 Promise（并发调用共享同一次查库） */
  loading: Promise<Record<string, string>> | null;
  /** @param force 跳过"已加载"快捷路径，强制重新查库（值全程不清空，见实现处说明） */
  ensureLoaded: (force?: boolean) => Promise<Record<string, string>>;
  /**
   * 「恢复默认布局」代数：每执行一次 +1（0 = 从未重置）。
   *
   * 为什么需要它（而不是清了库就完事）：布局的默认值是**各消费方自己的字面量**
   * （`MainLayout` 的 `useState(260)`、`ProjectDetail` 的 topHeight、`aiStore` 的宽度），
   * 且它们只在挂载时读一次库——删掉库里的行，它们不会自己回到默认值。
   * 消费方把这个数放进 effect 依赖里，重新贴一遍自己的默认值即可。
   *
   * 为什么不做成"清库 + 刷新窗口"：刷新会关掉所有已打开的标签页、并拆掉 AI 会话
   * （见 `AiPanel` 卸载时的 `destroyWv`）——用户点一下"恢复默认布局"不该付出这个代价。
   */
  resetSeq: number;
  /** 重置计数 +1（调用方负责清库与丢弃待写批次，见 `resetLayoutToDefaults`） */
  markReset: () => void;
}

/** 待写入的键值（防抖窗口内累积合并） */
let pending: Record<string, string> = {};
let flushTimer: ReturnType<typeof setTimeout> | null = null;

function flush(): Promise<void> {
  const patch = pending;
  pending = {};
  flushTimer = null;
  if (Object.keys(patch).length === 0) return Promise.resolve();
  // 返回 Promise 而不是 fire-and-forget：退出路径必须在 `app.exit(0)` 之前确定写库完成，
  // 否则这条 IPC 与退出流程竞争，挂起的布局改动会随进程一起消失
  return layoutApi.save(patch).catch((e) => {
    reportError('保存布局失败', e, { silent: true });
  });
}

/**
 * 立即把待写内容落库（退出前用）。
 *
 * 必须 await：`app.exit(0)` 会终止 WebView，挂起的 500ms 防抖定时器永远不会执行。
 * 原实现是 fire-and-forget 且**零调用点**——注释写着"退出前用"，但没有任何退出路径调它，
 * 于是拖完面板宽度后 500ms 内关窗，改动就静默丢失。
 */
export function flushLayoutWrites(): Promise<void> {
  if (flushTimer !== null) {
    clearTimeout(flushTimer);
    return flush();
  }
  return Promise.resolve();
}

/**
 * 写入布局键值（统一入口）。
 *
 * 防抖 + 合并：同一窗口内的多次修改只发一次 IPC，且合并成单个 patch
 * （原实现每处各自 clearTimeout 自己那份定时器，三处互不知情，先后触发会写三次）。
 */
export function saveLayout(patch: Partial<Record<LayoutKey, string>>): void {
  Object.assign(pending, patch);
  /**
   * 同步进内存缓存。
   *
   * 为什么必须（此前不需要）：`values` 原本只由 `ensureLoaded` 填，而各消费方都在挂载时
   * 读一次、之后只把值放进自己的 React state——没有人"订阅 values 再写回 values"，
   * 所以这个缺口一直没暴露。「编辑器字号」是第一个**既要读又要写**的设置项：
   * 只写 pending 而不更新 `values` 的话，拖动滑块时读取方拿到的还是旧值，界面纹丝不动。
   */
  useLayoutStore.setState((s) => ({ values: { ...(s.values ?? {}), ...patch } }));
  if (flushTimer !== null) clearTimeout(flushTimer);
  flushTimer = setTimeout(flush, SAVE_DEBOUNCE_MS);
}

/**
 * 恢复默认布局：落库待写批次 → 清库 → 通知各消费方重贴默认值。
 *
 * **顺序不能反**（这是这个函数唯一容易写错的地方）：
 * - 先删、再让防抖窗口里那一批落地 → 刚删掉的行被原样写回来，界面表现是"点了没反应"；
 * - 直接丢弃那一批（`pending = {}`）→ 会连带丢掉 `selected_project_id`：用户刚点了一个项目
 *   （500ms 内）就点「恢复默认布局」，那次选择从未落库，`reset_layout` 又只保留库里已有的值，
 *   于是下次启动会选中**上一个**项目。
 * 先 flush 再删两个问题都不存在：落库的值随后被删，而要保留的项目选择已经在库里了。
 *
 * **不刷新窗口**：刷新会关掉所有已打开的标签页并拆掉 AI 会话（见 `AiPanel` 卸载时的
 * `destroyWv`），而用户要的只是面板回到初始尺寸。
 *
 * 出错**往上抛**（与 `saveLayout` 的静默策略不同）：这是用户主动点的动作，成败都必须说出来
 * ——静默失败的表现就是"按钮点了没反应"。返回被清掉的行数，好让提示说出清了 N 项。
 */
export async function resetLayoutToDefaults(): Promise<number> {
  await flushLayoutWrites();
  const removed = await layoutApi.reset();
  /**
   * 缓存作废之后**立刻重查**，而不是只置 null 就完事。
   *
   * 库里并不是空的：`reset_layout` 保留了当前选中的项目与编辑器字号（见 `commands/layout.rs`
   * 的 `KEEP_ON_RESET`）。只置 null 的话，这些被保留的键在内存里会一直缺到下次启动——
   * 表现是"恢复默认布局之后编辑器字号莫名其妙变小了"，而数据库里其实还好好存着。
   *
   * 重查走 `ensureLoaded(true)`（`force`）而**不是"置 null 再 ensureLoaded"**：置 null 到
   * 新值回来的这几毫秒里，订阅方读到的是"没有布局"——字号回落到默认 14px，
   * 于是 Markdown 预览与日志面板的字跳一下、CodeMirror 白重测量一次（`values` 全程有效
   * 就不会有这一帧）。`force` 只跳过"已加载就不查"的快捷路径，其余合并逻辑完全一致。
   */
  await useLayoutStore.getState().ensureLoaded(true);
  useLayoutStore.getState().markReset();
  return removed;
}

export const useLayoutStore = create<LayoutStore>((set, get) => ({
  values: null,
  loading: null,
  resetSeq: 0,

  markReset: () => set((s) => ({ resetSeq: s.resetSeq + 1 })),

  /**
   * 读一次布局库（进程内只查一次，之后返回缓存）。
   *
   * `force = true` 用于「恢复默认布局」：库里的行刚被删过，内存这份必须重新查——
   * 但它**不清 `values`**，新值一次落地，中途订阅方读到的始终是有效值（原因见
   * `resetLayoutToDefaults`）。
   */
  ensureLoaded: (force = false) => {
    const { values, loading } = get();
    if (values && !force) return Promise.resolve(values);
    if (loading && !force) return loading;
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

