import { useState, useCallback, useEffect, useMemo, lazy, Suspense } from 'react';
import { createPortal } from 'react-dom';
import { EditorTabs } from '../editor/EditorTabs';
// CodeViewer 是首屏最大的一块（CodeMirror 核心 326KB），改为按需加载（PERF-19）：
// 未打开文件时不该为它付出解析成本（冷启动首屏实测因此少 326KB raw）。
// 它与下方三个 Viewer 不同：那些各自很小，静态引入无妨。
const CodeViewer = lazy(() => import('../editor/CodeViewer').then(m => ({ default: m.CodeViewer })));
const MarkdownPreview = lazy(() => import('../editor/MarkdownPreview').then(m => ({ default: m.MarkdownPreview })));
import { ImageViewer } from '../editor/ImageViewer';
import { HexViewer } from '../editor/HexViewer';
import { JarViewer } from '../editor/JarViewer';
import { LogViewer } from '../terminal/LogViewer';
import { Modal } from '../ui/Modal';
import { ErrorBoundary } from '../ui/ErrorBoundary';
import { PanelToggleIcon } from '../ui/PanelToggleIcon';
import { ToolCommandResultDialog } from '../ui/ToolCommandResultDialog';
import { ResizablePanel } from './ResizablePanel';
import { ServiceTreeEntry } from './ServiceTreeEntry';
import { ServiceContextMenu } from './ServiceContextMenu';
import { TemplateTreeEntry } from './TemplateTreeEntry';
import { SearchResultPanel } from './SearchResultPanel';
import { ServiceEditPanel } from './ServiceEditPanel';
import {
  DndContext,
  KeyboardSensor,
  PointerSensor,
  useSensor,
  useSensors,
  type DragEndEvent,
} from '@dnd-kit/core';
import { SortableContext, arrayMove, sortableKeyboardCoordinates, verticalListSortingStrategy } from '@dnd-kit/sortable';
import { useProjectDetail } from '../../hooks/useProjectDetail';
import { useEditorStore } from '../../stores/editor';
import { isMarkdownFile } from '../../utils/markdown';
import { useAiStore, selectAiRunningHere, selectAiPanelVisible } from '../../stores/aiStore';
import { RobotIcon } from '../ai/RobotIcon';
import { openToolsApi, parseToolCommands, serviceApi, type Service, type ServiceTemplate } from '../../services/service';
import { openInExplorer, openTerminal } from '../../services/system';
import { useToolStore } from '../../stores/toolStore';
import { useServiceActions } from '../../hooks/useServiceActions';
import { LAYOUT_KEYS, saveLayout, useLayoutStore } from '../../stores/layoutStore';
import { useToolCommandRunner } from '../../hooks/useToolCommandRunner';
import { showNotification } from '../ui/Toast';
// 模板导入导出用系统文件对话框选路径：后端按"用户显式选择的路径"处理（与打开工具选 exe 同口径）
import { open as openFileDialog, save as saveFileDialog } from '@tauri-apps/plugin-dialog';
import { reportError } from '../../utils/error';
import { pruneServiceDrafts } from '../../stores/serviceDraftStore';

/**
 * 空白模板：模板库点「新建」时用它打开既有的编辑面板。
 *
 * `id` 为空是**约定**而非"缺数据"：ServiceEditPanel 的 handleSave 据此分流——
 * 有 id 走 updateTemplate，没有则走 createServiceTemplate（id 由后端生成）。
 */
const EMPTY_TEMPLATE: ServiceTemplate = {
  id: '', name: '', command: '', cwd: '',
  // 留空而不是 '[]' / '{}'：那两串是**建表时的列默认值**，不是给用户看的初始值
  // （`[]` 摆进「监听路径」框、`{}` 摆进「环境变量」框都只会让人以为要填 JSON）
  watch_paths: '', watch_include: '*', watch_exclude: '', env_vars: '',
  restart_mode: 0, enabled: true, show_file_tree: false,
  tool_commands: '[]', open_tool_id: '', created_at: '',
};

interface Props {
  projectId: string;
  servicePanelCollapsed: boolean;
  onToggleServicePanel: () => void;
}

