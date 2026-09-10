import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { projectApi, processApi, serviceApi, watchApi, type ProjectDetail as PD, type Service } from '../services/service';
import { useLogStore } from '../stores/logStore';
import { useEditorStore } from '../stores/editor';
import { useRunningStore } from '../stores/runningStore';
import { useSvcCacheStore } from '../stores/svcCacheStore';
import { useToolStore } from '../stores/toolStore';
import { showNotification } from '../components/ui/Toast';

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
  const [showAddServiceModal, setShowAddServiceModal] = useState(false);
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
  const fileContent = useEditorStore(s => s.fileContent);
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
      useToolStore.getState().loadProject(projectId).catch((e) => console.error('加载项目工具绑定失败:', e));
      // 同步左侧展开列表缓存：右侧增删改后左侧保持展开的项目即时一致
      useSvcCacheStore.getState().setCache(projectId, d.services);
      setDetail(d);
      useRunningStore.getState().setRunning(r.running, r.failed);
      runningLoadedRef.current = true;
    } catch (e) {
      if (seq !== loadSeqRef.current) return;
      console.error('加载项目详情失败:', e);
      showNotification({ variant: 'error', title: '加载项目详情失败' });
    }
  }, [projectId]);

  useEffect(() => {
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

  // 日志面板生命周期：查看的服务既不在运行、也不在失败列表 = 已被主动停止
  // （项目列表停止 / 单服务停止 / 全部停止）→ 关闭面板 + 清空日志。
  // 崩溃服务在 failed 列表中，面板保留（报错是诊断关键）；
  // 服务已从项目配置中删除 → 同样关闭面板并清日志
  useEffect(() => {
    if (!viewingLog) return;
    const svcExists = detail?.services.some(s => s.id === viewingLog) ?? false;
    const isActive = running.some(r => r.service_id === viewingLog)
      || failed.some(f => f.service_id === viewingLog);
    if (!svcExists || !isActive) {
      setViewingLog(null);
      useLogStore.getState().clearLogs(viewingLog);
    }
  }, [viewingLog, running, failed, detail]);

  // 日志保留策略：日志仅在「服务已从项目配置中删除」时由 prune 清理；
  // running 首次加载成功后才清理，避免挂载瞬间（running 尚为 []）误删日志
  useEffect(() => {
    if (!runningLoadedRef.current) return;
    const activeKeys = new Set([
      ...running.map(r => r.service_id),
      ...(detail?.services ?? []).map(s => s.id),
    ]);
    useLogStore.getState().pruneInactive(activeKeys);
  }, [running, detail]);

  // ── 服务操作 ──────────────────────────────────────────────

  const isServiceRunning = useCallback((svc: Service) => {
    if (!detail) return false;
    return running.some(r => r.service_id === svc.id);
  }, [detail, running]);

  /** 拖拽排序后本地立即重排 services（不等后端重拉，回弹动画结束后 UI 无缝衔接） */
  const reorderServicesLocal = useCallback((orderedIds: string[]) => {
    setDetail(prev => {
      if (!prev) return prev;
      const byId = new Map(prev.services.map(s => [s.id, s]));
      const next = orderedIds.map(id => byId.get(id)).filter((s): s is Service => !!s);
      return { ...prev, services: next };
    });
  }, []);

  /** 服务是否意外失败（崩溃/秒退/spawn 失败）：卡片显示"失败"按钮，日志保留可查看 */
  const isServiceFailed = useCallback((svc: Service) => {
    return failed.some(f => f.service_id === svc.id);
  }, [failed]);

  const handleStartAll = useCallback(async () => {
    if (!detail) return;
    setLoading(p => ({ ...p, __all__: true }));
    try {
      const errors = await processApi.startProject(detail.project.id);
      if (errors.length > 0) {
        showNotification({ variant: 'error', title: '部分服务启动失败', description: errors.join(', ') });
      }
      // 启动项目级文件监听（所有 restart_mode>0 的服务）
      watchApi.start(detail.project.id).catch((e) => console.error('启动文件监听失败:', e));
      await load();
    } catch (e: unknown) {
      showNotification({ variant: 'error', title: '启动服务失败', description: String(e) });
    }
    setLoading(p => ({ ...p, __all__: false }));
  }, [detail, load]);

  const handleStopAll = useCallback(async () => {
    if (!detail) return;
    try {
      // 后端 stop_project_services 同时停止所有进程和项目级文件监听（总开关）
      await processApi.stopProject(detail.project.id);
      // 全部停止 = 主动关闭：清空本项目所有服务日志（含失败服务的日志）
      for (const s of detail.services) {
        useLogStore.getState().clearLogs(s.id);
      }
      await load();
    } catch (e: unknown) {
      showNotification({ variant: 'error', title: '停止服务失败', description: String(e) });
    }
  }, [detail, load]);

  const handleDeleteService = useCallback(async () => {
    if (!deleteSvcTarget) return;
    setDeleting(true);
    try {
      await serviceApi.delete(deleteSvcTarget.id);
      await load();
      if (editingService?.id === deleteSvcTarget.id) setEditingService(null);
      showNotification({ variant: 'warning', title: `已删除服务「${deleteSvcTarget.name}」` });
    } catch (e: unknown) {
      showNotification({ variant: 'error', title: String(e) });
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
    running,
    loading,
    editingService,
    setEditingService,
    showAddServiceModal,
    setShowAddServiceModal,
    deleteSvcTarget,
    setDeleteSvcTarget,
    deleting,
    viewingLog,
    setViewingLog,
    activeTab,
    fileContent,
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
