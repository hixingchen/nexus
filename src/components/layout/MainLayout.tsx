import { useState, useEffect, useRef, useCallback } from 'react';
import { listen } from '@tauri-apps/api/event';
import { TitleBar } from './TitleBar';
import { StatusBar } from './StatusBar';
import { ResizablePanel } from './ResizablePanel';
import { ProjectList } from './ProjectList';
import { ProjectDetail } from './ProjectDetail';
import { RestartConfirm } from './RestartConfirm';
import { PasteConflictModal } from '../file-tree/PasteConflictModal';
import { AiPanel } from '../ai/AiPanel';
import { CloseGuard } from './CloseGuard';
import { ProjectRail } from './ProjectRail';
import { securityApi, projectApi } from '../../services/service';
import { LAYOUT_KEYS, saveLayout as saveLayoutFn, useLayoutStore } from '../../stores/layoutStore';
import { useLogStore } from '../../stores/logStore';
import { useRunningStore } from '../../stores/runningStore';
import type { LogStream, ServiceLogBatchEvent } from '../../services/logService';
import { reportError } from '../../utils/error';

/**
 * 后端是否明确说"项目不存在"——与"读不到"的其它原因（数据库忙、WAL 锁超时、IPC 失败）
 * 区分开。只用在恢复上次选中项目那一处（CQ-34）：只有**确认项目真的没了**才把"取消选中"
 * 落库，瞬时故障落库是不可逆的数据变更。
 */
function isMissingProject(msg: string): boolean {
  return msg.includes('项目不存在');
}