export function ProjectDetail({ projectId, servicePanelCollapsed, onToggleServicePanel }: Props) {
  const {
    detail, services, loading, editingService, setEditingService,
    deleteSvcTarget, setDeleteSvcTarget, deleting,
    viewingLog, setViewingLog,
    activeTab, load, reorderServicesLocal,
    isServiceRunning, isServiceFailed, handleStartAll, handleStopAll,
    handleDeleteService, handleViewLog,
  } = useProjectDetail(projectId);
  // 只订阅这个布尔：`fileContent` 由 MarkdownPreview 自己订阅（见该组件说明）
  const mdPreview = useEditorStore(s => s.mdPreview);
  const mdPreviewOnThisTab = mdPreview && !!activeTab && isMarkdownFile(activeTab.path);

  // 工具命令执行（订阅事件流 / 跑命令 / 失败态 / 停止）抽到独立 hook（ARCH-7 ②）
  const {
    state: toolCommandState,
    run: handleRunToolCommand,
    stop: handleStopToolCommand,
    close: handleCloseToolCommand,
  } = useToolCommandRunner();

  // ── 服务模板库（全局、跨项目，右侧面板下半区） ──
  const [templates, setTemplates] = useState<ServiceTemplate[]>([]);
  const [addingTemplate, setAddingTemplate] = useState(false);
  const [editingTemplate, setEditingTemplate] = useState<ServiceTemplate | null>(null);
  const [deleteTplTarget, setDeleteTplTarget] = useState<{ id: string; name: string } | null>(null);
  const [deletingTpl, setDeletingTpl] = useState(false);
  // 上半区（项目服务）初始高度：默认 3/5 窗口高度（扣 TitleBar/StatusBar 约 56px），DB 恢复值到达后覆盖
  const [topPanelHeight, setTopPanelHeight] = useState(() =>
    Math.round(Math.max(96, Math.min((window.innerHeight - 56) * 0.6, window.innerHeight - 200))));
  const topPanelMaxHeight = Math.round(window.innerHeight - 200);

  const loadTemplates = useCallback(() => {
    serviceApi.getServiceTemplates()
      .then(list => {
        setTemplates(list);
        // 模板被删除后其编辑草稿没有意义（还会让关窗确认多算一项）；服务草稿在
        // useProjectDetail 的 load 里按当前项目的服务列表清理
        pruneServiceDrafts('template', list.map(t => t.id));
      })
      .catch(e => reportError('加载模板失败', e));
  }, []);

  useEffect(() => {
    loadTemplates();
    // 布局读走 layoutStore（与 MainLayout/AiPanel 共享同一次查库）
    useLayoutStore.getState().ensureLoaded().then(l => {
      const h = l[LAYOUT_KEYS.rightPanelTopHeight];
      if (h) setTopPanelHeight(Number(h));
    }).catch(() => { /* 读失败用默认高度（ensureLoaded 内部已留痕） */ });
  }, [loadTemplates]);

  // 防抖保存上半区高度：与其余布局键共用 layoutStore 的防抖窗口（原实现各自一份定时器）
  const persistTopPanelHeight = useCallback((h: number) => {
    saveLayout({ [LAYOUT_KEYS.rightPanelTopHeight]: String(h) });
  }, []);

  // 拖拽排序持久化：本地立即重排（回弹动画结束后 UI 无缝衔接，不等后端重拉），
  // 异步写入后端；失败才重载恢复后端顺序
  const handleReorderServices = useCallback((orderedIds: string[]) => {
    const pid = detail?.project?.id;
    if (!pid) return;
    reorderServicesLocal(orderedIds);
    serviceApi.reorderServices(pid, orderedIds).catch(e => {
      reportError('保存服务排序失败', e);
      load();
    });
  }, [detail?.project?.id, reorderServicesLocal, load]);

  const handleReorderTemplates = useCallback((orderedIds: string[]) => {
    setTemplates(prev => {
      const byId = new Map(prev.map(t => [t.id, t]));
      return orderedIds.map(id => byId.get(id)).filter((t): t is ServiceTemplate => !!t);
    });
    serviceApi.reorderServiceTemplates(orderedIds).catch(e => {
      reportError('保存模板排序失败', e);
      loadTemplates();
    });
  }, [loadTemplates]);

  // 执行工具命令：实现见 hooks/useToolCommandRunner.ts（订阅 + 流式输出 + 停止）

  // 从模板添加到当前项目（值拷贝，模板不受影响）
  const handleAddTemplate = async (tpl: ServiceTemplate) => {
    setAddingTemplate(true);
    try {
      await serviceApi.addServiceFromTemplate(projectId, tpl.id);
      showNotification({ title: `已从模板添加「${tpl.name}」` });
      await load();
    } catch (e: unknown) {
      reportError('添加服务失败', e);
    }
    setAddingTemplate(false);
  };

  // 删除模板：先弹确认框（与服务删除流程一致），确认后才执行
  const requestDeleteTemplate = useCallback((tpl: ServiceTemplate) => {
    setDeleteTplTarget({ id: tpl.id, name: tpl.name });
  }, []);

  const confirmDeleteTemplate = useCallback(async () => {
    if (!deleteTplTarget) return;
    setDeletingTpl(true);
    try {
      await serviceApi.deleteServiceTemplate(deleteTplTarget.id);
      if (editingTemplate?.id === deleteTplTarget.id) setEditingTemplate(null);
      loadTemplates();
      showNotification({ variant: 'warning', title: `已删除模板「${deleteTplTarget.name}」` });
    } catch (err) {
      reportError('删除模板失败', err);
    }
    setDeletingTpl(false);
    setDeleteTplTarget(null);
  }, [deleteTplTarget, editingTemplate, loadTemplates]);

  /**
   * 新建服务：用空壳打开右侧面板（与"新建模板"同构——`id` 为空即新建）。
   *
   * 为什么不再用弹窗：那个弹窗只有名称/命令/工作目录三个字段，监听规则与两个展示
   * 开关由后端写死，用户想配全得先把服务建出来、再点开卡片打开**同一个面板**。
   * 两套字段、两套组件，而且"添加用弹窗、编辑用面板"本身就不一致。
   */
  const handleCreateService = useCallback(() => {
    setEditingTemplate(null);
    setEditingService(prev => (prev && prev.id === '' ? null : {
      id: '', project_id: projectId, name: '', command: '', cwd: '',
      // watch_include 的 `*` 与后端默认一致（就一个字符，两边写死不会漂）；
      // watch_exclude 留空 = 交给后端填默认——那套排除规则只该有一个来源
      // 同样留空而不是 '[]'：那是建表默认值。后端把空串与 '[]' 一视同仁（都用 cwd 兜底），
      // 但用户看到空框才知道"这里我还没配"
      watch_paths: '', watch_include: '*', watch_exclude: '',
      // 环境变量留空而不是 '{}'：那串东西既不是 dotenv 格式（KEY=VALUE 每行一条），
      // 也不是"空"——它只是建表时的列默认值。摆进编辑框只会让人以为要填 JSON
      env_vars: '', restart_mode: 0, enabled: true, show_file_tree: false,
      sort_index: 0, tool_commands: '[]',
    }));
  }, [projectId, setEditingService, setEditingTemplate]);

  /**
   * 新建模板：用空壳打开既有的编辑面板。
   *
   * `id` 为空是**约定**——`ServiceEditPanel` 据此走 `createServiceTemplate` 而不是
   * `updateTemplate`（见那边的 handleSave）。模板库此前只能"从服务另存为模板"，
   * 想建一个还没在项目里跑过的配置得先编一个服务出来。
   */
  const handleCreateTemplate = useCallback(() => {
    setEditingService(null);
    // 面板正停在「新建」上（id 为空）时再点一次 → 关闭，与点卡片/服务条目的 toggle 一致。
    //
    // 不能写成 setEditingTemplate(EMPTY_TEMPLATE)：传的是同一个模块级常量，React 用
    // Object.is 比较后**直接跳过更新**——再点会毫无反应（用户反馈过这一点）。
    // 输入不会因此丢：草稿按 id 存，新建的键固定是空 id，重开自动恢复
    setEditingTemplate(prev => (prev && prev.id === '' ? null : EMPTY_TEMPLATE));
  }, [setEditingService, setEditingTemplate]);

  /** 导出模板到 JSON 文件。ids 为空 = 全部导出；单个模板走右键菜单传它自己的 id */
  const handleExportTemplates = useCallback(async (ids: string[] = []) => {
    try {
      const path = await saveFileDialog({
        title: ids.length === 1 ? '导出这个模板' : '导出全部模板',
        defaultPath: ids.length === 1 ? 'nexus-template.json' : 'nexus-templates.json',
        filters: [{ name: 'JSON', extensions: ['json'] }],
      });
      if (!path) return; // 用户取消
      const count = await serviceApi.exportServiceTemplates(path, ids);
      showNotification({ title: `已导出 ${count} 个模板`, description: path, duration: 4000 });
    } catch (e: unknown) {
      reportError('导出模板失败', e);
    }
  }, []);

  /** 从 JSON 文件导入模板：重名自动改名、工具按名字匹配本机工具库 */
  const handleImportTemplates = useCallback(async () => {
    try {
      const selected = await openFileDialog({
        title: '选择模板导出文件',
        multiple: false,
        filters: [{ name: 'JSON', extensions: ['json'] }],
      });
      if (typeof selected !== 'string') return; // 用户取消
      const res = await serviceApi.importServiceTemplates(selected);
      loadTemplates();
      // 如实说明发生了什么。尤其后两类——不说清楚，用户只会看到"导入了但用不了"
      // （模板的目录是导出方机器上的路径；工具名在本机工具库里没有）
      const notes: string[] = [];
      if (res.duplicated > 0) notes.push(`${res.duplicated} 个与现有模板同名（未改名）`);
      if (res.missing_tools.length > 0) notes.push(`本机没有这些工具、未绑定：${res.missing_tools.join('、')}`);
      if (res.missing_dirs.length > 0) notes.push(`工作目录在本机不存在：${res.missing_dirs.join('、')}`);
      showNotification({
        variant: notes.length > 0 ? 'warning' : 'success',
        title: `已导入 ${res.imported} 个模板`,
        description: notes.length > 0 ? notes.join('；') : undefined,
        duration: notes.length > 0 ? 6000 : 3000,
      });
    } catch (e: unknown) {
      reportError('导入模板失败', e);
    }
  }, [loadTemplates]);

  // 打开服务/模板编辑面板（互斥：同一时间只开一个）
  const openServiceEdit = useCallback((svc: Service) => {
    setEditingTemplate(null);
    setEditingService(prev => prev?.id === svc.id ? null : svc);
    // 两个 setter 都是 useState 的稳定引用（React 保证身份不变），进依赖不会导致重建
  }, [setEditingTemplate, setEditingService]);
  // 与服务卡片一致：再次点击正在编辑的模板 → 关闭面板
  const openTemplateEdit = useCallback((tpl: ServiceTemplate) => {
    setEditingService(null);
    setEditingTemplate(prev => prev?.id === tpl.id ? null : tpl);
  }, [setEditingService, setEditingTemplate]);

  if (!detail) {
    return (
      <div className="flex items-center justify-center h-full text-[12px] text-nexus-muted">
        加载中…
      </div>
    );
  }

  const { project } = detail;
  // 服务列表来自 hook 的单一来源（svcCacheStore），不再从 detail 里取（ARCH-7 ③）

  return (
    <div className="h-full bg-nexus-editor flex relative overflow-hidden">
      {/* 主区域：代码查看器 / 空状态。
          padding 补偿服务列宽度（瞬时变化，日志只 reflow 一次而非动画期间每帧 reflow） */}
      {/* paddingBottom 由搜索面板的 CSS 变量驱动（`--nexus-search-panel-h`，见 SearchResultPanel）：
          面板贴底时给它让出同高的空间，否则滚到底的最后几行永远被盖住。
          水平方向的让位（pr-*，给服务列）早就在做了，这里补上垂直方向。
          absolute 定位相对的是 padding box，所以面板自己仍然贴着容器底边 */}
      <div
        className={`flex-1 flex flex-col overflow-hidden relative ${servicePanelCollapsed ? 'pr-[32px]' : 'pr-[360px]'}`}
        style={{ paddingBottom: 'var(--nexus-search-panel-h, 0px)' }}
      >
        {/* ErrorBoundary：CodeMirror/各查看器任何渲染异常都不击穿应用白屏。
            key 随内容变化 → 出错后切文件/日志即自动重置（新内容重试） */}
        <ErrorBoundary key={viewingLog ?? activeTab?.path ?? 'empty'}>
          {viewingLog ? (
            <LogViewer
              serviceKey={viewingLog}
              serviceName={services.find(s => s.id === viewingLog)?.name}
              fill
              onClose={() => setViewingLog(null)}
            />
          ) : activeTab ? (
            <>
              <EditorTabs />
              <div className="flex-1 overflow-hidden">
                {activeTab.viewerType === 'image' ? (
                  <ImageViewer path={activeTab.path} name={activeTab.name} />
                ) : activeTab.viewerType === 'hex' ? (
                  <HexViewer path={activeTab.path} />
                ) : activeTab.viewerType === 'jar' ? (
                  <JarViewer path={activeTab.path} />
                ) : mdPreviewOnThisTab ? (
                  // Markdown 预览：内容取 store 里的当前文本（含未保存的编辑）——
                  // 预览要从正在编辑的内容渲染，而不是磁盘上的旧版本
                  <Suspense fallback={<div className="h-full bg-nexus-bg" />}>
                    <MarkdownPreview filePath={activeTab.path} />
                  </Suspense>
                ) : (
                  // fallback 用编辑器自身的底色占位：chunk 从本地盘加载（几十 ms），
                  // 空白会让"打开文件"看起来像闪了一下
                  <Suspense fallback={<div className="h-full bg-nexus-bg" />}>
                    <CodeViewer
                      filePath={activeTab.path}
                      editable={!activeTab.readonly}
                      onChange={(content) => useEditorStore.getState().updateDraft(content)}
                    />
                  </Suspense>
                )}
              </div>
            </>
          ) : (
            <EmptyState name={project.name} path={project.path} />
          )}
        </ErrorBoundary>

        {editingService && (
          <ServiceEditPanel
            key={editingService.id}
            service={editingService}
            onSave={async () => { await load(); setEditingService(null); }}
            onSavedAsTemplate={loadTemplates}
            // 标题栏（服务名 + 关闭）：此前服务面板没有标题栏，退出只能靠"再点一次卡片"
            // 这个隐蔽操作——用户问过"点开了怎么关"。
            // 新建时 name 是空的，得给个说法，否则标题栏只剩一个 ✕
            title={editingService.id ? editingService.name : '添加服务'}
            onClose={() => setEditingService(null)}
            // 面板右侧偏移 = 服务列宽（absolute 覆盖需让位）
            rightOffset={servicePanelCollapsed ? 32 : 360}
          />
        )}

        {editingTemplate && (
          <ServiceEditPanel
            key={`tpl-${editingTemplate.id}`}
            service={editingTemplate}
            mode="template"
            title="编辑模板"
            onSave={async () => { await loadTemplates(); setEditingTemplate(null); }}
            onClose={() => setEditingTemplate(null)}
            rightOffset={servicePanelCollapsed ? 32 : 360}
          />
        )}

        {/* 底部搜索结果面板（浮层覆盖编辑器，不挤压其高度）；右侧让位服务列宽度；
            日志视图打开时让位（日志优先完整显示，搜索面板等同被挡住，关日志后恢复） */}
        <SearchResultPanel
          rightOffset={servicePanelCollapsed ? 32 : 360}
          blocked={!!viewingLog}
        />
      </div>

      {/* 右侧面板：服务列表（上）+ 模板库（下），可收缩 */}
      <ServicePanel
        services={services}
        collapsed={servicePanelCollapsed}
        onToggle={() => { onToggleServicePanel(); setEditingService(null); setEditingTemplate(null); }}
        splitPanel={{
          bottom: <TemplateSection
            templates={templates}
            busy={addingTemplate}
            editingId={editingTemplate?.id ?? null}
            onEdit={openTemplateEdit}
            onAdd={handleAddTemplate}
            onRequestDelete={requestDeleteTemplate}
            onReorderTemplates={handleReorderTemplates}
            onCreate={handleCreateTemplate}
            onImport={() => { void handleImportTemplates(); }}
            onExportAll={() => { void handleExportTemplates(); }}
            onExportOne={tpl => { void handleExportTemplates([tpl.id]); }}
          />,
          topHeight: topPanelHeight,
          topMaxHeight: topPanelMaxHeight,
          onResize: persistTopPanelHeight,
        }}
        editingService={editingService}
        onEditService={openServiceEdit}
        isServiceRunning={isServiceRunning}
        isServiceFailed={isServiceFailed}
        setDeleteSvcTarget={setDeleteSvcTarget}
        // 改名自 setShowAddServiceModal：添加服务不再是"开个弹窗"，而是打开右侧编辑面板
        onCreateService={handleCreateService}
        handleStartAll={handleStartAll}
        handleStopAll={handleStopAll}
        handleViewLog={(svc) => {
          handleViewLog(svc);
          setEditingService(null);
          setEditingTemplate(null);
        }}
        handleRunToolCommand={handleRunToolCommand}
        handleReorderServices={handleReorderServices}
        loading={loading}
        load={load}
      />

      {/* Modals（"添加服务"不再是弹窗：改用右侧 ServiceEditPanel，与编辑服务、
          新建模板同一套表单——弹窗那份只有三个字段，配全还得再开一次面板） */}
      <Modal open={!!deleteSvcTarget} title="确认删除" onClose={() => setDeleteSvcTarget(null)}>
        <div className="space-y-4">
          <p className="text-[13px] text-nexus-text">
            确定要删除服务 <span className="text-nexus-warning font-medium">「{deleteSvcTarget?.name}」</span> 吗？
          </p>
          <p className="text-[12px] text-nexus-muted">此操作不可撤销。</p>
          <div className="flex items-center justify-end gap-2">
            <button
              className="px-4 py-1.5 text-[12px] text-nexus-text-muted hover:text-nexus-text rounded hover:bg-nexus-hover/50"
              onClick={() => setDeleteSvcTarget(null)}
            >取消</button>
            <button
              className="px-5 py-1.5 text-[13px] bg-nexus-error text-white rounded hover:bg-nexus-error/80 disabled:opacity-40"
              disabled={deleting}
              onClick={handleDeleteService}
            >{deleting ? '删除中…' : '确认删除'}</button>
          </div>
        </div>
      </Modal>

      {/* 删除模板确认（与服务删除同款；模板是值拷贝，不影响已添加的项目服务） */}
      <Modal open={!!deleteTplTarget} title="确认删除" onClose={() => setDeleteTplTarget(null)}>
        <div className="space-y-4">
          <p className="text-[13px] text-nexus-text">
            确定要删除模板 <span className="text-nexus-warning font-medium">「{deleteTplTarget?.name}」</span> 吗？
          </p>
          <p className="text-[12px] text-nexus-muted">此操作不可撤销，已从该模板添加的项目服务不受影响。</p>
          <div className="flex items-center justify-end gap-2">
            <button
              className="px-4 py-1.5 text-[12px] text-nexus-text-muted hover:text-nexus-text rounded hover:bg-nexus-hover/50"
              onClick={() => setDeleteTplTarget(null)}
            >取消</button>
            <button
              className="px-5 py-1.5 text-[13px] bg-nexus-error text-white rounded hover:bg-nexus-error/80 disabled:opacity-40"
              disabled={deletingTpl}
              onClick={confirmDeleteTemplate}
            >{deletingTpl ? '删除中…' : '确认删除'}</button>
          </div>
        </div>
      </Modal>

      {/* 工具命令执行结果弹窗（输出行由弹窗自己按 runId 订阅 store） */}
      <ToolCommandResultDialog
        open={toolCommandState.open}
        commandName={toolCommandState.commandName}
        runId={toolCommandState.runId || null}
        result={toolCommandState.result}
        error={toolCommandState.error}
        loading={toolCommandState.loading}
        onStop={handleStopToolCommand}
        onClose={handleCloseToolCommand}
      />
    </div>
  );
}

