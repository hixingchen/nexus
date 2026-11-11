import { lazy, Suspense, useState, useEffect, useRef, useCallback } from 'react';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { TitleBar } from './TitleBar';
import { StatusBar } from './StatusBar';
import { ResizablePanel } from './ResizablePanel';
import { ProjectList } from './ProjectList';

// 详情页（编辑器/文件树/服务列）只在选中项目后才需要：按需加载，首屏不解析它。
const ProjectDetail = lazy(() =>
  import('./ProjectDetail').then(m => ({ default: m.ProjectDetail })));
import { RestartConfirm } from './RestartConfirm';
import { PasteConflictModal } from '../file-tree/PasteConflictModal';
import { PortConflictModal } from './PortConflictModal';
import { AiPanel } from '../ai/AiPanel';
import type { NexusDataChanged } from '../../services/aiService';
import { CloseGuard } from './CloseGuard';
import { ProjectRail } from './ProjectRail';
import { ToolCommandResultDialog } from '../ui/ToolCommandResultDialog';
import { ToolCommandIndicator } from '../ui/ToolCommandIndicator';
import { useToolCommandDialog } from '../../hooks/useToolCommandRunner';
import { securityApi, projectApi } from '../../services/service';
import { setTrayProject } from '../../services/system';
import { useSvcCacheStore } from '../../stores/svcCacheStore';
import { startProjectWithFeedback, stopProjectWithFeedback } from '../../stores/serviceActions';
import { useExitIntentStore } from '../../stores/exitIntentStore';
import { LAYOUT_KEYS, saveLayout as saveLayoutFn, useLayoutStore } from '../../stores/layoutStore';
import { parseEditorFontSize } from '../../utils/editorFont';
import { isLogViewerOpen, useLogStore } from '../../stores/logStore';
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

/**
 * 左列（项目列表）的默认宽度。
 *
 * 抽成常量是为了「恢复默认布局」能复用同一个数：重置路径若另写一个字面量，
 * 两处早晚不一致——表现是"恢复默认布局恢复出另一个宽度"，而且很难被发现
 * （用户只会觉得"好像没完全恢复"）。
 */
const DEFAULT_LEFT_PANEL_WIDTH = 260;

