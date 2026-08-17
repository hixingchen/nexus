import { useState, useEffect, useCallback, useMemo } from 'react';
import { projectApi, processApi, type Project, type Service } from '../services/service';
import { useRunningStore } from '../stores/runningStore';
import { showNotification } from '../components/ui/Toast';

/**
 * ProjectList 组件的业务逻辑 hook
 * 管理项目列表、运行状态、搜索、CRUD 操作等
 */
export function useProjectList() {
  const [projects, setProjects] = useState<Project[]>([]);
  const [search, setSearch] = useState('');
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [expandedSvc, setExpandedSvc] = useState<Set<string>>(new Set());
  const [svcCache, setSvcCache] = useState<Record<string, Service[]>>({});
  // 运行状态来自全局共享 store（MainLayout 统一 3 秒轮询）
  const running = useRunningStore(s => s.running);
  const [actingId, setActingId] = useState<string | null>(null);

  // modal state
  const [showNewModal, setShowNewModal] = useState(false);
  const [ctxMenu, setCtxMenu] = useState<{ id: string; name: string; path: string; x: number; y: number } | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<{ id: string; name: string } | null>(null);
  const [editTarget, setEditTarget] = useState<Project | null>(null);
  const [duplicateTarget, setDuplicateTarget] = useState<{ id: string; name: string } | null>(null);

  // ── 数据加载 ──────────────────────────────────────────────

  const load = useCallback(async () => {
    try {
      setProjects(await projectApi.getAll());
    } catch (e) {
      console.error('加载项目列表失败:', e);
      showNotification({ variant: 'error', title: '加载项目列表失败' });
    }
  }, []);

  useEffect(() => { load(); }, [load]);

  // 右键菜单关闭逻辑在 ProjectList 组件内处理（mousedown + 菜单内部不关闭）

  // ── 派生状态 ──────────────────────────────────────────────

  const filtered = useMemo(() => {
    if (!search.trim()) return projects;
    const q = search.toLowerCase();
    return projects.filter(p =>
      p.name.toLowerCase().includes(q) || p.path.toLowerCase().includes(q)
    );
  }, [projects, search]);

  const isProjectRunning = useCallback((id: string) =>
    running.some(r => r.project_id === id),
    [running]
  );

  // ── 项目操作 ──────────────────────────────────────────────

  const handleTogglePin = useCallback(async (e: React.MouseEvent, projectId: string) => {
    e.stopPropagation();
    try {
      await projectApi.togglePin(projectId);
      await load();
    } catch (e: unknown) {
      showNotification({ variant: 'error', title: String(e) });
    }
  }, [load]);

  const handleStart = useCallback(async (e: React.MouseEvent, id: string, name: string) => {
    e.stopPropagation();
    setActingId(id);
    try {
      await processApi.startProject(id);
      await useRunningStore.getState().refresh();
      showNotification({ title: `「${name}」已启动`, description: '所有已启用的服务已启动' });
    } catch (err: unknown) {
      showNotification({ variant: 'error', title: String(err) });
    }
    setActingId(null);
  }, []);

  const handleStop = useCallback(async (e: React.MouseEvent, id: string, name: string) => {
    e.stopPropagation();
    setActingId(id);
    try {
      await processApi.stopProject(id);
      await useRunningStore.getState().refresh();
      showNotification({ variant: 'info', title: `「${name}」已停止`, description: '所有服务已停止' });
    } catch (err: unknown) {
      showNotification({ variant: 'error', title: String(err) });
    }
    setActingId(null);
  }, []);

  // ── 展开/折叠 ──────────────────────────────────────────────

  const toggleSvcExpand = useCallback((e: React.MouseEvent, projectId: string, serviceId: string) => {
    e.stopPropagation();
    const next = new Set(expandedSvc);
    const key = `${projectId}:${serviceId}`;
    if (next.has(key)) next.delete(key);
    else next.add(key);
    setExpandedSvc(next);
  }, [expandedSvc]);

  const toggleExpand = useCallback(async (e: React.MouseEvent, projectId: string) => {
    e.stopPropagation();
    const next = new Set(expanded);
    if (next.has(projectId)) {
      next.delete(projectId);
      // 折叠时失效缓存：避免详情面板编辑/删除服务后左侧仍显示过期数据
      setSvcCache(prev => {
        const { [projectId]: _removed, ...rest } = prev;
        return rest;
      });
    } else {
      next.clear();
      next.add(projectId);
      try {
        const detail = await projectApi.getDetail(projectId);
        setSvcCache(prev => ({ ...prev, [projectId]: detail.services }));
      } catch (e) {
        console.error('加载服务列表失败:', e);
        showNotification({ variant: 'error', title: '加载服务列表失败' });
      }
    }
    setExpanded(next);
  }, [expanded]);

  return {
    projects, search, setSearch,
    expanded, expandedSvc, svcCache, setSvcCache,
    running, actingId,
    showNewModal, setShowNewModal,
    ctxMenu, setCtxMenu,
    deleteTarget, setDeleteTarget,
    editTarget, setEditTarget,
    duplicateTarget, setDuplicateTarget,
    filtered,
    isProjectRunning,
    load,
    handleTogglePin,
    handleStart, handleStop,
    toggleSvcExpand, toggleExpand,
  };
}
