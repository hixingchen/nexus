import { useState, useCallback, useEffect, useRef } from 'react';
import { listen } from '@tauri-apps/api/event';
import { EditorTabs } from '../editor/EditorTabs';
import { CodeViewer } from '../editor/CodeViewer';
import { ImageViewer } from '../editor/ImageViewer';
import { HexViewer } from '../editor/HexViewer';
import { JarViewer } from '../editor/JarViewer';
import { LogViewer } from '../terminal/LogViewer';
import { Modal } from '../ui/Modal';
import { ToolCommandResultDialog } from '../ui/ToolCommandResultDialog';
import { ResizablePanel } from './ResizablePanel';
import { ServiceTreeEntry } from './ServiceTreeEntry';
import { TemplateTreeEntry } from './TemplateTreeEntry';
import { SearchResultPanel } from './SearchResultPanel';
import { AddServiceFormContent } from './AddServiceFormContent';
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
import { processApi, serviceApi, layoutApi, type ToolCommandResult, type ToolCommandLogPayload, type Service, type ServiceTemplate } from '../../services/service';
import { showNotification } from '../ui/Toast';

// 工具命令输出最多保留的行数（对齐服务日志上限，防止超长输出卡顿）
const MAX_TOOL_CMD_LOG_LINES = 2000;

interface Props {
  projectId: string;
  servicePanelCollapsed: boolean;
  onToggleServicePanel: () => void;
}