export function MainLayout() {
  const [selectedProjectId, setSelectedProjectId] = useState<string | null>(null);
  const [selectedProjectName, setSelectedProjectName] = useState<string | null>(null);
  const [selectedProjectPath, setSelectedProjectPath] = useState<string | null>(null);
  /**
   * 托盘事件回调注册一次、活得比项目切换久，闭包里必须读**最新**的当前项目。
   * （每次切换都重订阅也能做，但会平白多一轮 IPC 监听注册/注销。）
   */
  const selectedRef = useRef<{ id: string | null; name: string | null }>({ id: null, name: null });
  selectedRef.current = { id: selectedProjectId, name: selectedProjectName };
  const [leftPanelWidth, setLeftPanelWidth] = useState(DEFAULT_LEFT_PANEL_WIDTH);
  const [leftPanelCollapsed, setLeftPanelCollapsed] = useState(false);
  const [servicePanelCollapsed, setServicePanelCollapsed] = useState(false);
  const [ready, setReady] = useState(false);
  /**
   * 已落库的值。持久化 effect 只在“当前值 ≠ 已落库值”时写。
   *
   * 只靠 `ready` 会在布局读取失败时立刻用默认值（null/false）覆盖数据库里的真实选择：
   * 一次瞬时读失败就把“上次选中项目/折叠态”永久清掉。用已落库值做基线后，初始渲染与
   * 读取失败都不会触发写入；只有用户真的改了值才写。
   */
  const persistedSelectedRef = useRef<string | null>(null);
  const persistedServiceCollapsedRef = useRef(false);
  const persistedLeftCollapsedRef = useRef(false);
  // 工具命令弹窗挂在这里而不是 ProjectDetail：关掉弹窗/切换项目后命令仍在后台跑，
  // 弹窗与"运行中"指示器必须活得比详情页久（状态在 stores/toolCommandStore）
  const toolCommandDialog = useToolCommandDialog();

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
      // 只为打开着日志面板的服务维护前端缓冲；未打开的服务在打开时用后端快照补齐。
      // 不这样过滤的话，所有服务的输出都会进 JS 对象与 2000 行数组复制（面板没开也付费）。
      if (!isLogViewerOpen(event.payload.service_key)) return;
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

  /**
   * AI 增删改项目后同步当前选择：
   * - 改名/改路径 → 更新标题与 AI 会话工作目录（AiPanel 会按新目录重启会话）；
   * - 被删除 → 清空选择并落库，避免主区域停在一个不存在的项目上报错。
   */
  useEffect(() => {
    let alive = true;
    let unlisten: (() => void) | null = null;
    void listen<NexusDataChanged>('nexus-data-changed', (ev) => {
      if (ev.payload.kind !== 'projects') return;
      void projectApi.getAll().then((list) => {
        if (!alive || !selectedProjectId) return;
        const p = list.find((x) => x.id === selectedProjectId);
        if (!p) {
          setSelectedProjectId(null);
          setSelectedProjectName(null);
          setSelectedProjectPath(null);
          saveLayout({ [LAYOUT_KEYS.selectedProjectId]: '' });
        } else {
          setSelectedProjectName(p.name);
          setSelectedProjectPath(p.path);
        }
      }).catch((e) => reportError('刷新项目列表失败', e));
    }).then((fn) => {
      if (alive) unlisten = fn; else fn();
    }).catch((e) => reportError('订阅 AI 数据变更失败', e));
    return () => { alive = false; unlisten?.(); };
  }, [selectedProjectId, saveLayout]);

  // 启动时从 DB 恢复布局（ensureLoaded 保证整个进程只查库一次，与其他读取方共享）
  useEffect(() => {
    useLayoutStore.getState().ensureLoaded().then(async (layout) => {
      if (layout[LAYOUT_KEYS.selectedProjectId]) {
        try {
          const detail = await projectApi.getDetail(layout[LAYOUT_KEYS.selectedProjectId]);
          setSelectedProjectId(layout[LAYOUT_KEYS.selectedProjectId]);
          persistedSelectedRef.current = layout[LAYOUT_KEYS.selectedProjectId];
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
          // 不论瞬时故障还是项目已删除，本次渲染都不该触发持久化覆盖：
          // 瞬时故障保留库里原值；确认删除时下面的 saveLayout 才是唯一的写入。
          persistedSelectedRef.current = null;
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
      persistedLeftCollapsedRef.current = layout[LAYOUT_KEYS.leftPanelCollapsed] === '1';
      persistedServiceCollapsedRef.current = layout[LAYOUT_KEYS.servicePanelCollapsed] === '1';
      setReady(true);
    }).catch((e: unknown) => {
      // 兜底（正常到不了）：`ensureLoaded` 内部已 catch，所以这里只可能接住**上面这段回调
      // 自身**抛出的异常。留着它是因为后果不对称——漏掉这一行，界面会永远停在"加载中"，
      // 而用户看到的只是一片空白，没有任何线索
      reportError('恢复上次布局时出错（已按默认布局继续）', e);
      setReady(true);
    });
  }, [saveLayout]);

  /**
   * 「恢复默认布局」（设置 > 通用）：清库之后由**各消费方自己**把默认值贴回来。
   *
   * 为什么清了库还要这一步：默认值是本组件的字面量（见 `DEFAULT_LEFT_PANEL_WIDTH`），
   * 而后端只知道"库里有什么"，不知道默认值该是多少；更关键的是**组件只在挂载时读一次
   * 布局**，库里那些行没了并不会让已经渲染出来的面板改尺寸。`ResizablePanel` 也只在
   * `defaultLeftWidth` 这个 prop 变化时才同步内部尺寸（见它的注释），所以这里必须
   * 显式 set 一遍。
   *
   * 这也是这条路优于"清库 + 刷新窗口"的原因：刷新会关掉所有已打开的标签页、
   * 并拆掉 AI 会话（`AiPanel` 卸载时会 `destroyWv`），而用户要的只是面板回到初始尺寸。
   */
  const resetSeq = useLayoutStore(s => s.resetSeq);
  useEffect(() => {
    if (resetSeq === 0) return;   // 0 = 从未重置（首次挂载不走这里）
    setLeftPanelWidth(DEFAULT_LEFT_PANEL_WIDTH);
    setLeftPanelCollapsed(false);
    setServicePanelCollapsed(false);
  }, [resetSeq]);

  /**
   * 把「文本字号」写到 `:root` 的 `--nexus-text-size` 上——纯 CSS 的文本面靠它跟着变
   * （Markdown 预览的 `.md-preview` 一族：编辑器里的预览与「帮助 > 使用文档」）。
   *
   * 为什么代码视图不读这个变量：CodeMirror 缓存字符宽度与行高做光标定位，只改 CSS 会让它
   * 算错位置，必须走 `reconfigure` 重新测量（见 CodeViewer 的 fontSizeCompartment）。
   * 两条路径同源于同一个设置值，所以不会出现两处不一致。
   *
   * 为什么挂在 MainLayout：它是应用根，任何时候都挂载着（设置面板与标签页都可能关着），
   * 而字号必须在**启动时**就生效——不能等用户打开设置面板才写这个变量。
   */
  const textSize = useLayoutStore(s => parseEditorFontSize(s.values?.[LAYOUT_KEYS.editorFontSize]));
  useEffect(() => {
    document.documentElement.style.setProperty('--nexus-text-size', `${textSize}px`);
  }, [textSize]);

  // 选中项目变化时保存（只有真正偏离已落库值才写，见 persistedSelectedRef）
  useEffect(() => {
    if (!ready || selectedProjectId === persistedSelectedRef.current) return;
    persistedSelectedRef.current = selectedProjectId;
    saveLayout({ [LAYOUT_KEYS.selectedProjectId]: selectedProjectId ?? '' });
  }, [selectedProjectId, ready, saveLayout]);

  useEffect(() => {
    if (!ready || servicePanelCollapsed === persistedServiceCollapsedRef.current) return;
    persistedServiceCollapsedRef.current = servicePanelCollapsed;
    saveLayout({ [LAYOUT_KEYS.servicePanelCollapsed]: servicePanelCollapsed ? '1' : '0' });
  }, [servicePanelCollapsed, ready, saveLayout]);

  useEffect(() => {
    if (!ready || leftPanelCollapsed === persistedLeftCollapsedRef.current) return;
    persistedLeftCollapsedRef.current = leftPanelCollapsed;
    saveLayout({ [LAYOUT_KEYS.leftPanelCollapsed]: leftPanelCollapsed ? '1' : '0' });
  }, [leftPanelCollapsed, ready, saveLayout]);

  // 当前选中项目 → 托盘菜单（启动/停止项的文字与可用状态）
  useEffect(() => {
    void setTrayProject(selectedProjectName);
  }, [selectedProjectName]);

  // 托盘「启动/停止当前项目」：与界面上的按钮走同一套动作与反馈（含日志/监听/提示）。
  // 窗口隐藏时前端仍存活，事件照常送达；失败反馈会再转成系统通知。
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    listen<string>('tray-action', (event) => {
      if (disposed) return;
      const { id, name } = selectedRef.current;
      if (!id) return;
      const display = name ?? '当前项目';
      if (event.payload === 'start-project') {
        void startProjectWithFeedback(id, display);
      } else if (event.payload === 'stop-project') {
        const services = useSvcCacheStore.getState().cache[id] ?? [];
        void stopProjectWithFeedback(id, display, services);
      }
    }).then(fn => {
      if (disposed) fn(); else unlisten = fn;
    }).catch(e => reportError('订阅托盘操作失败', e, { silent: true }));
    return () => { disposed = true; unlisten?.(); };
  }, []);

  // 托盘「退出」：标记一次性意图后走**同一条**关窗守卫（未保存草稿确认 → prepare_exit）。
  // 后端在发事件前已把窗口显示出来，确认框不会出现在隐藏的窗口里。
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    listen('tray-quit', () => {
      if (disposed) return;
      useExitIntentStore.getState().requestTrayQuit();
      void getCurrentWindow().close().catch(e => reportError('托盘退出失败', e));
    }).then(fn => {
      if (disposed) fn(); else unlisten = fn;
    }).catch(e => reportError('订阅托盘退出失败', e, { silent: true }));
    return () => { disposed = true; unlisten?.(); };
  }, []);

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
    <Suspense fallback={<div className="h-full bg-nexus-editor" />}>
      <ProjectDetail
        projectId={selectedProjectId}
        servicePanelCollapsed={servicePanelCollapsed}
        onToggleServicePanel={() => setServicePanelCollapsed(p => !p)}
      />
    </Suspense>
  ) : (
    <div className="h-full bg-nexus-editor flex items-center justify-center">
      <div className="text-center space-y-3">
        <span className="text-[80px] opacity-[0.06] select-none font-extralight">N</span>
        <p className="text-[12px] text-nexus-muted">选择一个项目开始</p>
      </div>
    </div>
  );

  /**
   * 主内容：项目列 + 编辑器/服务列。
   *
   * **收起与展开必须是同一棵树**（只是左槽换成窄轨、分隔条消失）：原先写成
   * `cond ? <div>…</div> : <ResizablePanel/>`，根元素类型一变，React 就把整块右区
   * （详情页 / 模板库 / 文件树 / 编辑器）卸载重建——点一次收缩，这些模块全部重挂载并
   * 各自重拉数据，界面上闪「暂无模板」「暂无项目」和一片空白（实测连闪两帧 ~130ms）。
   * 现在根一直是 `ResizablePanel`，右区原地保留，收缩只剩左槽换视图这一件事。
   */
  const mainContent = (
    <ResizablePanel
      left={leftPanelCollapsed ? leftRail : leftPanel}
      right={detailPanel}
      collapsed={leftPanelCollapsed}
      // 收起态不复用 `leftPanelWidth`：分隔条已消失，拖出来的宽度无处可去，
      // 松手后窄轨不动、只有持久化值被改写。展开时宽度仍是收起前那个值
      collapsedWidth={32}
      defaultLeftWidth={leftPanelWidth}
      minWidth={150}
      maxWidth={500}
      direction="horizontal"
      onResize={(w) => { setLeftPanelWidth(w); saveLayout({ [LAYOUT_KEYS.leftPanelWidth]: String(w) }); }}
    />
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
      <PortConflictModal />
      <CloseGuard />
      <ToolCommandResultDialog
        open={!!toolCommandDialog.state}
        commandName={toolCommandDialog.state?.commandName ?? ''}
        runId={toolCommandDialog.state?.runId ?? null}
        result={toolCommandDialog.state?.result ?? null}
        error={toolCommandDialog.state?.error ?? null}
        loading={toolCommandDialog.state?.loading ?? false}
        onStop={toolCommandDialog.stop}
        onClose={toolCommandDialog.close}
      />
      <ToolCommandIndicator />
      <StatusBar />
    </div>
  );
}
