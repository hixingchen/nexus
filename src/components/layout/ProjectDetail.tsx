import { useState } from 'react';
import { EditorTabs } from '../editor/EditorTabs';
import { CodeViewer } from '../editor/CodeViewer';
import { LogViewer } from '../terminal/LogViewer';
import { Modal } from '../ui/Modal';
import { ToolCommandResultDialog } from '../ui/ToolCommandResultDialog';
import { ServiceTreeEntry } from './ServiceTreeEntry';
import { AddServiceFormContent } from './AddServiceFormContent';
import { ServiceEditPanel } from './ServiceEditPanel';
import { useProjectDetail } from '../../hooks/useProjectDetail';
import { processApi, type ToolCommandResult } from '../../services/service';
import { showNotification } from '../ui/Toast';
import type { Service } from '../../services/service';

interface Props {
  projectId: string;
  servicePanelCollapsed: boolean;
  onToggleServicePanel: () => void;
  onProjectStart?: () => void;
}

export function ProjectDetail({ projectId, servicePanelCollapsed, onToggleServicePanel, onProjectStart }: Props) {
  const {
    detail, loading, editingService, setEditingService,
    showAddServiceModal, setShowAddServiceModal,
    deleteSvcTarget, setDeleteSvcTarget, deleting,
    svcCtxMenu, setSvcCtxMenu, viewingLog, setViewingLog,
    activeTab, fileContent, load,
    isServiceRunning, handleStartAll, handleStopAll,
    handleDeleteService, handleViewLog,
  } = useProjectDetail(projectId, onProjectStart);

  // 工具命令执行状态
  const [toolCommandState, setToolCommandState] = useState<{
    open: boolean;
    loading: boolean;
    commandName: string;
    result: ToolCommandResult | null;
  }>({ open: false, loading: false, commandName: '', result: null });

  // 执行工具命令
  const handleRunToolCommand = async (serviceId: string, commandId: string, commandName: string) => {
    setToolCommandState({ open: true, loading: true, commandName, result: null });
    try {
      const result = await processApi.runToolCommand(serviceId, commandId);
      setToolCommandState(prev => ({ ...prev, loading: false, result }));
    } catch (err) {
      console.error('执行工具命令失败:', err);
      showNotification({ variant: 'error', title: '执行工具命令失败', description: String(err) });
      setToolCommandState(prev => ({ ...prev, loading: false }));
    }
  };

  if (!detail) {
    return (
      <div className="flex items-center justify-center h-full text-[12px] text-nexus-muted">
        加载中…
      </div>
    );
  }

  const { project, services } = detail;

  return (
    <div className="h-full bg-nexus-editor flex">
      {/* 主区域：代码查看器 / 空状态 */}
      <div className="flex-1 flex flex-col overflow-hidden relative">
        {viewingLog ? (
          <LogViewer serviceKey={viewingLog} fill onClose={() => setViewingLog(null)} />
        ) : activeTab ? (
          <>
            <EditorTabs />
            <div className="flex-1 overflow-hidden">
              <CodeViewer filePath={activeTab.path} content={fileContent ?? ''} />
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
          />
        )}
      </div>

      {/* 右侧面板：服务列表（可收缩） */}
      <ServicePanel
        services={services}
        collapsed={servicePanelCollapsed}
        onToggle={onToggleServicePanel}
        editingService={editingService}
        setEditingService={setEditingService}
        isServiceRunning={isServiceRunning}
        svcCtxMenu={svcCtxMenu}
        setSvcCtxMenu={setSvcCtxMenu}
        setDeleteSvcTarget={setDeleteSvcTarget}
        setShowAddServiceModal={setShowAddServiceModal}
        handleStartAll={handleStartAll}
        handleStopAll={handleStopAll}
        handleViewLog={(svc) => {
          handleViewLog(svc);
          setEditingService(null);
        }}
        handleRunToolCommand={handleRunToolCommand}
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

      {/* 工具命令执行结果弹窗 */}
      <ToolCommandResultDialog
        open={toolCommandState.open}
        commandName={toolCommandState.commandName}
        result={toolCommandState.result}
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
  editingService: Service | null;
  setEditingService: React.Dispatch<React.SetStateAction<Service | null>>;
  isServiceRunning: (svc: Service) => boolean;
  svcCtxMenu: { id: string; name: string; x: number; y: number } | null;
  setSvcCtxMenu: (menu: { id: string; name: string; x: number; y: number } | null) => void;
  setDeleteSvcTarget: (target: { id: string; name: string } | null) => void;
  setShowAddServiceModal: (show: boolean) => void;
  handleStartAll: () => void;
  handleStopAll: () => void;
  handleViewLog: (svc: Service) => void;
  handleRunToolCommand: (serviceId: string, commandId: string, commandName: string) => void;
  loading: Record<string, boolean>;
  load: () => void;
}

function ServicePanel({
  services, collapsed, onToggle, editingService, setEditingService,
  isServiceRunning, svcCtxMenu, setSvcCtxMenu, setDeleteSvcTarget,
  setShowAddServiceModal, handleStartAll, handleStopAll, handleViewLog,
  handleRunToolCommand, loading, load,
}: ServicePanelProps) {
  return (
    <div className={`bg-nexus-surface border-l border-nexus-border flex flex-col flex-shrink-0 overflow-hidden transition-all duration-200 ${
      collapsed ? 'w-[32px]' : 'w-[360px]'
    }`}>
      {collapsed ? (
        <CollapsedView
          services={services}
          isServiceRunning={isServiceRunning}
          onToggle={onToggle}
        />
      ) : (
        <ExpandedView
          services={services}
          editingService={editingService}
          setEditingService={setEditingService}
          isServiceRunning={isServiceRunning}
          svcCtxMenu={svcCtxMenu}
          setSvcCtxMenu={setSvcCtxMenu}
          setDeleteSvcTarget={setDeleteSvcTarget}
          setShowAddServiceModal={setShowAddServiceModal}
          handleStartAll={handleStartAll}
          handleStopAll={handleStopAll}
          handleViewLog={handleViewLog}
          handleRunToolCommand={handleRunToolCommand}
          loading={loading}
          load={load}
          onToggle={onToggle}
        />
      )}
    </div>
  );
}

function CollapsedView({
  services, isServiceRunning, onToggle,
}: {
  services: Service[];
  isServiceRunning: (svc: Service) => boolean;
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
              isServiceRunning(svc) ? 'bg-nexus-success' : 'bg-nexus-muted/30'
            }`}
            title={`${svc.name}${isServiceRunning(svc) ? ' (运行中)' : ''}`}
          />
        ))}
      </div>
    </button>
  );
}

interface ExpandedViewProps {
  services: Service[];
  editingService: Service | null;
  setEditingService: React.Dispatch<React.SetStateAction<Service | null>>;
  isServiceRunning: (svc: Service) => boolean;
  svcCtxMenu: { id: string; name: string; x: number; y: number } | null;
  setSvcCtxMenu: (menu: { id: string; name: string; x: number; y: number } | null) => void;
  setDeleteSvcTarget: (target: { id: string; name: string } | null) => void;
  setShowAddServiceModal: (show: boolean) => void;
  handleStartAll: () => void;
  handleStopAll: () => void;
  handleViewLog: (svc: Service) => void;
  handleRunToolCommand: (serviceId: string, commandId: string, commandName: string) => void;
  loading: Record<string, boolean>;
  load: () => void;
  onToggle: () => void;
}

function ExpandedView({
  services, editingService, setEditingService, isServiceRunning,
  svcCtxMenu, setSvcCtxMenu, setDeleteSvcTarget, setShowAddServiceModal,
  handleStartAll, handleStopAll, handleViewLog, handleRunToolCommand,
  loading, load, onToggle,
}: ExpandedViewProps) {
  return (
    <>
      {/* 头部 */}
      <div className="flex items-center justify-between px-4 h-[42px] border-b border-nexus-border flex-shrink-0">
        <div className="flex items-center gap-2 min-w-0">
          <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.3" className="text-nexus-muted flex-shrink-0">
            <rect x="1.5" y="1.5" width="11" height="11" rx="2"/><line x1="5" y1="5" x2="9" y2="5"/><line x1="5" y1="7" x2="9" y2="7"/><line x1="5" y1="9" x2="7" y2="9"/>
          </svg>
          <span className="text-[13px] text-nexus-text font-medium truncate">服务</span>
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
            onClick={() => { onToggle(); setEditingService(null); }}
          >
            <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
              <polyline points="5,2 10,7 5,12" />
            </svg>
          </button>
        </div>
      </div>

      {/* 服务列表 */}
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
        {services.map(svc => (
          <ServiceTreeEntry
            key={svc.id}
            service={svc}
            running={isServiceRunning(svc)}
            isEditing={editingService?.id === svc.id}
            onEdit={() => setEditingService(prev => prev?.id === svc.id ? null : svc)}
            onRefresh={load}
            onContextMenu={(e, id, name) => setSvcCtxMenu({ id, name, x: e.clientX, y: e.clientY })}
            onViewLog={() => handleViewLog(svc)}
            onRunToolCommand={handleRunToolCommand}
          />
        ))}
      </div>

      {/* 服务右键菜单 */}
      {svcCtxMenu && (
        <div
          className="fixed z-[70] min-w-[120px] bg-nexus-surface border border-nexus-border rounded-md shadow-xl py-1"
          style={{ left: svcCtxMenu.x, top: svcCtxMenu.y }}
        >
          <button
            className="w-full flex items-center gap-2 px-3 py-1.5 text-[12px] text-nexus-error hover:bg-nexus-error/10 text-left"
            onClick={() => { setDeleteSvcTarget({ id: svcCtxMenu.id, name: svcCtxMenu.name }); setSvcCtxMenu(null); }}
          >
            <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.3"><line x1="3" y1="6" x2="9" y2="6"/></svg>
            删除
          </button>
        </div>
      )}

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
    </>
  );
}
