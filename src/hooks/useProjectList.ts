import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { projectApi, type Project } from '../services/service';
import { useRunningStore } from '../stores/runningStore';
import { useSvcCacheStore } from '../stores/svcCacheStore';
import { useProjectActions } from './useProjectActions';
import { reportError } from '../utils/error';

/**
 * 最近一次成功拉到的项目列表（模块级，跨卸载保留）。
 *
 * 列表（ProjectList）与窄轨（ProjectRail）是**两个组件、各持一份 state**，且不会同时挂载，
 * 所以每次收缩/展开切换，接手的那一方都是首次挂载：没有这份缓存，它的首帧必然是空列表——
 * 展开时闪「暂无项目」+「创建第一个项目」，收起时窄轨空一拍（实测各 ~1~2 帧，把 IPC
 * 延迟算进去更久）。数据仍以 `load()` 为事实来源，这里只是让首帧别说"没有项目"这句
 * 没根据的话（同 `detailCache` / `svcCacheStore` 的口径）。
 */
let lastProjects: Project[] = [];

/** 首帧用的项目列表（只读快照语义：拿到后仍会照常 `load()` 刷新） */
export function cachedProjects(): Project[] {
  return lastProjects;
}

/**
 * 拉一份项目列表并写进共享缓存。
 *
 * 两个消费方（`useProjectList.load` 与 `ProjectRail` 的加载 effect）都走这里，
 * 而不是各自 `projectApi.getAll()`：缓存若只有一个写入口，"谁写的是新数据"就不需要再判。
 */
export async function fetchProjects(): Promise<Project[]> {
  const list = await projectApi.getAll();
  lastProjects = list;
  return list;
}

/**
 * ProjectList 组件的业务逻辑 hook
 * 管理项目列表、运行状态、搜索、CRUD 操作等
 */
export function useProjectList() {
  const [projects, setProjects] = useState<Project[]>(cachedProjects);
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

  // modal state
  const [showNewModal, setShowNewModal] = useState(false);
  const [ctxMenu, setCtxMenu] = useState<{ id: string; name: string; path: string; x: number; y: number } | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<{ id: string; name: string } | null>(null);
  const [editTarget, setEditTarget] = useState<Project | null>(null);
  const [duplicateTarget, setDuplicateTarget] = useState<{ id: string; name: string } | null>(null);

  // ── 数据加载 ──────────────────────────────────────────────

  const load = useCallback(async () => {
    try {
      setProjects(await fetchProjects());   // 顺带刷新共享缓存（见 cachedProjects）
    } catch (e) {
      reportError('加载项目列表失败', e);
    }
  }, []);

  useEffect(() => { load(); }, [load]);

  // 项目动作（启动/停止/收藏）与窄轨共用一份实现（见 useProjectActions 的说明）。
  // 收藏会改变排序（后端 ORDER BY pinned DESC），所以动作完成后要重新拉列表
  const { actingId, start, stop, togglePin } = useProjectActions(load);

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

  /**
   * 只展开、不折叠。
   *
   * 为什么单列一个而不是复用 toggleExpand：**"从窄轨双击进来"要的是"看到它"**，
   * 而 toggle 在"已经展开"时会把它收起来——正好与调用方的意图相反。列表里的双击
   * 仍然是 toggle（那里看得见展开状态，用户点的是"切换"）。
   */
  const expandProject = useCallback(async (projectId: string) => {
    if (expanded.has(projectId)) return;
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

  // 参数类型与上面的 toggleSvcExpand 同一口径（只用到 stopPropagation）：展开入口
  // 既有鼠标（双击卡片）也有键盘（卡片上的 ArrowRight/ArrowLeft，UX-7），
  // 键盘那条路没有真实鼠标事件——不写成 React.MouseEvent 就不必伪造一个假事件传进来
  const toggleExpand = useCallback(async (e: { stopPropagation(): void }, projectId: string) => {
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
    await expandProject(projectId);
  }, [expanded, expandProject]);
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
    start, stop, togglePin,
    toggleSvcExpand, toggleExpand, expandProject,
  };
}
