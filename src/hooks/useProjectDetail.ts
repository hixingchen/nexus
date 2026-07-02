import { useState, useEffect, useCallback, useMemo } from 'react';
import { projectApi, processApi, serviceApi, watchApi, type ProjectDetail as PD, type Service } from '../services/service';
import { useLogStore } from '../stores/logStore';
import { useEditorStore } from '../stores/editor';
import { showNotification } from '../components/ui/Toast';

/**
 * ProjectDetail 组件的业务逻辑 hook
 * 管理项目详情、运行状态、服务 CRUD、日志查看等状态
 */
export function useProjectDetail(projectId: string, onProjectStart?: () => void) {
  const [detail, setDetail] = useState<PD | null>(null);
  const [running, setRunning] = useState<string[]>([]);
  const [loading, setLoading] = useState<Record<string, boolean>>({});
  const [editingService, setEditingService] = useState<Service | null>(null);
  const [showAddServiceModal, setShowAddServiceModal] = useState(false);
  const [deleteSvcTarget, setDeleteSvcTarget] = useState<{ id: string; name: string } | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [svcCtxMenu, setSvcCtxMenu] = useState<{ id: string; name: string; x: number; y: number } | null>(null);
  const [viewingLog, setViewingLog] = useState<string | null>(null);

  // 编辑器状态
  const editorTabs = useEditorStore(s => s.tabs);
  const activeTabId = useEditorStore(s => s.activeTabId);
  const fileContent = useEditorStore(s => s.fileContent);
  const activeTab = useMemo(() => editorTabs.find(t => t.id === activeTabId), [editorTabs, activeTabId]);

  // ── 数据加载 ──────────────────────────────────────────────

  const load = useCallback(async () => {
    try {
      const [d, r] = await Promise.all([
        projectApi.getDetail(projectId),
        processApi.getRunning(),
      ]);
      setDetail(d);
      setRunning(r);
      watchApi.start(projectId).catch((e) => console.error('启动文件监听失败:', e));
    } catch (e) {
      console.error('加载项目详情失败:', e);
      showNotification({ variant: 'error', title: '加载项目详情失败' });
    }
  }, [projectId]);

  useEffect(() => { load(); setEditingService(null); }, [load]);

  // 组件卸载或项目切换时停止文件监听
  useEffect(() => {
    return () => { watchApi.stop(projectId).catch((e) => console.error('停止文件监听失败:', e)); };
  }, [projectId]);

  // 轮询运行状态
  useEffect(() => {
    const interval = setInterval(async () => {
      try {
        setRunning(await processApi.getRunning());
      } catch (e) {
        console.error('获取运行状态失败:', e);
        showNotification({ variant: 'error', title: '获取运行状态失败' });
      }
    }, 3000);
    return () => clearInterval(interval);
  }, []);

  // ── 日志管理 ──────────────────────────────────────────────

  useEffect(() => {
    if (viewingLog && !running.includes(viewingLog)) {
      setViewingLog(null);
      useLogStore.getState().clearLogs(viewingLog);
    }
    if (running.length > 0) {
      useLogStore.getState().pruneInactive(new Set(running));
    }
  }, [running, viewingLog]);

  // 点击任意处关闭服务右键菜单
  useEffect(() => {
    if (!svcCtxMenu) return;
    const handler = () => setSvcCtxMenu(null);
    document.addEventListener('click', handler);
    return () => document.removeEventListener('click', handler);
  }, [svcCtxMenu]);

  // ── 服务操作 ──────────────────────────────────────────────

  const isServiceRunning = useCallback((svc: Service) => {
    if (!detail) return false;
    return running.includes(`${detail.project.id}:${svc.name}`);
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
      // 服务启动成功后打开终端
      onProjectStart?.();
    } catch (e: unknown) {
      showNotification({ variant: 'error', title: '启动服务失败', description: String(e) });
    }
    setLoading(p => ({ ...p, __all__: false }));
  }, [detail, load, onProjectStart]);

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
    const key = `${detail.project.id}:${service.name}`;
    setViewingLog(prev => prev === key ? null : key);
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
    svcCtxMenu,
    setSvcCtxMenu,
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
