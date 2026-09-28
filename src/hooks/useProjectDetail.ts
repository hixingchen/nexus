import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { projectApi, serviceApi, type FailedService, type ProjectDetailView, type Service } from '../services/service';
import { useLogStore } from '../stores/logStore';
import { useEditorStore } from '../stores/editor';
import { useRunningStore } from '../stores/runningStore';
import { useSvcCacheStore } from '../stores/svcCacheStore';
import { toMessage } from '../utils/message';
import { useToolStore } from '../stores/toolStore';
import { startProjectServices, stopProjectServices } from '../stores/serviceActions';
import { showNotification } from '../components/ui/Toast';
import { reportError } from '../utils/error';
import { pruneServiceDrafts } from '../stores/serviceDraftStore';
import { selectProjectDetail, servicesPending, type DetailSlot } from '../utils/projectDetail';

/** 空列表常量：缓存未命中时复用它，避免每次渲染产生新数组（会让订阅方无谓重渲染） */
const EMPTY_SERVICES: Service[] = [];

/**
 * 已加载过的项目详情，按项目 id 归档（UX-18）。
 *
 * 为什么需要：切项目时 `detail` 必须立刻失效——load 在途（或失败）期间留着上一份，
 * "启动全部/停止全部"取的是 `detail.project.id`，会作用到上一个项目。但"失效"到
 * "新数据回来"之间，界面只能拿 null 顶着，于是整个右侧面板（服务列 + 模板库 + 主区）
 * 跟着卸载又挂载；实测切一次项目会连闪三帧：**上一个项目的服务 →「暂无服务」→
 * 「加载中…」→ 新项目的服务**，本地 SQLite 再快也躲不掉（前两帧与后端耗时无关）。
 *
 * 缓存的作用只是"首帧就有正确内容"：按 id 取，绝不混项目；命中后 `load()` 照常执行，
 * 数据仍以库里的为准（它是显示来源，不是事实来源）。
 */
const detailCache = new Map<string, ProjectDetailView>();

/**
 * ProjectDetail 组件的业务逻辑 hook
 * 管理项目详情、运行状态、服务 CRUD、日志查看等状态
 */
