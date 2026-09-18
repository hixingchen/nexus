import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { projectApi, type Project } from '../services/service';
import { useRunningStore } from '../stores/runningStore';
import { useSvcCacheStore } from '../stores/svcCacheStore';
import { startProjectWithFeedback, stopProjectWithFeedback } from '../stores/serviceActions';
import { reportError } from '../utils/error';

/**
 * ProjectList 组件的业务逻辑 hook
 * 管理项目列表、运行状态、搜索、CRUD 操作等
 */
export function useProjectList() {
  const [projects, setProjects] = useState<Project[]>([]);
  const [search, setSearch] = useState('');
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [expandedSvc, setExpandedSvc] = useState<Set<string>>(new Set());
  /** 正在拉服务列表的项目（卡片箭头槽位显示 spinner，给双击一个即时反馈） */
  const [expandingId, setExpandingId] = useState<string | null>(null);
  /** 展开请求序号：连点不同项目时丢弃过期响应，避免先点的项目后到反而生效 */
  const expandSeqRef = useRef(0);
  // 项目服务缓存共享 store：详情页 load 每次写入，保持左侧展开与服务编辑即时一致
  const svcCache = useSvcCacheStore(s => s.cache);
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
      reportError('加载项目列表失败', e);
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
      reportError('收藏项目失败', e);
    }
  }, [load]);

  const handleStart = useCallback(async (e: React.MouseEvent, id: string, name: string) => {
    e.stopPropagation();
    setActingId(id);
    // 共享动作层：启动 + 项目级监听 + 运行状态刷新 + 逐服务失败汇总（与详情页同一实现）
    await startProjectWithFeedback(id, name);
    setActingId(null);
  }, []);

  const handleStop = useCallback(async (e: React.MouseEvent, id: string, name: string) => {
    e.stopPropagation();
    setActingId(id);
    // 停止：日志清单取自缓存的服务列表（列表页只缓存了服务数组，与详情页口径一致）
    const services = useSvcCacheStore.getState().cache[id] ?? [];
    await stopProjectWithFeedback(id, name, services);
    setActingId(null);
  }, []);

  // ── 展开/折叠 ──────────────────────────────────────────────

  /**
   * 参数收窄成"只需要能阻止冒泡"的结构性类型（CQ-21）。
   *
   * 唯一调用点（`ProjectList` 的 `onToggleSvcExpand`）手上只有 serviceId、**没有**真实事件
   * 对象——签成 `React.MouseEvent` 就会逼它在调用点伪造一个 `as React.MouseEvent` 的假事件；
   * 而假事件在将来真用到 `currentTarget`/`preventDefault` 时会运行期抛 TypeError，
   * 编译期毫无提示。用结构性类型后，调用点传的就是一个名实相符的 no-op。
   */
  const toggleSvcExpand = useCallback((e: { stopPropagation(): void }, projectId: string, serviceId: string) => {
    e.stopPropagation();
    const key = `${projectId}:${serviceId}`;
    // 函数式更新：用捕获的 expandedSvc 计算会让同一 tick 内的两次切换互相覆盖
    // （第二次拿到的仍是旧集合，结果表现为"点两下只生效一次"）
    setExpandedSvc(prev => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);

  const toggleExpand = useCallback(async (e: React.MouseEvent, projectId: string) => {
    e.stopPropagation();
    if (expanded.has(projectId)) {
      // 折叠时清除缓存释放内存（缓存正确性由详情页 load 每次写入保证）
      useSvcCacheStore.getState().invalidate(projectId);
      setExpanded(prev => {
        const next = new Set(prev);
        next.delete(projectId);
        return next;
      });
      return;
    }
    // 展开：先亮加载态，拉到服务列表后才真正展开——期间不展开可避免空缓存被渲染成
    // 「暂无开启目录树的服务」（那是加载失败，不是没有服务）；失败则不展开，重试 = 再点一次
    const seq = ++expandSeqRef.current;
    setExpandingId(projectId);
    try {
      const detail = await projectApi.getDetail(projectId);
      if (seq !== expandSeqRef.current) return; // 过期响应：用户已点了别的项目
      useSvcCacheStore.getState().setCache(projectId, detail.services);
      setExpanded(new Set([projectId])); // 单开（同一时间只展开一个项目）
    } catch (err) {
      if (seq !== expandSeqRef.current) return;
      reportError('加载服务列表失败', err);
    } finally {
      if (seq === expandSeqRef.current) setExpandingId(null);
    }
  }, [expanded]);
  return {
    projects, search, setSearch,
    expanded, expandedSvc, svcCache, expandingId,
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
