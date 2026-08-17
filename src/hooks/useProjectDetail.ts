import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { projectApi, processApi, serviceApi, watchApi, type ProjectDetail as PD, type Service } from '../services/service';
import { useLogStore } from '../stores/logStore';
import { useEditorStore } from '../stores/editor';
import { useRunningStore } from '../stores/runningStore';
import { showNotification } from '../components/ui/Toast';

/**
 * ProjectDetail 组件的业务逻辑 hook
 * 管理项目详情、运行状态、服务 CRUD、日志查看等状态
 */
export function useProjectDetail(projectId: string) {
  const [detail, setDetail] = useState<PD | null>(null);
  // 运行状态来自全局共享 store（MainLayout 统一 3 秒轮询）
  const running = useRunningStore(s => s.running);
  const [loading, setLoading] = useState<Record<string, boolean>>({});
  const [editingService, setEditingService] = useState<Service | null>(null);
  const [showAddServiceModal, setShowAddServiceModal] = useState(false);
  const [deleteSvcTarget, setDeleteSvcTarget] = useState<{ id: string; name: string } | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [viewingLog, setViewingLog] = useState<string | null>(null);

  // 请求序号：项目切换时旧请求的响应被丢弃，避免错项目数据
  const loadSeqRef = useRef(0);
  const mountedRef = useRef(true);
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
      setDetail(d);
      useRunningStore.getState().setRunning(r);
      runningLoadedRef.current = true;
      if (mountedRef.current) {
        watchApi.start(projectId).catch((e) => console.error('启动文件监听失败:', e));
      }
    } catch (e) {
      if (seq !== loadSeqRef.current) return;
      console.error('加载项目详情失败:', e);
      showNotification({ variant: 'error', title: '加载项目详情失败' });
    }
  }, [projectId]);

  useEffect(() => {
    mountedRef.current = true;
    load();
    setEditingService(null);
    setViewingLog(null);
    return () => { mountedRef.current = false; };
  }, [load]);

  // 组件卸载或项目切换时停止文件监听
  useEffect(() => {
    return () => { watchApi.stop(projectId).catch((e) => console.error('停止文件监听失败:', e)); };
  }, [projectId]);

  // 运行状态由全局 runningStore 轮询；首次加载成功后置位 runningLoadedRef
  const runningLoaded = useRunningStore(s => s.loaded);
  useEffect(() => {
    if (runningLoaded) runningLoadedRef.current = true;
  }, [runningLoaded]);

  // ── 日志管理 ──────────────────────────────────────────────

  useEffect(() => {
    if (viewingLog && !running.some(r => r.service_id === viewingLog)) {
      setViewingLog(null);
      useLogStore.getState().clearLogs(viewingLog);
    }
    // running 首次加载成功后才清理，避免挂载瞬间（running 尚为 []）误删正在运行服务的日志
    if (runningLoadedRef.current) {
      useLogStore.getState().pruneInactive(new Set(running.map(r => r.service_id)));
    }
  }, [running, viewingLog]);

  // ── 服务操作 ──────────────────────────────────────────────

  const isServiceRunning = useCallback((svc: Service) => {
    if (!detail) return false;
    return running.some(r => r.service_id === svc.id);
  }, [detail, running]);

  const handleStartAll = useCallback(async () => {
    if (!detail) return;
    setLoading(p => ({ ...p, __all__: true }));
    try {
      const errors = await processApi.startProject(detail.project.id);
      if (errors.length > 0) {
        showNotification({ variant: 'error', title: '部分服务启动失败', description: errors.join(', ') });
      }
      await load();
    } catch (e: unknown) {
      showNotification({ variant: 'error', title: '启动服务失败', description: String(e) });
    }
    setLoading(p => ({ ...p, __all__: false }));
  }, [detail, load]);

  const handleStopAll = useCallback(async () => {
    if (!detail) return;
    try {
      await processApi.stopProject(detail.project.id);
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
    isServiceRunning,
    handleStartAll,
    handleStopAll,
    handleDeleteService,
    handleViewLog,
  };
}