export function useProjectDetail(projectId: string) {
  /**
   * 详情与**它所属的项目 id** 一起存：读的时候不同源一律当作"还没有"。
   *
   * 为什么不直接 `useState<PD>`：那样在"项目 id 已切、效果还没跑"的那一帧里，
   * `detail` 仍是上一个项目的对象，界面会把它的名称与服务当成新项目的渲染出去
   * （实测就是切项目时的第一帧"上一个项目的服务"）。绑定 id 后这一帧自然落到
   * 加载态，不需要靠 effect 里补一次 `setDetail(null)` 去追。
   */
  const [detailState, setDetailState] = useState<DetailSlot<ProjectDetailView> | null>(null);
  /** 当前项目的详情：同源 state → 缓存那一份 → null。规则与用例见 utils/projectDetail */
  const detail = selectProjectDetail(detailState, projectId, detailCache);
  // 运行状态来自全局共享 store（MainLayout 统一 3 秒轮询）
  const running = useRunningStore(s => s.running);
  /** 意外退出的服务（崩溃/秒退/spawn 失败） */
  const failed = useRunningStore(s => s.failed);
  const [loading, setLoading] = useState<Record<string, boolean>>({});
  const [editingService, setEditingService] = useState<Service | null>(null);
  const [deleteSvcTarget, setDeleteSvcTarget] = useState<{ id: string; name: string } | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [viewingLog, setViewingLog] = useState<string | null>(null);
  /**
   * 详情加载失败的原因（null = 没有失败）。
   *
   * 为什么要单独一个状态（UX-17）：失败时 `detail` 同样是 null，而界面把 null 一律当成
   * "加载中…"——于是加载失败后主区域**永远停在"加载中"**，真正的原因只在一条 8 秒后
   * 消失的 toast 里出现过一次。原因是留了，但没留在持久可见的地方。
   */
  const [loadError, setLoadError] = useState<string | null>(null);

  // 请求序号：项目切换时旧请求的响应被丢弃，避免错项目数据
  const loadSeqRef = useRef(0);
  // running 首次加载成功后才允许 pruneInactive，避免挂载瞬间误清日志
  const runningLoadedRef = useRef(false);

  // 编辑器状态
  const editorTabs = useEditorStore(s => s.tabs);
  const activeTabId = useEditorStore(s => s.activeTabId);
  // 注意：这里**不**订阅 fileContent——它是每次按键都变的编辑回声，订阅它会让整个
  // ProjectDetail 子树跟着重渲染。CodeViewer 自己订阅（见 CodeViewer 内的说明）。
  const activeTab = useMemo(() => editorTabs.find(t => t.id === activeTabId), [editorTabs, activeTabId]);

  // ── 数据加载 ──────────────────────────────────────────────

  const load = useCallback(async () => {
    const seq = ++loadSeqRef.current;
    try {
      const d = await projectApi.getDetail(projectId);
      if (seq !== loadSeqRef.current) return; // 已被更新的请求取代，丢弃过期响应
      // 并行刷新打开工具绑定（服务增删后右键"用 XX 打开"显示才准）
      useToolStore.getState().loadProject(projectId).catch((e) => reportError('加载项目工具绑定失败', e));
      // 同步左侧展开列表缓存：右侧增删改后左侧保持展开的项目即时一致
      useSvcCacheStore.getState().setCache(projectId, d.services);
      // 服务已被删除时其编辑草稿没有意义（还会让关窗确认多算一项）；模板草稿由模板库加载时清理
      pruneServiceDrafts('service', d.services.map(s => s.id));
      // 交给组件的只有 project：services 已写进共享缓存，再留一份就是第二份真相（ARCH-29）
      const view: ProjectDetailView = { project: d.project };
      setDetailState({ projectId, detail: view });
      detailCache.set(projectId, view); // 供下次切回本项目时首帧直接命中（见 detailCache 说明）
      setLoadError(null);
      // 运行状态**不在这里单独拉**（ARCH-28）：`runningStore` 自己每 3 秒轮询，而
      // `get_running` 是全项目最重的命令（逐服务 try_wait + 一次全系统进程快照 +
      // 未认领服务各扫 7 个日志目录）——「启动全部」原先要付两遍。
      // 这里只让它刷新，数据从订阅进来；与 `setRunning` 一样走同一套代际判定。
      void useRunningStore.getState().refresh();
      // "运行态已就绪"只在 store 真有数据时才置位：否则 `pruneInactive` 会拿一份空表
      // 判"哪些服务不在运行"，把正在跑的服务的日志缓冲清掉（这就是这个 ref 存在的理由）
      if (useRunningStore.getState().loaded) runningLoadedRef.current = true;
    } catch (e) {
      if (seq !== loadSeqRef.current) return;
      // 失败也要保持"无详情"：否则 load 失败后界面继续显示上一个项目并可对其操作。
      // 缓存里那一份也一并丢弃——读失败说明这份数据不可信，留着它就会把 UX-17 的
      // 失败态（持久的失败原因 + 重试）盖成"看起来一切正常"
      setDetailState(null);
      detailCache.delete(projectId);
      // 同时记下原因：主区域据此显示**持久的**失败态与「重试」，而不是永远"加载中…"（UX-17）
      setLoadError(toMessage(e));
      reportError('加载项目详情失败', e);
    }
  }, [projectId]);

  useEffect(() => {
    // 切项目：把 state 收敛到**本项目**（命中缓存则首帧就是完整界面，未命中则是加载态；
    // 两种情况下都不会把上一个项目的详情留在界面上——同源判定在读取处，见 detailState）。
    // 上一个项目的加载失败态也要一并清掉，否则它会盖在新项目头上。
    setLoadError(null);
    setDetailState(prev => (prev?.projectId === projectId ? prev : null));
    load();
    setEditingService(null);
    setViewingLog(null);
  }, [load, projectId]);

  // 运行状态由全局 runningStore 轮询；首次加载成功后置位 runningLoadedRef
  const runningLoaded = useRunningStore(s => s.loaded);
  useEffect(() => {
    if (runningLoaded) runningLoadedRef.current = true;
  }, [runningLoaded]);

  // ── 日志管理 ──────────────────────────────────────────────

  // 用户请求打开文件时自动关闭日志面板（日志与文件区互斥，文件优先）。
  //
  // 信号取 `openRequestSeq` 而不是活动标签 id：后者只在**标签真的换了**的时候才变，
  // 而点文件树里**当前已经打开的那个文件**不会换标签（`setActiveTabId` 设的是同一个值），
  // 于是那次点击对日志毫无影响——表现为"要切到别的文件，日志才收"。
  // `loadAndOpenFile` 是所有打开路径的唯一入口（文件树 / 搜索结果 / Markdown 内链），
  // 它在入口处递增这个信号，每次请求都算。
  //
  // **依赖数组里只能放这个信号**：效果里不读 `viewingLog`，只是无条件把它按回 null
  // （本来就是 null 时 React 会跳过重渲染）。若把 `viewingLog` 也列进去，点「日志」按钮
  // 引起的状态变化会重新触发本效果、把刚打开的日志面板立刻关掉——那正是原实现要拿
  // `lastTabIdRef` 挡住的事，这里改从依赖上根除，不再需要那个 ref。
  const openRequestSeq = useEditorStore(s => s.openRequestSeq);
  useEffect(() => {
    setViewingLog(null);
  }, [openRequestSeq]);

  // ── 服务列表（单一来源：svcCacheStore） ─────────────────────
  //
  // 详情页 load 拿到 payload 后写入缓存（见 load 内的 setCache），之后**只读缓存**：
  // 左侧项目列表展开态与右侧详情页因此看到同一份数据，不存在"两处各有一份、谁先更新谁对"
  // 的问题（ARCH-7 ③）。retain/release 让"详情页开着"这件事对缓存的清理可见——
  // 左侧折叠项目行会请求删除缓存，此时只推迟到详情页卸载后再删。
  const cachedServices = useSvcCacheStore(s => s.cache[projectId] ?? EMPTY_SERVICES);
  /**
   * 服务列表是否"还没到"（UX-18）。
   *
   * 判据刻意不是"列表为空"：本项目详情尚未落地 **且** 共享缓存里也没有服务，才是
   * "不知道有没有服务"。用空列表直接当结论会说出「暂无服务」这句没有根据的话——
   * 切项目那一帧正是如此（旧项目的服务已被排除，新项目的数据还没到）。
   *
   * 缓存里那份详情命中时（detail 非 null）不算"还没到"：它的 services 与即将 load
   * 回来的是同一份；但**服务列表读的是共享缓存**，若那份被左侧折叠清掉了，这里仍要
   * 如实显示加载态。
   */
  const servicesLoading = servicesPending(detailState, projectId, cachedServices);
  useEffect(() => {
    const store = useSvcCacheStore.getState();
    store.retain(projectId);
    return () => store.release(projectId);
  }, [projectId]);

  // 日志面板生命周期：查看的服务既不在运行、也不在失败列表 = 已被主动停止
  // （项目列表停止 / 单服务停止 / 全部停止）→ 关闭面板 + 清空日志。
  // 崩溃服务在 failed 列表中，面板保留（报错是诊断关键）；
  // 服务已从项目配置中删除 → 同样关闭面板并清日志
  useEffect(() => {
    if (!viewingLog) return;
    const svcExists = cachedServices.some(s => s.id === viewingLog);
    const isActive = running.some(r => r.service_id === viewingLog)
      || failed.some(f => f.service_id === viewingLog);
    if (!svcExists || !isActive) {
      setViewingLog(null);
      useLogStore.getState().clearLogs(viewingLog);
    }
  }, [viewingLog, running, failed, cachedServices]);

  // 日志保留策略：日志仅在「服务已从项目配置中删除」时由 prune 清理；
  // running 首次加载成功后才清理，避免挂载瞬间（running 尚为 []）误删日志
  useEffect(() => {
    if (!runningLoadedRef.current) return;
    const activeKeys = new Set([
      ...running.map(r => r.service_id),
      ...cachedServices.map(s => s.id),
    ]);
    useLogStore.getState().pruneInactive(activeKeys);
  }, [running, cachedServices]);

  // ── 服务操作 ──────────────────────────────────────────────

  const isServiceRunning = useCallback((svc: Service) => {
    if (!detail) return false;
    return running.some(r => r.service_id === svc.id);
  }, [detail, running]);

  /**
   * 拖拽排序后本地立即重排 services（不等后端重拉，回弹动画结束后 UI 无缝衔接）。
   *
   * 只写缓存这一处（单一来源，见上方 cachedServices 的说明）：详情页与左侧列表读的是
   * 同一份数据，不会再出现"拖完顺序、切走再切回在左侧看到旧顺序"的错位。
   */
  const reorderServicesLocal = useCallback((orderedIds: string[]) => {
    const prev = useSvcCacheStore.getState().cache[projectId] ?? [];
    const byId = new Map(prev.map(s => [s.id, s]));
    const next = orderedIds.map(id => byId.get(id)).filter((s): s is Service => !!s);
    useSvcCacheStore.getState().setCache(projectId, next);
  }, [projectId]);

  /**
   * 服务的失败信息（null = 没失败；崩溃/秒退/spawn 失败都会在这里）。
   *
   * 返回对象而不是布尔：卡片徽章要写"已失败 · 退出码 1"、日志面板头部要写同一句，
   * 而退出码与发生时刻本来就随这条记录一起到了前端——压成 boolean 会让每个展示位
   * 都只剩"红点"，用户必须点开日志才知道是什么失败（见 utils/serviceFailure 的说明）。
   */
  const failedInfoOf = useCallback((svc: Service): FailedService | null => {
    return failed.find(f => f.service_id === svc.id) ?? null;
  }, [failed]);

  /**
   * 该服务正在跟随的日志文件路径（null = 没在跟随）。
   *
   * 数据随运行状态那条 3 秒轮询一起回来（后端 `get_running` 里带上），
   * 右键菜单据此显示「取消跟随日志文件」并标出跟的是哪个文件。
   */
  const followedLogOf = useCallback((svc: Service) => {
    return running.find(r => r.service_id === svc.id)?.followed_log ?? null;
  }, [running]);

  const handleStartAll = useCallback(async () => {
    if (!detail) return;
    setLoading(p => ({ ...p, __all__: true }));
    // 共享动作层（与项目列表的「启动」是同一实现，见 stores/serviceActions.ts）
    try {
      const errors = await startProjectServices(detail.project.id);
      if (errors.length > 0) {
        showNotification({ variant: 'error', title: '部分服务启动失败', description: errors.join(', ') });
      }
      await load();
    } catch (e: unknown) {
      reportError('启动服务失败', e);
    }
    setLoading(p => ({ ...p, __all__: false }));
  }, [detail, load]);

  const handleStopAll = useCallback(async () => {
    if (!detail) return;
    // 与「全部启动」对称（UX-15）：停止是**串行收尸**（每个服务最长等数秒），
    // 期间按钮外观此前毫无变化，用户会以为没点上而连点——连点就是一次又一次串行停止。
    // 同一份 `loading.__all__` 既禁用按钮也用来显示"停止中…"。
    setLoading(p => ({ ...p, __all__: true }));
    try {
      // 停止逻辑（含项目级监听与日志清理）统一在动作层；服务清单取自单一来源（缓存）
      const errors = await stopProjectServices(detail.project.id, cachedServices);
      if (errors.length > 0) {
        showNotification({ variant: 'error', title: '部分服务停止失败', description: errors.join(', ') });
      }
      await load();
    } catch (e: unknown) {
      reportError('停止服务失败', e);
    }
    setLoading(p => ({ ...p, __all__: false }));
  }, [detail, cachedServices, load]);

  const handleDeleteService = useCallback(async () => {
    if (!deleteSvcTarget) return;
    setDeleting(true);
    try {
      await serviceApi.delete(deleteSvcTarget.id);
      await load();
      if (editingService?.id === deleteSvcTarget.id) setEditingService(null);
      showNotification({ variant: 'warning', title: `已删除服务「${deleteSvcTarget.name}」` });
    } catch (e: unknown) {
      reportError(`删除服务「${deleteSvcTarget.name}」失败`, e);
    }
    setDeleting(false);
    setDeleteSvcTarget(null);
  }, [deleteSvcTarget, editingService, load]);

  const handleViewLog = useCallback((service: Service) => {
    if (!detail) return;
    setViewingLog(prev => prev === service.id ? null : service.id);
  }, [detail]);

  return {
    detail,
    /** 加载失败的原因（非 null 时界面显示失败态而不是"加载中…"，见 loadError 的说明） */
    loadError,
    /** 服务列表（单一来源：svcCacheStore）——组件一律用这个，不要再从 detail 里取 */
    services: cachedServices,
    /** 服务列表还没到：空列表要显示加载态，不能显示成「暂无服务」（UX-18） */
    servicesLoading,
    running,
    loading,
    editingService,
    setEditingService,
    deleteSvcTarget,
    setDeleteSvcTarget,
    deleting,
    viewingLog,
    setViewingLog,
    activeTab,
    load,
    reorderServicesLocal,
    isServiceRunning,
    failedInfoOf,
    followedLogOf,
    handleStartAll,
    handleStopAll,
    handleDeleteService,
    handleViewLog,
  };
}