// ── 子组件 ────────────────────────────────────────────────

function EmptyState({ name, path }: { name: string; path: string }) {
  return (
    <div className="flex-1 flex items-center justify-center">
      <div className="text-center space-y-2">
        <span className="text-[48px] opacity-[0.06] select-none font-extralight">N</span>
        <p className="text-[12px] text-nexus-text-muted">{name}</p>
        <p className="text-[13px] text-nexus-muted">{path}</p>
      </div>
    </div>
  );
}

interface ServicePanelProps {
  services: Service[];
  collapsed: boolean;
  onToggle: () => void;
  /** 右侧面板上下分栏配置 */
  splitPanel: SplitPanel;
  editingService: Service | null;
  /** 点击服务卡片（父组件处理互斥，关闭模板编辑面板） */
  onEditService: (svc: Service) => void;
  isServiceRunning: (svc: Service) => boolean;
  isServiceFailed: (svc: Service) => boolean;
  setDeleteSvcTarget: (target: { id: string; name: string } | null) => void;
  /** 新建服务（打开右侧编辑面板，空 id 即新建） */
  onCreateService: () => void;
  handleStartAll: () => void;
  handleStopAll: () => void;
  handleViewLog: (svc: Service) => void;
  handleRunToolCommand: (serviceId: string, commandId: string, commandName: string) => void;
  /** 服务卡片拖拽排序持久化 */
  handleReorderServices: (orderedIds: string[]) => void;
  loading: Record<string, boolean>;
  load: () => void;
}