export function MainLayout() {
  const [selectedProjectId, setSelectedProjectId] = useState<string | null>(null);
  const [selectedProjectName, setSelectedProjectName] = useState<string | null>(null);
  const [selectedProjectPath, setSelectedProjectPath] = useState<string | null>(null);
  const [leftPanelWidth, setLeftPanelWidth] = useState(260);
  const [leftPanelCollapsed, setLeftPanelCollapsed] = useState(false);
  const [servicePanelCollapsed, setServicePanelCollapsed] = useState(false);
  const [ready, setReady] = useState(false);

  // ── 全局 service-log 监听 ──
  // 用 useRef 存 unlisten，避免 Strict Mode 双挂载导致 listener 错乱
  const logListenerRef = useRef<{ unlisten: () => void } | null>(null);

  useEffect(() => {
    type Item = { serviceKey: string; stream: LogStream; data: string; timestamp?: string; seq: number };
    const batch: Item[] = [];
    let timer: ReturnType<typeof setTimeout> | null = null;
    let disposed = false;

    // 注意早退分支也要清 timer（CQ-39）：排程判据是 `if (!timer)`，留着一个已触发的
    // 定时器句柄会让**此后所有批次都不再排程**——日志从此静默停止刷新，而界面只是"不再更新"。
    // 当前 `batch` 只在 flush 里清空，这条早退够不着；一旦有人在这中间加一句过滤/丢弃就立刻可达
    const flush = () => {
      timer = null;
      if (batch.length === 0) return;
      useLogStore.getState().bulkAppend(batch.splice(0));
    };

    // 后端每 ~50ms 发一批（`service-log-batch`，载荷是这个窗口内的所有行）。
    // 这里保留一层 50ms 合帧：多个服务同时刷屏时把它们的批次再合并成一次 store 写入。
    listen<ServiceLogBatchEvent>('service-log-batch', (event) => {
      if (disposed) return;
      for (const line of event.payload.lines) {
        batch.push({
          serviceKey: event.payload.service_key,
          stream: line.stream,
          data: line.text,
          timestamp: line.timestamp,
          seq: line.seq,
        });
      }
      if (!timer) timer = setTimeout(flush, 50);
    }).then(fn => {
      if (disposed) { fn(); return; }
      logListenerRef.current = { unlisten: fn };
    }).catch((e) => {
      // 订阅失败 = 所有服务日志静默停止显示（且 logListenerRef 保持 null、清理变空操作）。
      // 原实现没有 catch：这里既会抛未处理的 rejection，又让用户完全无从察觉。
      reportError('订阅 service-log-batch 失败', e, {
        title: '日志订阅失败',
        description: '服务日志将无法实时显示，请重启应用',
      });
    });

    return () => {
      disposed = true;
      if (timer) clearTimeout(timer);
      if (logListenerRef.current) {
        logListenerRef.current.unlisten();
        logListenerRef.current = null;
      }
    };
  }, []);

  // 全局运行状态轮询（单例：useProjectList / useProjectDetail 订阅同一 store）
  useEffect(() => {
    useRunningStore.getState().startPolling();
    return () => useRunningStore.getState().stopPolling();
  }, []);

  // 布局读写统一走 layoutStore（ARCH-6：键名集中、单次查库、统一防抖写）
  const saveLayout = useCallback((patch: Parameters<typeof saveLayoutFn>[0]) => {
    saveLayoutFn(patch);
  }, []);

  // 启动时从 DB 恢复布局（ensureLoaded 保证整个进程只查库一次，与其他读取方共享）
  useEffect(() => {
    useLayoutStore.getState().ensureLoaded().then(async (layout) => {
      if (layout[LAYOUT_KEYS.selectedProjectId]) {
        try {
          const detail = await projectApi.getDetail(layout[LAYOUT_KEYS.selectedProjectId]);
          setSelectedProjectId(layout[LAYOUT_KEYS.selectedProjectId]);
          setSelectedProjectName(detail.project.name);
          setSelectedProjectPath(detail.project.path);
        } catch (e: unknown) {
          // **失败不等于"项目已删除"**（CQ-34）：这里的 catch 覆盖"项目不存在"与
          // "数据库忙 / WAL 锁超时 / 查询失败"两类，而后者是**瞬时**故障。原实现两件事都做：
          // 清掉内存状态 + `saveLayout({selectedProjectId: ''})` **落库**——一次瞬时读失败
          // 就把"用户上次选中的项目"永久取消选中，没有任何提示、连控制台都没有。
          // 现在：先留痕（不沉默失败），且**只有确认项目真的不存在时才落库**；
          // 其余错误保持 DB 里的值不动，用户重开一次应用就能恢复。
          const msg = String(e);
          reportError('恢复上次选中的项目失败', e, {
            variant: 'warning',
            silent: isMissingProject(msg),
            title: '未能恢复上次选中的项目',
            description: '读取项目详情失败，本次未自动选中（配置里记的项目没有被改动）',
          });
          setSelectedProjectId(null);
          setSelectedProjectName(null);
          setSelectedProjectPath(null);
          // 后端在项目不存在时给的是"项目不存在"（commands/project.rs）；其余错误
          // 一律当作临时故障，不把结论写进数据库
          if (isMissingProject(msg)) {
            saveLayout({ [LAYOUT_KEYS.selectedProjectId]: '' });
          }
        }
      }
      if (layout[LAYOUT_KEYS.leftPanelWidth]) setLeftPanelWidth(Number(layout[LAYOUT_KEYS.leftPanelWidth]));
      if (layout[LAYOUT_KEYS.leftPanelCollapsed] === '1') setLeftPanelCollapsed(true);
      if (layout[LAYOUT_KEYS.servicePanelCollapsed] === '1') setServicePanelCollapsed(true);
      setReady(true);
    }).catch((e: unknown) => {
      // 兜底（正常到不了）：`ensureLoaded` 内部已 catch，所以这里只可能接住**上面这段回调
      // 自身**抛出的异常。留着它是因为后果不对称——漏掉这一行，界面会永远停在"加载中"，
      // 而用户看到的只是一片空白，没有任何线索
      reportError('恢复上次布局时出错（已按默认布局继续）', e);
      setReady(true);
    });
  }, [saveLayout]);

  // 选中项目变化时保存
  useEffect(() => {
    if (!ready) return;
    saveLayout({ [LAYOUT_KEYS.selectedProjectId]: selectedProjectId ?? '' });
  }, [selectedProjectId, ready, saveLayout]);

  useEffect(() => {
    if (!ready) return;
    saveLayout({ [LAYOUT_KEYS.servicePanelCollapsed]: servicePanelCollapsed ? '1' : '0' });
  }, [servicePanelCollapsed, ready, saveLayout]);

  useEffect(() => {
    if (!ready) return;
    saveLayout({ [LAYOUT_KEYS.leftPanelCollapsed]: leftPanelCollapsed ? '1' : '0' });
  }, [leftPanelCollapsed, ready, saveLayout]);

  // 文件监听由 ProjectDetail 统一管理（服务配置变更时需重启监听）

  // 项目切换时设置文件访问白名单
  useEffect(() => {
    // 白名单根设置失败 = 之后所有文件命令都会被拒（现象是"打不开文件"，原因只在这里），
    // 因此必须让用户看见，而不是留一行日志
    securityApi.setProjectRoot(selectedProjectPath).catch((e) => reportError('设置项目根失败', e));
  }, [selectedProjectPath]);

  /**
   * 窄轨双击项目 → 面板展开后要展开哪个项目。用"请求 + 回执"传递：窄轨与列表不会同时挂载，
   * 请求在列表挂载后由它自己执行一次（见 ProjectList 的 effect）。
   */
  const [railExpandProjectId, setRailExpandProjectId] = useState<string | null>(null);

  const leftPanel = (
    <ProjectList
      selectedId={selectedProjectId}
      onSelect={(id) => { setSelectedProjectId(id); if (!id) { setSelectedProjectName(null); setSelectedProjectPath(null); } }}
      onProjectName={setSelectedProjectName}
      onProjectPath={setSelectedProjectPath}
      onCollapse={() => setLeftPanelCollapsed(true)}
      expandProjectId={railExpandProjectId}
      onExpandHandled={() => setRailExpandProjectId(null)}
    />
  );

  /** 收起后的项目列：32px 窄轨（含可点击的项目条目），与服务列收起态同构 */
  const leftRail = (
    <ProjectRail
      selectedId={selectedProjectId}
      onSelect={(p) => { setSelectedProjectId(p.id); setSelectedProjectName(p.name); setSelectedProjectPath(p.path); }}
      onExpand={() => setLeftPanelCollapsed(false)}
      onExpandProject={(p) => {
        setRailExpandProjectId(p.id);
        setLeftPanelCollapsed(false); // 与顶部箭头同一入口（持久化由下面那个 effect 统一处理）
      }}
    />
  );

  const detailPanel = selectedProjectId ? (
    <ProjectDetail
      projectId={selectedProjectId}
      servicePanelCollapsed={servicePanelCollapsed}
      onToggleServicePanel={() => setServicePanelCollapsed(p => !p)}
    />
  ) : (
    <div className="h-full bg-nexus-editor flex items-center justify-center">
      <div className="text-center space-y-3">
        <span className="text-[80px] opacity-[0.06] select-none font-extralight">N</span>
        <p className="text-[12px] text-nexus-muted">选择一个项目开始</p>
      </div>
    </div>
  );

  // 首次加载中
  if (!ready) {
    return (
      <div className="h-screen flex flex-col bg-nexus-editor text-nexus-text">
        <TitleBar />
        <div className="flex-1 flex items-center justify-center">
          <span className="text-[12px] text-nexus-muted">加载中…</span>
        </div>
      </div>
    );
  }

  // 收起态不复用 ResizablePanel：否则分隔条仍可拖动，会与"固定 32px 窄轨"的语义打架
  // （拖出来的宽度无处可去，松手后窄轨不动、只有持久化值被改写）。展开时宽度保持不变。
  const mainContent = leftPanelCollapsed ? (
    <div className="flex h-full w-full overflow-hidden">
      <div className="w-[32px] flex-shrink-0">{leftRail}</div>
      <div className="flex-1 min-w-0 overflow-hidden">{detailPanel}</div>
    </div>
  ) : (
    <ResizablePanel
      left={leftPanel}
      right={detailPanel}
      defaultLeftWidth={leftPanelWidth}
      minWidth={150}
      maxWidth={500}
      direction="horizontal"
      onResize={(w) => { setLeftPanelWidth(w); saveLayout({ left_panel_width: String(w) }); }}
    />
  );

  return (
    <div className="h-screen flex flex-col relative bg-nexus-editor text-nexus-text">
      <TitleBar projectName={selectedProjectName} />
      <div className="flex-1 flex overflow-hidden">
        {/* 主内容（项目列表 / 编辑器 / 服务列）：AI 面板打开时自动让出宽度 */}
        <div className="flex-1 min-w-0 overflow-hidden">
          {mainContent}
        </div>
        {/* AI 助手面板：布局一列，不遮挡内容区；projectName 同步为 dsh 工作区标题 */}
        <AiPanel cwd={selectedProjectPath} projectName={selectedProjectName} />
      </div>
      <RestartConfirm />
      {/* 粘贴同名冲突弹框：**全局只挂一份**——放 FileTree 里会随"项目树 + 每个服务树"各挂一个，
          打开时同时渲染出好几层遮罩（见 stores/pasteConflict 的说明） */}
      <PasteConflictModal />
      <CloseGuard />
      <StatusBar />
    </div>
  );
}
