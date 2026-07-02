import { useState, useEffect, useRef, useCallback } from 'react';
import { listen } from '@tauri-apps/api/event';
import { TitleBar } from './TitleBar';
import { StatusBar } from './StatusBar';
import { ResizablePanel } from './ResizablePanel';
import { DevConsole } from '../terminal/DevConsole';
import { ProjectList } from './ProjectList';
import { ProjectDetail } from './ProjectDetail';
import { RestartConfirm } from './RestartConfirm';
import { layoutApi, securityApi, projectApi } from '../../services/service';
import { useLogStore } from '../../stores/logStore';
import type { ServiceLogEvent } from '../../services/logService';
import { showNotification } from '../ui/Toast';

export function MainLayout() {
  const [selectedProjectId, setSelectedProjectId] = useState<string | null>(null);
  const [selectedProjectName, setSelectedProjectName] = useState<string | null>(null);
  const [selectedProjectPath, setSelectedProjectPath] = useState<string | null>(null);
  const [terminalInitCommand, setTerminalInitCommand] = useState<string | undefined>(undefined);
  const [showTerminal, setShowTerminal] = useState(false);
  const [leftPanelWidth, setLeftPanelWidth] = useState(260);
  const [terminalHeight, setTerminalHeight] = useState(400);
  const [servicePanelCollapsed, setServicePanelCollapsed] = useState(false);
  const [ready, setReady] = useState(false);

  // ── 全局 service-log 监听 ──
  // 用 useRef 存 unlisten，避免 Strict Mode 双挂载导致 listener 错乱
  const logListenerRef = useRef<{ unlisten: () => void } | null>(null);

  useEffect(() => {
    type Item = { serviceKey: string; stream: 'stdout' | 'stderr'; data: string };
    const batch: Item[] = [];
    let timer: ReturnType<typeof setTimeout> | null = null;
    let disposed = false;

    const flush = () => {
      if (batch.length === 0) return;
      useLogStore.getState().bulkAppend(batch.splice(0));
      timer = null;
    };

    listen<ServiceLogEvent>('service-log', (event) => {
      if (disposed) return;
      batch.push({
        serviceKey: event.payload.service_key,
        stream: event.payload.stream,
        data: event.payload.data,
      });
      if (!timer) timer = setTimeout(flush, 50);
    }).then(fn => {
      if (disposed) { fn(); return; }
      logListenerRef.current = { unlisten: fn };
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

  // 防抖保存布局
  const saveTimer = useRef<ReturnType<typeof setTimeout>>();
  const saveLayout = useCallback((patch: Record<string, string>) => {
    clearTimeout(saveTimer.current);
    saveTimer.current = setTimeout(() => {
      layoutApi.save(patch).catch((e) => { console.error('保存布局失败:', e); showNotification({ variant: 'error', title: '保存布局失败' }); });
    }, 500);
  }, []);
  // 卸载时清理定时器
  useEffect(() => () => clearTimeout(saveTimer.current), []);

  // 启动时从 DB 恢复布局
  useEffect(() => {
    layoutApi.load().then(async (layout) => {
      if (layout.selected_project_id) {
        try {
          const detail = await projectApi.getDetail(layout.selected_project_id);
          setSelectedProjectId(layout.selected_project_id);
          setSelectedProjectName(detail.project.name);
          setSelectedProjectPath(detail.project.path);
          setTerminalInitCommand(detail.project.terminal_init_command || undefined);
        } catch {
          // 项目可能已删除或数据库重建，清除选中状态
          setSelectedProjectId(null);
          setSelectedProjectName(null);
          setSelectedProjectPath(null);
          setTerminalInitCommand(undefined);
          saveLayout({ selected_project_id: '' });
        }
      }
      if (layout.show_terminal === '1') setShowTerminal(true);
      if (layout.left_panel_width) setLeftPanelWidth(Number(layout.left_panel_width));
      if (layout.terminal_height) setTerminalHeight(Number(layout.terminal_height));
      if (layout.service_panel_collapsed === '1') setServicePanelCollapsed(true);
      setReady(true);
    }).catch(() => setReady(true));
  }, []);

  // 选中项目变化时保存
  useEffect(() => {
    if (!ready) return;
    saveLayout({ selected_project_id: selectedProjectId ?? '' });
  }, [selectedProjectId, ready, saveLayout]);

  // 面板状态变化时保存
  useEffect(() => {
    if (!ready) return;
    saveLayout({ show_terminal: showTerminal ? '1' : '0' });
  }, [showTerminal, ready, saveLayout]);

  useEffect(() => {
    if (!ready) return;
    saveLayout({ service_panel_collapsed: servicePanelCollapsed ? '1' : '0' });
  }, [servicePanelCollapsed, ready, saveLayout]);

  // 文件监听由 ProjectDetail 统一管理（服务配置变更时需重启监听）

  // 项目切换时设置文件访问白名单
  useEffect(() => {
    securityApi.setProjectRoot(selectedProjectPath).catch((e) => console.error('setProjectRoot 失败:', e));
  }, [selectedProjectPath]);

  const leftPanel = (
    <ProjectList
      selectedId={selectedProjectId}
      onSelect={(id) => { setSelectedProjectId(id); if (!id) { setSelectedProjectName(null); setSelectedProjectPath(null); } }}
      onProjectName={setSelectedProjectName}
      onProjectPath={setSelectedProjectPath}
    />
  );

  // 项目启动服务时自动打开终端面板
  const handleProjectStart = useCallback(() => {
    setShowTerminal(true);
  }, []);

  const detailPanel = selectedProjectId ? (
    <ProjectDetail
      projectId={selectedProjectId}
      servicePanelCollapsed={servicePanelCollapsed}
      onToggleServicePanel={() => setServicePanelCollapsed(p => !p)}
      onProjectStart={handleProjectStart}
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

  const editorWithTerminal = (
    <div className="h-full w-full relative">
      {/* 终端面板：始终挂载，用 visibility 控制显隐 */}
      <div
        className="absolute inset-0"
        style={{ visibility: showTerminal ? 'visible' : 'hidden', pointerEvents: showTerminal ? 'auto' : 'none' }}
      >
        <ResizablePanel
          left={detailPanel}
          right={<DevConsole projectId={selectedProjectId} projectName={selectedProjectName} projectPath={selectedProjectPath} visible={showTerminal} terminalInitCommand={terminalInitCommand} />}
          defaultLeftWidth={terminalHeight}
          minWidth={120}
          direction="vertical"
          onResize={(h) => { setTerminalHeight(h); saveLayout({ terminal_height: String(h) }); }}
        />
      </div>
      {/* 终端隐藏时显示纯编辑器 */}
      {!showTerminal && (
        <div className="absolute inset-0">{detailPanel}</div>
      )}
    </div>
  );

  const mainContent = (
    <ResizablePanel
      left={leftPanel}
      right={editorWithTerminal}
      defaultLeftWidth={leftPanelWidth}
      minWidth={150}
      maxWidth={500}
      direction="horizontal"
      onResize={(w) => { setLeftPanelWidth(w); saveLayout({ left_panel_width: String(w) }); }}
    />
  );

  return (
    <div className="h-screen flex flex-col bg-nexus-editor text-nexus-text">
      <TitleBar projectName={selectedProjectName} />
      <div className="flex-1 overflow-hidden">{mainContent}</div>
      <RestartConfirm />
      <StatusBar showTerminal={showTerminal} onToggleTerminal={() => setShowTerminal(p => !p)} />
    </div>
  );
}