function ServicePanel({
  services, collapsed, onToggle, splitPanel, editingService, onEditService,
  isServiceRunning, isServiceFailed, setDeleteSvcTarget,
  onCreateService, handleStartAll, handleStopAll, handleViewLog,
  handleRunToolCommand, handleReorderServices, loading, load,
}: ServicePanelProps) {
  return (
    <div className={`absolute right-0 top-0 bottom-0 z-10 flex flex-col flex-shrink-0 overflow-hidden bg-nexus-surface border-l border-nexus-border ${
      collapsed ? 'w-[32px]' : 'w-[360px]'
    }`}>
      {/* 折叠条（32px）：折叠时可见 */}
      <div className={`h-full transition-opacity duration-200 ${collapsed ? 'opacity-100' : 'opacity-0 pointer-events-none'}`}>
        <CollapsedView
          services={services}
          isServiceRunning={isServiceRunning}
          isServiceFailed={isServiceFailed}
          onToggle={onToggle}
          onViewLog={handleViewLog}
          onRunToolCommand={handleRunToolCommand}
          load={load}
        />
      </div>
      {/* 展开内容：折叠时用 transform 向右滑出（GPU 合成，不触发布局 reflow，
          避免日志视图在动画期间每帧重排导致卡顿） */}
      <div className={`absolute left-0 top-0 bottom-0 w-[360px] transition-transform duration-200 ${
        collapsed ? 'translate-x-full' : 'translate-x-0'
      }`}>
        <ExpandedView
          services={services}
          splitPanel={splitPanel}
          editingService={editingService}
          onEditService={onEditService}
          isServiceRunning={isServiceRunning}
          isServiceFailed={isServiceFailed}
          setDeleteSvcTarget={setDeleteSvcTarget}
          onCreateService={onCreateService}
          handleStartAll={handleStartAll}
          handleStopAll={handleStopAll}
          handleViewLog={handleViewLog}
          handleRunToolCommand={handleRunToolCommand}
          handleReorderServices={handleReorderServices}
          loading={loading}
          load={load}
          onToggle={onToggle}
        />
      </div>
    </div>
  );
}

