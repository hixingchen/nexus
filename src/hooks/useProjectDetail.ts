import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { projectApi, processApi, serviceApi, type ProjectDetail as PD, type Service } from '../services/service';
import { useLogStore } from '../stores/logStore';
import { useEditorStore } from '../stores/editor';
import { useRunningStore } from '../stores/runningStore';
import { useSvcCacheStore } from '../stores/svcCacheStore';
import { useToolStore } from '../stores/toolStore';
import { startProjectServices, stopProjectServices } from '../stores/serviceActions';
import { showNotification } from '../components/ui/Toast';
import { reportError } from '../utils/error';
import { pruneServiceDrafts } from '../stores/serviceDraftStore';

/** 空列表常量：缓存未命中时复用它，避免每次渲染产生新数组（会让订阅方无谓重渲染） */
const EMPTY_SERVICES: Service[] = [];

/**
 * ProjectDetail 组件的业务逻辑 hook
 * 管理项目详情、运行状态、服务 CRUD、日志查看等状态
 */
export function useProjectDetail(projectId: string) {
  const [detail, setDetail] = useState<PD | null>(null);
  // 运行状态来自全局共享 store（MainLayout 统一 3 秒轮询）
  const running = useRunningStore(s => s.running);
  /** 意外退出的服务（崩溃/秒退/spawn 失败） */
  const failed = useRunningStore(s => s.failed);
  const [loading, setLoading] = useState<Record<string, boolean>>({});
  const [editingService, setEditingService] = useState<Service | null>(null);
  const [deleteSvcTarget, setDeleteSvcTarget] = useState<{ id: string; name: string } | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [viewingLog, setViewingLog] = useState<string | null>(null);

  // 请求序号：项目切换时旧请求的响应被丢弃，避免错项目数据
  const loadSeqRef = useRef(0);
  /** 最近一次活动标签 id（打开/切换文件时若在日志面板则自动关闭——文件内容优先） */
  const lastTabIdRef = useRef<string | null>(null);
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
      const [d, r] = await Promise.all([
        projectApi.getDetail(projectId),
        processApi.getRunning(),
      ]);
      if (seq !== loadSeqRef.current) return; // 已被更新的请求取代，丢弃过期响应
      // 并行刷新打开工具绑定（服务增删后右键"用 XX 打开"显示才准）
      useToolStore.getState().loadProject(projectId).catch((e) => reportError('加载项目工具绑定失败', e));
      // 同步左侧展开列表缓存：右侧增删改后左侧保持展开的项目即时一致
      useSvcCacheStore.getState().setCache(projectId, d.services);
      // 服务已被删除时其编辑草稿没有意义（还会让关窗确认多算一项）；模板草稿由模板库加载时清理
      pruneServiceDrafts('service', d.services.map(s => s.id));
      setDetail(d);
      useRunningStore.getState().setRunning(r.running, r.failed);
      runningLoadedRef.current = true;
    } catch (e) {
      if (seq !== loadSeqRef.current) return;
      // 失败也要保持"无详情"：否则 load 失败后界面继续显示上一个项目并可对其操作
      setDetail(null);
      reportError('加载项目详情失败', e);
    }
  }, [projectId]);

  useEffect(() => {
    // 先清空上一份 detail：load 在途（或失败）期间界面上仍是上一个项目的数据，
    // 而"启动全部/停止全部"等操作取的是 detail.project.id —— 会作用到上一个项目
    setDetail(null);
    load();
    setEditingService(null);
    setViewingLog(null);
  }, [load]);

  // 运行状态由全局 runningStore 轮询；首次加载成功后置位 runningLoadedRef
  const runningLoaded = useRunningStore(s => s.loaded);
  useEffect(() => {
    if (runningLoaded) runningLoadedRef.current = true;
  }, [runningLoaded]);

  // ── 日志管理 ──────────────────────────────────────────────

  // 打开/切换文件时自动关闭日志面板（日志与文件区互斥，文件优先）。
  // 仅当活动标签 id 变化才关：点"日志"按钮本身不触发（id 未变），避免日志打不开
  useEffect(() => {
    const id = activeTabId ?? null;
    const changed = id !== lastTabIdRef.current;
    lastTabIdRef.current = id;
    if (changed && viewingLog) {
      setViewingLog(null);
    }
  }, [activeTabId, viewingLog, setViewingLog]);

  // ── 服务列表（单一来源：svcCacheStore） ─────────────────────
  //
  // 详情页 load 拿到 payload 后写入缓存（见 load 内的 setCache），之后**只读缓存**：
  // 左侧项目列表展开态与右侧详情页因此看到同一份数据，不存在"两处各有一份、谁先更新谁对"
  // 的问题（ARCH-7 ③）。retain/release 让"详情页开着"这件事对缓存的清理可见——
  // 左侧折叠项目行会请求删除缓存，此时只推迟到详情页卸载后再删。
  const cachedServices = useSvcCacheStore(s => s.cache[projectId] ?? EMPTY_SERVICES);
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

  /** 服务是否意外失败（崩溃/秒退/spawn 失败）：卡片显示"失败"按钮，日志保留可查看 */
  const isServiceFailed = useCallback((svc: Service) => {
    return failed.some(f => f.service_id === svc.id);
  }, [failed]);

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
    /** 服务列表（单一来源：svcCacheStore）——组件一律用这个，不要再从 detail 里取 */
    services: cachedServices,
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
    isServiceFailed,
    handleStartAll,
    handleStopAll,
    handleDeleteService,
    handleViewLog,
  };
}