export function ProjectDetail({ projectId, servicePanelCollapsed, onToggleServicePanel }: Props) {
  const {
    detail, loading, editingService, setEditingService,
    showAddServiceModal, setShowAddServiceModal,
    deleteSvcTarget, setDeleteSvcTarget, deleting,
    viewingLog, setViewingLog,
    activeTab, fileContent, load, reorderServicesLocal,
    isServiceRunning, isServiceFailed, handleStartAll, handleStopAll,
    handleDeleteService, handleViewLog,
  } = useProjectDetail(projectId);

  // 工具命令执行状态（logs 为执行中的实时输出行数组，结束后由 result 兜底）
  const [toolCommandState, setToolCommandState] = useState<{
    open: boolean;
    loading: boolean;
    commandName: string;
    result: ToolCommandResult | null;
    logs: string[];
  }>({ open: false, loading: false, commandName: '', result: null, logs: [] });

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
    serviceApi.getServiceTemplates().then(setTemplates).catch(e => console.error('加载模板失败:', e));
  }, []);

  useEffect(() => {
    loadTemplates();
    layoutApi.load().then(l => {
      if (l.right_panel_top_height) setTopPanelHeight(Number(l.right_panel_top_height));
    }).catch(() => {});
  }, [loadTemplates]);

  // 防抖保存上半区高度（对齐 MainLayout 的布局保存模式）
  const topHeightTimer = useRef<ReturnType<typeof setTimeout>>();
  const persistTopPanelHeight = useCallback((h: number) => {
    clearTimeout(topHeightTimer.current);
    topHeightTimer.current = setTimeout(() => {
      layoutApi.save({ right_panel_top_height: String(h) }).catch(e => console.error('保存布局失败:', e));
    }, 500);
  }, []);
  useEffect(() => () => clearTimeout(topHeightTimer.current), []);

  // 拖拽排序持久化：本地立即重排（回弹动画结束后 UI 无缝衔接，不等后端重拉），
  // 异步写入后端；失败才重载恢复后端顺序
  const handleReorderServices = useCallback((orderedIds: string[]) => {
    const pid = detail?.project?.id;
    if (!pid) return;
    reorderServicesLocal(orderedIds);
    serviceApi.reorderServices(pid, orderedIds).catch(e => {
      showNotification({ variant: 'error', title: '保存服务排序失败', description: String(e) });
      load();
    });
  }, [detail?.project?.id, reorderServicesLocal, load]);

  const handleReorderTemplates = useCallback((orderedIds: string[]) => {
    setTemplates(prev => {
      const byId = new Map(prev.map(t => [t.id, t]));
      return orderedIds.map(id => byId.get(id)).filter((t): t is ServiceTemplate => !!t);
    });
    serviceApi.reorderServiceTemplates(orderedIds).catch(e => {
      showNotification({ variant: 'error', title: '保存模板排序失败', description: String(e) });
      loadTemplates();
    });
  }, [loadTemplates]);

  // 执行工具命令（流式：先订阅 tool-command-log 事件实时追加，结束再取完整结果）
  const handleRunToolCommand = async (serviceId: string, commandId: string, commandName: string) => {
    const runId = crypto.randomUUID();
    setToolCommandState({ open: true, loading: true, commandName, result: null, logs: [] });
    const unlisten = await listen<ToolCommandLogPayload>('tool-command-log', event => {
      if (event.payload.run_id !== runId) return;
      setToolCommandState(prev => {
        const logs = [...prev.logs, event.payload.data];
        if (logs.length > MAX_TOOL_CMD_LOG_LINES) logs.splice(0, logs.length - MAX_TOOL_CMD_LOG_LINES);
        return { ...prev, logs };
      });
    });
    try {
      const result = await processApi.runToolCommand(serviceId, commandId, runId);
      // 以完整结果兜底（含按序号合并的顺序，避免事件流微乱序；同为最新 N 行）
      const logs = result.output.split('\n').slice(-MAX_TOOL_CMD_LOG_LINES);
      setToolCommandState(prev => ({ ...prev, loading: false, result, logs }));
    } catch (err) {
      console.error('执行工具命令失败:', err);
      showNotification({ variant: 'error', title: '执行工具命令失败', description: String(err) });
      setToolCommandState(prev => ({ ...prev, loading: false }));
    } finally {
      unlisten();
    }
  };

  // 从模板添加到当前项目（值拷贝，模板不受影响）
  const handleAddTemplate = async (tpl: ServiceTemplate) => {
    setAddingTemplate(true);
    try {
      await serviceApi.addServiceFromTemplate(projectId, tpl.id);
      showNotification({ title: `已从模板添加「${tpl.name}」` });
      await load();
    } catch (e: unknown) {
      showNotification({ variant: 'error', title: '添加服务失败', description: String(e) });
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
      showNotification({ variant: 'error', title: '删除模板失败', description: String(err) });
    }
    setDeletingTpl(false);
    setDeleteTplTarget(null);
  }, [deleteTplTarget, editingTemplate, loadTemplates]);

  // 打开服务/模板编辑面板（互斥：同一时间只开一个）
  const openServiceEdit = useCallback((svc: Service) => {
    setEditingTemplate(null);
    setEditingService(prev => prev?.id === svc.id ? null : svc);
  }, []);
  // 与服务卡片一致：再次点击正在编辑的模板 → 关闭面板
  const openTemplateEdit = useCallback((tpl: ServiceTemplate) => {
    setEditingService(null);
    setEditingTemplate(prev => prev?.id === tpl.id ? null : tpl);
  }, []);

  if (!detail) {
    return (
      <div className="flex items-center justify-center h-full text-[12px] text-nexus-muted">
        加载中…
      </div>
    );
  }

  const { project, services } = detail;

  return (
    <div className="h-full bg-nexus-editor flex relative overflow-hidden">
      {/* 主区域：代码查看器 / 空状态。
          padding 补偿服务列宽度（瞬时变化，日志只 reflow 一次而非动画期间每帧 reflow） */}
      <div className={`flex-1 flex flex-col overflow-hidden relative ${servicePanelCollapsed ? 'pr-[32px]' : 'pr-[360px]'}`}>
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
              ) : (
                <CodeViewer
                  filePath={activeTab.path}
                  content={fileContent ?? ''}
                  editable={!activeTab.readonly}
                  onChange={(content) => useEditorStore.getState().updateDraft(content)}
                />
              )}
            </div>
          </>
        ) : (
          <EmptyState name={project.name} path={project.path} />
        )}

        {editingService && (
          <ServiceEditPanel
            key={editingService.id}
            service={editingService}
            onSave={async () => { await load(); setEditingService(null); }}
            onSavedAsTemplate={loadTemplates}
            // 面板右侧偏移 = 服务列宽度（服务列 absolute 覆盖，编辑面板需显示在其左侧）
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
            rightOffset={servicePanelCollapsed ? 32 : 360}
          />
        )}

        {/* 底部搜索结果面板（编辑器下方，打开时挤压编辑器高度） */}
        <SearchResultPanel />
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
        setShowAddServiceModal={setShowAddServiceModal}
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

      {/* Modals */}
      <Modal open={showAddServiceModal} title="添加服务" onClose={() => setShowAddServiceModal(false)}>
        <AddServiceFormContent
          projectId={project.id}
          projectPath={project.path}
          onDone={() => { setShowAddServiceModal(false); load(); }}
        />
      </Modal>

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

      {/* 工具命令执行结果弹窗 */}
      <ToolCommandResultDialog
        open={toolCommandState.open}
        commandName={toolCommandState.commandName}
        result={toolCommandState.result}
        logs={toolCommandState.logs}
        loading={toolCommandState.loading}
        onClose={() => setToolCommandState(prev => ({ ...prev, open: false }))}
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
  setShowAddServiceModal: (show: boolean) => void;
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
  setShowAddServiceModal, handleStartAll, handleStopAll, handleViewLog,
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
          setShowAddServiceModal={setShowAddServiceModal}
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
  services, isServiceRunning, isServiceFailed, onToggle,
}: {
  services: Service[];
  isServiceRunning: (svc: Service) => boolean;
  isServiceFailed: (svc: Service) => boolean;
  onToggle: () => void;
}) {
  return (
    <button
      className="flex flex-col items-center h-full w-full cursor-pointer hover:bg-nexus-hover/30 transition-colors"
      title="展开服务列表"
      onClick={onToggle}
    >
      <div className="flex-1 flex flex-col items-center justify-center gap-1.5">
        {services.map(svc => (
          <span key={svc.id}
            className={`w-[6px] h-[6px] rounded-full flex-shrink-0 ${
              isServiceRunning(svc) ? 'bg-nexus-success' : isServiceFailed(svc) ? 'bg-nexus-error' : 'bg-nexus-muted/30'
            }`}
            title={`${svc.name}${isServiceRunning(svc) ? ' (运行中)' : isServiceFailed(svc) ? ' (失败)' : ''}`}
          />
        ))}
      </div>
    </button>
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
  setShowAddServiceModal: (show: boolean) => void;
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
  setDeleteSvcTarget, setShowAddServiceModal,
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
          setShowAddServiceModal={setShowAddServiceModal}
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
  setShowAddServiceModal: (show: boolean) => void;
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
  services, editingService, onEditService, isServiceRunning, isServiceFailed,
  setDeleteSvcTarget, setShowAddServiceModal,
  handleStartAll, handleStopAll, handleViewLog, handleRunToolCommand,
  handleReorderServices, loading, load, onToggle,
}: ServiceSectionProps) {
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
            className="p-1.5 text-nexus-muted hover:text-nexus-text rounded-md hover:bg-nexus-hover/50 flex-shrink-0"
            title="添加服务"
            onClick={() => setShowAddServiceModal(true)}
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
            <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
              <polyline points="5,2 10,7 5,12" />
            </svg>
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
              onClick={() => setShowAddServiceModal(true)}
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
}

function TemplateSection({ templates, busy, editingId, onEdit, onAdd, onRequestDelete, onReorderTemplates }: TemplateSectionProps) {
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
        <span className="text-[13px] text-nexus-text font-medium truncate">服务模板库</span>
      </div>

      {/* 模板列表（dnd-kit 拖拽排序：拖手柄排序，整卡点击编辑） */}
      <div className="flex-1 overflow-auto py-1">
        {templates.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-10 px-4 text-center">
            <span className="text-[24px] opacity-[0.08] select-none font-extralight mb-2">T</span>
            <span className="text-[12px] text-nexus-muted mb-1">暂无模板</span>
            <span className="text-[11px] text-nexus-muted/60">编辑服务时点「另存为模板」创建</span>
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
                />
              ))}
            </SortableContext>
          </DndContext>
        )}
      </div>
    </div>
  );
}