function CollapsedView({
  services, isServiceRunning, isServiceFailed, onToggle, onViewLog, onRunToolCommand, load,
}: {
  services: Service[];
  isServiceRunning: (svc: Service) => boolean;
  isServiceFailed: (svc: Service) => boolean;
  onToggle: () => void;
  /** 收起态直接查看服务日志（不展开列） */
  onViewLog: (svc: Service) => void;
  /** 工具命令：输出弹窗由 ProjectDetail 顶层渲染，与面板收起与否无关 */
  onRunToolCommand: (serviceId: string, commandId: string, commandName: string) => void;
  /** 动作完成后刷新服务列表（与展开态同一个 load：它写的是共享的 svcCacheStore） */
  load: () => void;
}) {
  // 图标亮 = 本项目的 AI 会话活跃（判据见 selectAiRunningHere）；
  // 选中底色只表示「面板展开」：面板收起时会话照跑，此时只亮不选中（同满足两态会像被按下的开关）
  const aiRunning = useAiStore(selectAiRunningHere);
  /** 面板是否真的显示（安装进度会强制撑开面板，与 RestartConfirm 判定同源） */
  const aiPanelVisible = useAiStore(selectAiPanelVisible);
  const toggleAi = () => useAiStore.getState().togglePanel();

  /** 右键菜单指向的服务（null = 菜单没开） */
  const [menu, setMenu] = useState<{ id: string; x: number; y: number } | null>(null);
  // 动作与展开态的服务卡片共用一份实现（见 useServiceActions 的说明）
  const { runAction } = useServiceActions(load);
  // 服务对象与工具绑定都现查、不快照：菜单可能开着好几秒，期间 services 会重新加载
  const menuSvc = menu ? services.find(s => s.id === menu.id) : undefined;
  const boundToolId = useToolStore(s => (menu ? s.bindings[menu.id] : undefined));
  const openTools = useToolStore(s => s.openTools);
  const boundTool = openTools.find(t => t.id === boundToolId);
  const menuCommands = useMemo(
    () => (menuSvc ? parseToolCommands(menuSvc.tool_commands) : []),
    [menuSvc],
  );
  return (
    <div className="flex flex-col h-full w-full">
      {/* 顶部：展开服务列表在上，AI 开关在它下方（收起态依旧可点） */}
      <div className="flex flex-col items-center py-1 gap-0.5 flex-shrink-0 border-b border-nexus-border/40">
        <button
          className="w-6 h-6 flex items-center justify-center rounded-md text-nexus-muted hover:text-nexus-text hover:bg-nexus-hover/40 transition-colors"
          title="展开服务列表"
          onClick={onToggle}
        >
          <PanelToggleIcon size={13} />
        </button>
        <button
          className={'w-6 h-6 flex items-center justify-center rounded-md transition-colors ' + (
            aiPanelVisible ? 'text-nexus-accent bg-nexus-accent/15'
            : aiRunning ? 'text-nexus-accent hover:bg-nexus-hover/40'
            : 'text-nexus-muted hover:bg-nexus-hover/40 hover:text-nexus-text'
          )}
          title={aiRunning ? `AI 助手（运行中）${aiPanelVisible ? ' · 点击隐藏' : ' · 点击显示'}` : 'AI 助手（未运行）'}
          onClick={toggleAi}
        >
          <RobotIcon size={13} />
        </button>
      </div>

      {/* 服务日志入口：运行/失败的服务点击直接打开日志面板（无需展开列）。
          状态点同时充当运行指示与可点区域 */}
      <div className="flex-1 flex flex-col items-center gap-1 overflow-y-auto py-2">
        {services.map(svc => {
          const running = isServiceRunning(svc);
          const failed = isServiceFailed(svc);
          const clickable = running || failed; // 停止的服务无日志可看
          return (
            <button
              key={svc.id}
              // 与展开态的服务卡片同一个属性名：两处都靠它标识"这一格是哪个服务"
              data-service-id={svc.id}
              disabled={!clickable}
              onClick={() => onViewLog(svc)}
              // 右键菜单与展开态的服务卡片同一份（ServiceContextMenu 的前几组）：
              // 未运行的服务在这里连点击都是 disabled，没有右键就等于"收起后完全够不着"
              onContextMenu={e => {
                e.preventDefault();
                e.stopPropagation();
                setMenu({ id: svc.id, x: e.clientX, y: e.clientY });
              }}
              className={`w-6 h-6 rounded-md flex items-center justify-center flex-shrink-0 transition-colors ${
                clickable ? 'hover:bg-nexus-accent/20 cursor-pointer' : 'cursor-default'
              }`}
              title={`${svc.name}${running ? '（运行中）' : failed ? '（失败）' : '（未运行）'}${clickable ? ' · 点击查看日志' : ''}
右键：启停 / 重启、工具、打开位置`}
            >
              <span className={`w-[8px] h-[8px] rounded-full flex-shrink-0 ${
                running ? 'bg-nexus-success' : failed ? 'bg-nexus-error' : 'bg-nexus-muted/25'
              }`} />
            </button>
          );
        })}
      </div>

      {/* 右键菜单：与展开态的卡片共用同一份，唯独**不传 onDelete**——收起态只有 24px 圆点，
          删除这种破坏性操作挂在这里误触代价太高（其余条目都保留：收起态恰恰是最需要它们的场景）。
          Portal 到 body：服务面板有 transform 容器（展开内容用 translate-x 滑出），
          fixed 定位的菜单留在其内会以它为包含块，坐标错位被裁剪而不可见 */}
      {menu && menuSvc && createPortal(
        <ServiceContextMenu
          x={menu.x}
          y={menu.y}
          cwd={menuSvc.cwd}
          running={isServiceRunning(menuSvc)}
          failed={isServiceFailed(menuSvc)}
          openToolName={boundTool?.name ?? null}
          toolCommands={menuCommands}
          onViewLog={() => onViewLog(menuSvc)}
          onStart={() => void runAction(menuSvc, 'start')}
          onStop={() => void runAction(menuSvc, 'stop')}
          onRestart={() => void runAction(menuSvc, 'restart')}
          onOpenWithTool={() => { void openToolsApi.openWith(menuSvc.id).catch(e => reportError('用工具打开失败', e)); }}
          onOpenInExplorer={() => void openInExplorer(menuSvc.cwd)}
          onOpenTerminal={() => void openTerminal(menuSvc.cwd)}
          onRunCommand={cmd => onRunToolCommand(menuSvc.id, cmd.id, cmd.name)}
          onClose={() => setMenu(null)}
        />,
        document.body,
      )}
    </div>
  );
}

/** 右侧面板上下分栏配置：bottom 为下半区模板库内容，topHeight 为上半区（项目服务）高度 */
interface SplitPanel {
  bottom: React.ReactNode;
  topHeight: number;
  topMaxHeight: number;
  onResize: (h: number) => void;
}

interface ExpandedViewProps {
  services: Service[];
  splitPanel: SplitPanel;
  editingService: Service | null;
  /** 点击服务卡片（父组件处理互斥，关闭模板编辑面板） */
  onEditService: (svc: Service) => void;
  isServiceRunning: (svc: Service) => boolean;
  isServiceFailed: (svc: Service) => boolean;
  setDeleteSvcTarget: (target: { id: string; name: string } | null) => void;
  /** 新建服务（打开右侧编辑面板，空 id 即新建） */
  onCreateService: () => void;
  handleStartAll: () => void;
  handleStopAll: () => void;
  handleViewLog: (svc: Service) => void;
  handleRunToolCommand: (serviceId: string, commandId: string, commandName: string) => void;
  /** 服务卡片拖拽排序持久化 */
  handleReorderServices: (orderedIds: string[]) => void;
  loading: Record<string, boolean>;
  load: () => void;
  onToggle: () => void;
}

function ExpandedView({
  services, splitPanel, editingService, onEditService, isServiceRunning, isServiceFailed,
  setDeleteSvcTarget, onCreateService,
  handleStartAll, handleStopAll, handleViewLog, handleRunToolCommand,
  handleReorderServices,
  loading, load, onToggle,
}: ExpandedViewProps) {
  return (
    <ResizablePanel
      direction="vertical"
      left={
        <ServiceSection
          services={services}
          editingService={editingService}
          onEditService={onEditService}
          isServiceRunning={isServiceRunning}
          isServiceFailed={isServiceFailed}
          setDeleteSvcTarget={setDeleteSvcTarget}
          onCreateService={onCreateService}
          handleStartAll={handleStartAll}
          handleStopAll={handleStopAll}
          handleViewLog={handleViewLog}
          handleRunToolCommand={handleRunToolCommand}
          handleReorderServices={handleReorderServices}
          loading={loading}
          load={load}
          onToggle={onToggle}
        />
      }
      right={splitPanel.bottom}
      defaultLeftWidth={splitPanel.topHeight}
      minWidth={96}
      maxWidth={splitPanel.topMaxHeight}
      onResize={splitPanel.onResize}
    />
  );
}

interface ServiceSectionProps {
  services: Service[];
  editingService: Service | null;
  /** 点击服务卡片（父组件处理互斥，关闭模板编辑面板） */
  onEditService: (svc: Service) => void;
  isServiceRunning: (svc: Service) => boolean;
  isServiceFailed: (svc: Service) => boolean;
  setDeleteSvcTarget: (target: { id: string; name: string } | null) => void;
  /** 新建服务（打开右侧编辑面板，空 id 即新建） */
  onCreateService: () => void;
  handleStartAll: () => void;
  handleStopAll: () => void;
  handleViewLog: (svc: Service) => void;
  handleRunToolCommand: (serviceId: string, commandId: string, commandName: string) => void;
  /** 服务卡片拖拽排序持久化 */
  handleReorderServices: (orderedIds: string[]) => void;
  loading: Record<string, boolean>;
  load: () => void;
  onToggle: () => void;
}

function ServiceSection({
  services, editingService, onEditService,
  isServiceRunning, isServiceFailed,
  setDeleteSvcTarget, onCreateService,
  handleStartAll, handleStopAll, handleViewLog, handleRunToolCommand,
  handleReorderServices, loading, load, onToggle,
}: ServiceSectionProps) {
  // 图标亮 = 本项目的 AI 会话活跃（进程归属判别见 CollapsedView 说明）；
  // 选中底色只在面板展开时给，收起时会话照跑但只亮不选中
  const aiRunning = useAiStore(selectAiRunningHere);
  const aiPanelVisible = useAiStore(selectAiPanelVisible);
  const toggleAi = () => useAiStore.getState().togglePanel();
  // dnd-kit 拖拽排序：长按 250ms 激活（delay 期间移动超过 5px 则取消，视为普通点击），
  // 快速点击照常打开编辑面板
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { delay: 250, tolerance: 5 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );
  const handleDragEnd = useCallback((event: DragEndEvent) => {
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const oldIndex = services.findIndex(s => s.id === active.id);
    const newIndex = services.findIndex(s => s.id === over.id);
    if (oldIndex === -1 || newIndex === -1) return;
    handleReorderServices(arrayMove(services, oldIndex, newIndex).map(s => s.id));
  }, [services, handleReorderServices]);
  return (
    <div className="flex flex-col h-full">
      {/* 头部 */}
      <div className="flex items-center justify-between px-4 h-[42px] border-b border-nexus-border flex-shrink-0">
        <div className="flex items-center gap-2 min-w-0">
          <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.3" className="text-nexus-muted flex-shrink-0">
            <rect x="1.5" y="1.5" width="11" height="11" rx="2"/><line x1="5" y1="5" x2="9" y2="5"/><line x1="5" y1="7" x2="9" y2="7"/><line x1="5" y1="9" x2="7" y2="9"/>
          </svg>
          <span className="text-[13px] text-nexus-text font-medium truncate">项目服务</span>
        </div>
        <div className="flex items-center gap-1">
          <button
            className={'p-1.5 rounded-md flex-shrink-0 transition-colors ' + (
              aiPanelVisible ? 'text-nexus-accent bg-nexus-accent/15'
              : aiRunning ? 'text-nexus-accent hover:bg-nexus-hover/50'
              : 'text-nexus-muted hover:text-nexus-text hover:bg-nexus-hover/50'
            )}
            title={aiRunning ? `AI 助手（运行中）${aiPanelVisible ? ' · 点击隐藏' : ' · 点击显示'}` : 'AI 助手（未运行）'}
            onClick={toggleAi}
          >
            <RobotIcon size={15} />
          </button>
          <button
            className="p-1.5 text-nexus-muted hover:text-nexus-text rounded-md hover:bg-nexus-hover/50 flex-shrink-0"
            title="添加服务"
            onClick={onCreateService}
          >
            <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.5">
              <line x1="7" y1="2" x2="7" y2="12"/><line x1="2" y1="7" x2="12" y2="7"/>
            </svg>
          </button>
          <button
            className="p-1.5 text-nexus-muted hover:text-nexus-text rounded-md hover:bg-nexus-hover/50 flex-shrink-0"
            title="收起服务列表"
            onClick={onToggle}
          >
            <PanelToggleIcon />
          </button>
        </div>
      </div>

      {/* 服务列表（dnd-kit 拖拽排序：拖手柄排序，整卡点击编辑） */}
      <div className="flex-1 overflow-auto py-1">
        {services.length === 0 && (
          <div className="flex flex-col items-center justify-center py-16 px-4 text-center">
            <span className="text-[24px] opacity-[0.08] select-none font-extralight mb-2">S</span>
            <span className="text-[12px] text-nexus-muted mb-3">暂无服务</span>
            <button
              className="px-4 py-1.5 text-[12px] bg-nexus-accent text-white rounded-md hover:bg-nexus-accent-hover"
              onClick={onCreateService}
            >添加服务</button>
          </div>
        )}
        <DndContext sensors={sensors} onDragEnd={handleDragEnd}>
          <SortableContext items={services.map(s => s.id)} strategy={verticalListSortingStrategy}>
            {services.map(svc => (
              <ServiceTreeEntry
                key={svc.id}
                service={svc}
                running={isServiceRunning(svc)}
                failed={isServiceFailed(svc)}
                isEditing={editingService?.id === svc.id}
                onEdit={() => onEditService(svc)}
                onRefresh={load}
                onContextMenu={(id, name) => setDeleteSvcTarget({ id, name })}
                onViewLog={() => handleViewLog(svc)}
                onRunToolCommand={handleRunToolCommand}
              />
            ))}
          </SortableContext>
        </DndContext>
      </div>

      {/* 底部操作 */}
      <div className="flex items-center gap-2 px-4 py-2.5 border-t border-nexus-border flex-shrink-0">
        <button
          className="flex-1 px-3 py-1.5 text-[12px] bg-nexus-success/15 text-nexus-success rounded-md hover:bg-nexus-success/25 disabled:opacity-40 font-medium"
          disabled={loading.__all__ || services.length === 0}
          onClick={handleStartAll}
        >▶ 全部启动</button>
        <button
          className="flex-1 px-3 py-1.5 text-[12px] bg-nexus-error/15 text-nexus-error rounded-md hover:bg-nexus-error/25 disabled:opacity-40 font-medium"
          disabled={services.length === 0}
          onClick={handleStopAll}
        >■ 全部停止</button>
      </div>
    </div>
  );
}

// ── 服务模板库区块（右侧面板下半区） ───────────────────────

interface TemplateSectionProps {
  templates: ServiceTemplate[];
  busy: boolean;
  /** 正在编辑的模板 ID（用于卡片高亮） */
  editingId: string | null;
  /** 点击模板卡片：打开模板编辑面板 */
  onEdit: (tpl: ServiceTemplate) => void;
  onAdd: (tpl: ServiceTemplate) => void;
  /** 请求删除（父组件弹确认框） */
  onRequestDelete: (tpl: ServiceTemplate) => void;
  /** 模板卡片拖拽排序持久化 */
  onReorderTemplates: (orderedIds: string[]) => void;
  /** 新建空白模板（打开编辑面板，保存时由编辑面板走 create） */
  onCreate: () => void;
  /** 从 JSON 文件导入（重名自动改名） */
  onImport: () => void;
  /** 导出全部模板 */
  onExportAll: () => void;
  /** 导出单个模板（右键菜单） */
  onExportOne: (tpl: ServiceTemplate) => void;
}

function TemplateSection({ templates, busy, editingId, onEdit, onAdd, onRequestDelete, onReorderTemplates, onCreate, onImport, onExportAll, onExportOne }: TemplateSectionProps) {
  // dnd-kit 拖拽排序：长按 250ms 激活（delay 期间移动超过 5px 则取消，视为普通点击），
  // 快速点击照常打开编辑面板
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { delay: 250, tolerance: 5 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );
  const handleDragEnd = useCallback((event: DragEndEvent) => {
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const oldIndex = templates.findIndex(t => t.id === active.id);
    const newIndex = templates.findIndex(t => t.id === over.id);
    if (oldIndex === -1 || newIndex === -1) return;
    onReorderTemplates(arrayMove(templates, oldIndex, newIndex).map(t => t.id));
  }, [templates, onReorderTemplates]);
  return (
    <div className="flex flex-col h-full">
      {/* 头部 */}
      <div className="flex items-center gap-2 px-4 h-[42px] border-b border-nexus-border flex-shrink-0">
        <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.3" className="text-nexus-muted flex-shrink-0">
          <path d="M7 1.5l1.4 2.9 3.1.4-2.3 2.2.6 3.1L7 8.8l-2.8 1.3.6-3.1L2.5 4.8l3.1-.4L7 1.5z"/>
        </svg>
        <span className="text-[13px] text-nexus-text font-medium truncate min-w-0">服务模板库</span>
        {/* 新建 / 导入 / 导出三个入口都放标题栏：它们是同一层级的**库级**操作，
            混进列表里反而难找（单个模板的导出在它自己的右键菜单里）。

            尺寸与「项目服务」的「+」严格对齐（p-1.5 / rounded-md / 14px 图标 / 描边 1.5）：
            两块面板上下相邻，图标差 1px、内边距差 0.5 都看得出来 */}
        <div className="ml-auto flex items-center gap-1 flex-shrink-0">
          <button
            className="p-1.5 text-nexus-muted hover:text-nexus-text rounded-md hover:bg-nexus-hover/50 flex-shrink-0 transition-colors disabled:opacity-30 disabled:hover:bg-transparent"
            title="新建模板"
            onClick={onCreate}
          >
            <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.5">
              <line x1="7" y1="2" x2="7" y2="12" /><line x1="2" y1="7" x2="12" y2="7" />
            </svg>
          </button>
          <button
            className="p-1.5 text-nexus-muted hover:text-nexus-text rounded-md hover:bg-nexus-hover/50 flex-shrink-0 transition-colors disabled:opacity-30 disabled:hover:bg-transparent"
            title="从文件导入模板（重名自动改名）"
            onClick={onImport}
          >
            <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
              <path d="M7 2v7" /><polyline points="4,6.5 7,9.5 10,6.5" /><line x1="2.5" y1="12" x2="11.5" y2="12" />
            </svg>
          </button>
          <button
            className="p-1.5 text-nexus-muted hover:text-nexus-text rounded-md hover:bg-nexus-hover/50 flex-shrink-0 transition-colors disabled:opacity-30 disabled:hover:bg-transparent"
            title="导出全部模板"
            onClick={onExportAll}
            disabled={templates.length === 0}
          >
            <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
              <path d="M7 10V3" /><polyline points="4,5.5 7,2.5 10,5.5" /><line x1="2.5" y1="12" x2="11.5" y2="12" />
            </svg>
          </button>
        </div>
      </div>

      {/* 模板列表（dnd-kit 拖拽排序：拖手柄排序，整卡点击编辑） */}
      <div className="flex-1 overflow-auto py-1">
        {templates.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-10 px-4 text-center">
            <span className="text-[24px] opacity-[0.08] select-none font-extralight mb-2">T</span>
            <span className="text-[12px] text-nexus-muted mb-1">暂无模板</span>
            <span className="text-[11px] text-nexus-muted/60">点标题栏的「+」新建，或编辑服务时点「另存为模板」</span>
          </div>
        ) : (
          <DndContext sensors={sensors} onDragEnd={handleDragEnd}>
            <SortableContext items={templates.map(t => t.id)} strategy={verticalListSortingStrategy}>
              {templates.map(tpl => (
                <TemplateTreeEntry
                  key={tpl.id}
                  tpl={tpl}
                  busy={busy}
                  isEditing={tpl.id === editingId}
                  onEdit={onEdit}
                  onAdd={onAdd}
                  onRequestDelete={onRequestDelete}
                  onExport={onExportOne}
                />
              ))}
            </SortableContext>
          </DndContext>
        )}
      </div>
    </div>
  );
}
