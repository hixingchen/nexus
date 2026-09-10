import { useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { serviceApi, type Service, type ToolCommand } from '../../services/service';
import { open } from '@tauri-apps/plugin-dialog';
import { useToolStore } from '../../stores/toolStore';
import { ToolsManagerModal } from './ToolsManagerModal';
import { showNotification } from '../ui/Toast';

const WATCH_MODE_OFF = 0;
const WATCH_MODE_CONFIRM = 1;
const WATCH_MODE_AUTO = 2;

/** 编辑对象：服务或模板（模板无 project_id/sort_index，其余字段一致；模板额外带 open_tool_id） */
type ServiceConfig = Omit<Service, 'project_id' | 'sort_index'> & { open_tool_id?: string };

interface Props {
  service: ServiceConfig;
  onSave: () => void;
  /** 编辑模式：template 时保存到模板库，并隐藏「另存为模板」 */
  mode?: 'service' | 'template';
  /** 面板标题（有值时显示标题栏，区分服务/模板编辑） */
  title?: string;
  /** 另存为模板成功后的回调（父组件刷新模板库） */
  onSavedAsTemplate?: () => void;
  /** 面板右侧偏移（px）= 服务列宽度：服务列 absolute 覆盖在主区域上，编辑面板需显示在其左侧 */
  rightOffset?: number;
}

export function ServiceEditPanel({ service, onSave, mode = 'service', title, rightOffset = 360, onSavedAsTemplate }: Props) {
  const [name, setName] = useState(service.name);
  const [command, setCommand] = useState(service.command);
  const [cwd, setCwd] = useState(service.cwd);
  const [watchPaths, setWatchPaths] = useState(service.watch_paths);
  const [watchInclude, setWatchInclude] = useState(service.watch_include);
  const [watchExclude, setWatchExclude] = useState(service.watch_exclude);
  const [envVars, setEnvVars] = useState(service.env_vars);
  const [restartMode, setRestartMode] = useState(service.restart_mode);
  const [enabled, setEnabled] = useState(service.enabled);
  const [showFileTree, setShowFileTree] = useState(service.show_file_tree);
  const [saving, setSaving] = useState(false);
  const [savingTemplate, setSavingTemplate] = useState(false);
  const [showAdvanced, setShowAdvanced] = useState(false);

  // 工具命令状态
  const [toolCommands, setToolCommands] = useState<ToolCommand[]>(() => {
    try {
      return JSON.parse(service.tool_commands || '[]');
    } catch {
      return [];
    }
  });
  const [editingToolCmd, setEditingToolCmd] = useState<ToolCommand | null>(null);
  const [showToolCmdForm, setShowToolCmdForm] = useState(false);
  const [showToolManager, setShowToolManager] = useState(false);

  // 打开工具绑定（全局工具库 + 当前服务绑定）
  const openTools = useToolStore(s => s.openTools);
  const boundToolId = useToolStore(s => service.id ? s.bindings[service.id] : undefined);
  const bindTool = useToolStore(s => s.bind);
  // 模板模式：默认打开工具存模板字段（随"保存模板"提交，不是即时绑定）
  const [tplToolId, setTplToolId] = useState(service.open_tool_id ?? '');
  // 当前模式生效的工具选择值（服务=store 即时绑定；模板=表单状态）
  const pickerToolId = mode === 'template' ? (tplToolId || undefined) : boundToolId;
  const pickerTool = openTools.find(t => t.id === pickerToolId);
  const pickerToolName = pickerTool?.name;

  // 打开方式选择器：自定义浮层（fixed + portal，参考右键菜单模式）。
  // 原生 <select> 的选项样式/展开体验与面板风格割裂，且不可控
  const [toolPickerOpen, setToolPickerOpen] = useState(false);
  const pickerAnchorRef = useRef<HTMLButtonElement | null>(null);
  const [pickerPos, setPickerPos] = useState<{ left: number; top: number; width: number } | null>(null);

  const toolPickerRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!toolPickerOpen) return;
    // 外部点击 / 滚动 / Escape 时关闭（滚动容器非 window，需捕获阶段监听）。
    // contains 判断：点在菜单内、或点的是触发器本身 → 不自动关（触发器走 click 显式 toggle），
    // 否则会出现"mousedown 刚关掉、click 又打开"导致菜单关不掉
    const inMenu = (e: Event) => toolPickerRef.current?.contains(e.target as Node) ?? false;
    const inAnchor = (e: Event) => pickerAnchorRef.current?.contains(e.target as Node) ?? false;
    const onMouseDown = (e: MouseEvent) => { if (!inMenu(e) && !inAnchor(e)) setToolPickerOpen(false); };
    const onScroll = (e: Event) => { if (!inMenu(e)) setToolPickerOpen(false); };
    const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') setToolPickerOpen(false); };
    document.addEventListener('mousedown', onMouseDown);
    document.addEventListener('scroll', onScroll, true);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onMouseDown);
      document.removeEventListener('scroll', onScroll, true);
      document.removeEventListener('keydown', onKey);
    };
  }, [toolPickerOpen]);

  const openToolPicker = () => {
    const anchor = pickerAnchorRef.current;
    if (!anchor) return;
    const rect = anchor.getBoundingClientRect();
    // 菜单与触发器等宽（视觉对齐），超出视口时收拢
    const left = Math.max(8, Math.min(rect.left, window.innerWidth - rect.width - 8));
    setPickerPos({ left, top: rect.bottom + 4, width: rect.width });
    setToolPickerOpen(true);
  };

  const handleBindTool = async (toolId: string | null) => {
    setToolPickerOpen(false);
    if (mode === 'template') {
      // 模板：默认打开工具随「保存模板」提交，此处仅改本地状态
      setTplToolId(toolId ?? '');
      return;
    }
    try {
      await bindTool(service.id, toolId);
      showNotification({ variant: 'success', title: toolId ? '已绑定打开工具' : '已解除绑定' });
    } catch (err) {
      console.error('设置打开工具失败:', err);
      showNotification({ variant: 'error', title: '设置打开工具失败', description: String(err) });
    }
  };

  /**
   * 更新工作目录；若监听路径还处于"跟随工作目录"的默认状态
   * （空 / [] / 恰好等于 [旧 cwd]），则同步更新为 [新 cwd]
   */
  const applyCwd = (next: string) => {
    const prev = cwd;
    setCwd(next);
    const trimmed = watchPaths.trim();
    let follows = trimmed === '' || trimmed === '[]';
    if (!follows) {
      try {
        const arr = JSON.parse(trimmed) as unknown;
        follows = Array.isArray(arr) && arr.length === 1 && arr[0] === prev;
      } catch { /* 非 JSON（用户自定义格式）：不跟随 */ }
    }
    if (follows) setWatchPaths(next ? JSON.stringify([next]) : '[]');
  };

  const handleSelectCwd = async () => {
    const selected = await open({ directory: true, title: '选择工作目录', defaultPath: cwd });
    if (selected) applyCwd(selected);
  };

  const handleSave = async () => {
    setSaving(true);
    try {
      const payload = {
        id: service.id, name: name.trim() || service.name,
        command, cwd, watchPaths, watchInclude, watchExclude, envVars, restartMode, enabled,
        showFileTree,
        toolCommands: JSON.stringify(toolCommands),
      };
      if (mode === 'template') {
        // 模板：默认打开工具随保存提交（从模板添加服务时复制为绑定）
        await serviceApi.updateTemplate({ ...payload, openToolId: tplToolId });
      } else {
        await serviceApi.update(payload);
      }
      onSave();
    } catch (e: unknown) {
      console.error('保存配置失败:', e);
      showNotification({ variant: 'error', title: mode === 'template' ? '保存模板配置失败' : '保存服务配置失败', description: String(e) });
    }
    setSaving(false);
  };

  // 另存为模板：值拷贝当前已持久化的服务配置到全局模板库（跨项目复用）
  const handleSaveAsTemplate = async () => {
    setSavingTemplate(true);
    try {
      const tpl = await serviceApi.saveServiceAsTemplate(service.id);
      showNotification({ title: `已保存为模板「${tpl.name}」` });
      onSavedAsTemplate?.();
    } catch (e: unknown) {
      console.error('保存模板失败:', e);
      showNotification({ variant: 'error', title: '保存模板失败', description: String(e) });
    }
    setSavingTemplate(false);
  };

  // 添加/更新工具命令
  const handleSaveToolCommand = (cmd: ToolCommand) => {
    setToolCommands(prev => {
      const idx = prev.findIndex(c => c.id === cmd.id);
      if (idx >= 0) {
        const updated = [...prev];
        updated[idx] = cmd;
        return updated;
      }
      return [...prev, cmd];
    });
    setEditingToolCmd(null);
    setShowToolCmdForm(false);
  };

  // 删除工具命令
  const handleDeleteToolCommand = (id: string) => {
    setToolCommands(prev => prev.filter(c => c.id !== id));
  };

  // 编辑工具命令
  const handleEditToolCommand = (cmd: ToolCommand) => {
    setEditingToolCmd(cmd);
    setShowToolCmdForm(true);
  };

  const inputCls = "w-full mt-1 px-2.5 py-1.5 text-[13px] bg-nexus-bg border border-nexus-border rounded-md text-nexus-text placeholder:text-nexus-muted/50 focus:outline-none focus:border-nexus-accent transition-colors";
  const labelCls = "text-[11px] font-semibold text-nexus-muted uppercase tracking-wider";
  const cardCls = "bg-nexus-bg/30 border border-nexus-border/50 rounded-lg p-3.5";

  return (
    <div className="absolute top-0 bottom-0 w-[360px] bg-nexus-surface border-l border-nexus-border flex flex-col z-[60] shadow-2xl"
      style={{ right: rightOffset }}>
      {title && (
        <div className="flex items-center px-4 h-[42px] border-b border-nexus-border flex-shrink-0">
          <span className="text-[13px] text-nexus-text font-medium">{title}</span>
        </div>
      )}
      <div className="flex-1 overflow-auto p-3 space-y-3">
        {/* ── 基本信息卡片 ── */}
        <div className={cardCls}>
          <div className="space-y-3">
            <div>
              <label className={labelCls}>名称</label>
              <input className={inputCls} value={name} onChange={e => setName(e.target.value)} />
            </div>
            <div>
              <label className={labelCls}>启动命令</label>
              <textarea className={`${inputCls} font-mono resize-none`} rows={2} value={command}
                onChange={e => setCommand(e.target.value)} placeholder="npm run dev" />
            </div>
            <div>
              <label className={labelCls}>工作目录</label>
              <div className="relative mt-1">
                <input className={`${inputCls} pr-8`} value={cwd}
                  onChange={e => applyCwd(e.target.value)} placeholder="/path/to/service" />
                <button
                  className="absolute right-1.5 top-1/2 -translate-y-1/2 p-1 text-nexus-muted hover:text-nexus-text rounded"
                  onClick={handleSelectCwd}
                >
                  <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.3"><rect x="1" y="3" width="12" height="9" rx="1"/><path d="M1 5h12"/><path d="M5 1h2l1 2H5z"/></svg>
                </button>
              </div>
            </div>
          </div>
        </div>

        {/* ── 文件监听卡片 ── */}
        <div className={cardCls}>
          <span className={`${labelCls} block mb-2.5`}>文件监听</span>
          {/* 三段式选择 */}
          <div className="flex bg-nexus-bg rounded-md p-0.5 mb-3">
            {[
              { v: WATCH_MODE_OFF, label: '关闭' },
              { v: WATCH_MODE_CONFIRM, label: '确认重启' },
              { v: WATCH_MODE_AUTO, label: '自动重启' },
            ].map(opt => (
              <button key={opt.v}
                className={`flex-1 py-1.5 text-[12px] rounded transition-colors ${
                  restartMode === opt.v
                    ? 'bg-nexus-surface text-nexus-text font-medium shadow-sm'
                    : 'text-nexus-muted hover:text-nexus-text'
                }`}
                onClick={() => setRestartMode(opt.v)}
              >{opt.label}</button>
            ))}
          </div>
          {restartMode > WATCH_MODE_OFF && (
            <div className="space-y-3">
              <div>
                <label className={labelCls}>监听路径</label>
                <input className={`${inputCls} font-mono mt-1`} value={watchPaths}
                  onChange={e => setWatchPaths(e.target.value)} placeholder='["./server", "./shared"]' />
              </div>
              <div>
                <label className={labelCls}>包含文件</label>
                <textarea className={`${inputCls} font-mono resize-none mt-1`} rows={3} value={watchInclude}
                  onChange={e => setWatchInclude(e.target.value)}
                  placeholder={'*\n*.ts\n*.tsx\n*.rs\n*.py'} />
              </div>
              <div>
                <label className={labelCls}>排除</label>
                <textarea className={`${inputCls} font-mono resize-none mt-1`} rows={4} value={watchExclude}
                  onChange={e => setWatchExclude(e.target.value)}
                  placeholder={'node_modules\n.git\ndist\ntarget'} />
              </div>
            </div>
          )}
        </div>

        {/* ── 行为设置卡片 ── */}
        <div className={cardCls}>
          <span className={`${labelCls} block mb-2.5`}>行为</span>
          <div className="space-y-1">
            {[
              { checked: enabled, onChange: setEnabled, label: '跟随项目启动' },
              { checked: showFileTree, onChange: setShowFileTree, label: '在项目列表中显示目录树' },
            ].map((item, i) => (
              <div
                key={i}
                className="flex items-center gap-2.5 px-2 py-1.5 rounded hover:bg-nexus-hover/30 cursor-pointer transition-colors"
                onClick={() => item.onChange(!item.checked)}
              >
                <span className="flex-1 text-[13px] text-nexus-text">{item.label}</span>
                {/* 开关：选中时 accent 蓝轨道 + 滑块右移 */}
                <span
                  role="switch"
                  aria-checked={item.checked}
                  tabIndex={0}
                  onKeyDown={(e) => {
                    // 键盘切换（Enter/空格与 switch 惯例一致），鼠标走外层 div 点击
                    if (e.key === 'Enter' || e.key === ' ') {
                      e.preventDefault();
                      item.onChange(!item.checked);
                    }
                  }}
                  className={`relative flex-shrink-0 w-[34px] h-[18px] rounded-full transition-colors cursor-pointer focus:outline-none focus:ring-2 focus:ring-nexus-accent/40 ${
                    item.checked ? 'bg-nexus-accent' : 'bg-nexus-border'
                  }`}
                >
                  <span className={`absolute top-[2px] left-[2px] w-[14px] h-[14px] rounded-full bg-white shadow-sm transition-transform ${
                    item.checked ? 'translate-x-[16px]' : ''
                  }`} />
                </span>
              </div>
            ))}
          </div>
        </div>

        {/* ── 打开方式卡片（服务即时绑定；模板存默认值随保存提交，从模板添加服务时复制） ── */}
        <div className={cardCls}>
          <div className="flex items-center justify-between mb-2">
            <span className={labelCls}>打开方式</span>
            <button
              className="text-[11px] text-nexus-muted hover:text-nexus-accent rounded px-1.5 py-0.5 hover:bg-nexus-hover/50 transition-colors"
              onClick={() => setShowToolManager(true)}
            >管理工具</button>
          </div>
          <button
            ref={pickerAnchorRef}
            onClick={() => {
              if (toolPickerOpen) { setToolPickerOpen(false); return; }
              openToolPicker();
            }}
            className="w-full flex items-center gap-2 px-2.5 py-2 bg-nexus-bg border border-nexus-border rounded-md hover:border-nexus-accent/50 hover:bg-nexus-bg/80 transition-colors text-left"
            title={pickerToolName ? `${pickerToolName} · ${pickerTool?.executable ?? ''}` : '选择打开工具'}
          >
            <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.3"
              className={`flex-shrink-0 ${pickerToolId ? 'text-nexus-accent' : 'text-nexus-muted/40'}`}>
              <path d="M2 1h8a1 1 0 011 1v8a1 1 0 01-1 1H2a1 1 0 01-1-1V2a1 1 0 011-1z"/><path d="M1.5 7.5h9M4 7.5V11"/>
            </svg>
            <span className={`flex-1 truncate text-[13px] ${pickerToolId ? 'text-nexus-text' : 'text-nexus-muted/60'}`}>
              {pickerToolName ?? '未设置'}
            </span>
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.4"
              className={`flex-shrink-0 text-nexus-muted/50 transition-transform ${toolPickerOpen ? 'rotate-180' : ''}`}>
              <polyline points="2,3.5 5,6.5 8,3.5" />
            </svg>
          </button>
          <p className="text-[10px] text-nexus-muted/50 mt-1.5">
            {mode === 'template'
              ? '作为模板默认值：从模板添加服务时自动带上该绑定'
              : '服务右键菜单将出现「用所选工具打开」，打开其工作目录'}
          </p>
        </div>

        {/* ── 工具命令卡片 ── */}
        <div className={cardCls}>
          <div className="flex items-center justify-between mb-2.5">
            <span className={labelCls}>工具命令</span>
            <button
              className="p-1 text-nexus-muted hover:text-nexus-text rounded hover:bg-nexus-hover/50"
              title="添加工具命令"
              onClick={() => { setEditingToolCmd(null); setShowToolCmdForm(true); }}
            >
              <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.3">
                <line x1="6" y1="2" x2="6" y2="10"/><line x1="2" y1="6" x2="10" y2="6"/>
              </svg>
            </button>
          </div>
          {toolCommands.length === 0 ? (
            <div className="text-center py-3">
              <p className="text-[11px] text-nexus-muted/40">暂无工具命令</p>
              <p className="text-[10px] text-nexus-muted/30 mt-0.5">右键服务卡片可快速执行</p>
            </div>
          ) : (
            <div className="space-y-1.5">
              {toolCommands.map(cmd => (
                <div
                  key={cmd.id}
                  className={`group relative rounded-md border transition-all duration-150 ${
                    editingToolCmd?.id === cmd.id
                      ? 'border-nexus-accent/40 bg-nexus-accent/5 shadow-sm'
                      : 'border-nexus-border/20 bg-nexus-bg/40 hover:border-nexus-border/40 hover:bg-nexus-bg/60'
                  }`}
                >
                  <div className="flex items-center gap-2.5 px-2.5 py-2">
                    {/* 命令图标 */}
                    <div className="flex-shrink-0 w-6 h-6 rounded bg-nexus-surface border border-nexus-border/50 flex items-center justify-center">
                      <svg width="12" height="12" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" className="text-nexus-muted">
                        <polyline points="1,3 4,5 1,7"/>
                        <line x1="5" y1="7" x2="8" y2="7"/>
                      </svg>
                    </div>

                    {/* 命令信息 */}
                    <div className="flex-1 min-w-0 flex items-baseline gap-2 overflow-hidden">
                      <span className="text-[12px] text-nexus-text font-medium flex-shrink-0 leading-none">{cmd.name}</span>
                      <span className="text-[11px] text-nexus-muted/60 font-mono truncate leading-none">{cmd.command}</span>
                    </div>

                    {/* 操作按钮 */}
                    {editingToolCmd?.id !== cmd.id && (
                      <div className="flex items-center gap-1 opacity-0 group-hover:opacity-100 transition-opacity">
                        <button
                          className="p-1.5 text-nexus-muted hover:text-nexus-accent hover:bg-nexus-accent/10 rounded-md transition-colors"
                          title="编辑"
                          onClick={() => handleEditToolCommand(cmd)}
                        >
                          <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.3">
                            <path d="M10 3l1 1-6 6H4v-1l6-6z"/>
                          </svg>
                        </button>
                        <button
                          className="p-1.5 text-nexus-muted hover:text-nexus-error hover:bg-nexus-error/10 rounded-md transition-colors"
                          title="删除"
                          onClick={() => handleDeleteToolCommand(cmd.id)}
                        >
                          <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.3">
                            <path d="M4 4l6 6M10 4l-6 6"/>
                          </svg>
                        </button>
                      </div>
                    )}
                  </div>
                </div>
              ))}
            </div>
          )}

          {/* 工具命令表单 */}
          {showToolCmdForm && (
            <ToolCommandForm
              initial={editingToolCmd}
              onSave={handleSaveToolCommand}
              onDelete={editingToolCmd ? () => { handleDeleteToolCommand(editingToolCmd.id); setEditingToolCmd(null); setShowToolCmdForm(false); } : undefined}
              onCancel={() => { setEditingToolCmd(null); setShowToolCmdForm(false); }}
            />
          )}
        </div>

        {/* ── 高级卡片 ── */}
        <div className={cardCls}>
          <button
            className="flex items-center gap-1.5 w-full text-left text-[11px] font-semibold text-nexus-muted uppercase tracking-wider hover:text-nexus-text"
            onClick={() => setShowAdvanced(!showAdvanced)}
          >
            <svg className={`transition-transform ${showAdvanced ? 'rotate-90' : ''}`} width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round">
              <polyline points="3,1 7,5 3,9" />
            </svg>
            高级
          </button>
          {showAdvanced && (
            <div className="mt-3">
              <label className={labelCls}>环境变量</label>
              <textarea className={`${inputCls} font-mono resize-none mt-1`} rows={4} value={envVars}
                onChange={e => setEnvVars(e.target.value)} placeholder={'PORT=3000\nNODE_ENV=development'} />
            </div>
          )}
        </div>
      </div>

      {/* 底部 */}
      <div className="p-3 border-t border-nexus-border flex-shrink-0 space-y-2">
        {mode !== 'template' && (
          <button
            className="w-full px-4 py-1.5 text-[12px] text-nexus-accent border border-nexus-accent/40 rounded-md hover:bg-nexus-accent/10 disabled:opacity-40 font-medium transition-colors"
            disabled={savingTemplate}
            onClick={handleSaveAsTemplate}
            title="复制此服务配置到模板库，供其他项目复用"
          >{savingTemplate ? '保存中…' : '☆ 另存为模板'}</button>
        )}
        <button
          className="w-full px-4 py-2 text-[13px] bg-nexus-accent text-white rounded-md hover:bg-nexus-accent-hover disabled:opacity-40 font-medium transition-colors"
          disabled={saving || !name.trim()} onClick={handleSave}>{saving ? '保存中…' : (mode === 'template' ? '保存模板' : '保存配置')}</button>
      </div>

      {/* 打开方式浮层列表（portal 到 body：面板容器 overflow 会裁剪内部 absolute 菜单；宽度与触发器等宽） */}
      {toolPickerOpen && pickerPos && createPortal(
        <div
          ref={toolPickerRef}
          className="fixed z-[70] bg-nexus-surface border border-nexus-border/60 rounded-lg shadow-2xl overflow-hidden"
          style={{ left: pickerPos.left, top: pickerPos.top, width: pickerPos.width }}
        >
          <div className="max-h-[264px] overflow-auto py-1">
            {/* 解绑 */}
            <button
              className={`w-full flex items-center gap-2 px-3 py-2 text-left transition-colors ${
                !pickerToolId ? 'bg-nexus-accent/10' : 'hover:bg-nexus-hover/50'
              }`}
              onClick={() => handleBindTool(null)}
            >
              <span className={`flex-1 text-[12px] ${pickerToolId ? 'text-nexus-muted/70' : 'text-nexus-text font-medium'}`}>
                未设置
              </span>
              {!pickerToolId && (
                <svg width="11" height="11" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.6"
                  className="text-nexus-accent flex-shrink-0">
                  <polyline points="1.5,5.5 4,7.5 8.5,2.5" />
                </svg>
              )}
            </button>
            {openTools.length === 0 && (
              <p className="px-3 py-1.5 text-[11px] text-nexus-muted/40">暂无工具，点右上「管理工具」添加</p>
            )}
            {openTools.map(t => {
              const active = pickerToolId === t.id;
              return (
                <button
                  key={t.id}
                  className={`w-full flex items-center gap-2 px-3 py-1.5 text-left transition-colors ${
                    active ? 'bg-nexus-accent/10' : 'hover:bg-nexus-hover/50'
                  }`}
                  title={t.command}
                  onClick={() => handleBindTool(t.id)}
                >
                  <span className={`text-[12px] truncate flex-1 ${active ? 'text-nexus-text font-medium' : 'text-nexus-text/90'}`}>
                    {t.name}
                  </span>
                  {active && (
                    <svg width="11" height="11" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.6"
                      className="text-nexus-accent flex-shrink-0">
                      <polyline points="1.5,5.5 4,7.5 8.5,2.5" />
                    </svg>
                  )}
                </button>
              );
            })}
          </div>
        </div>,
        document.body,
      )}

      {/* 工具库管理（全局增删改 + 快捷填充） */}
      <ToolsManagerModal open={showToolManager} onClose={() => setShowToolManager(false)} />
    </div>
  );
}

// ── 工具命令表单组件 ──────────────────────────────────────

interface ToolCommandFormProps {
  initial: ToolCommand | null;
  onSave: (cmd: ToolCommand) => void;
  onDelete?: () => void;
  onCancel: () => void;
}

function ToolCommandForm({ initial, onSave, onDelete, onCancel }: ToolCommandFormProps) {
  const [name, setName] = useState(initial?.name ?? '');
  const [cmd, setCmd] = useState(initial?.command ?? '');
  /** 超时（秒）文本：空 = 默认 60；0 = 不限制；正数 = 该秒数 */
  const [timeoutSecs, setTimeoutSecs] = useState(
    initial?.timeout_secs != null ? String(initial.timeout_secs) : ''
  );

  /** 空 → undefined（默认）；非负整数 → 数值；非法输入 → undefined（按默认处理） */
  const parseTimeout = (): number | undefined => {
    const s = timeoutSecs.trim();
    if (s === '') return undefined;
    const n = Number(s);
    if (!Number.isFinite(n) || n < 0) return undefined;
    return Math.floor(n);
  };

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (!name.trim() || !cmd.trim()) return;
    onSave({
      id: initial?.id ?? crypto.randomUUID(),
      name: name.trim(),
      command: cmd.trim(),
      timeout_secs: parseTimeout(),
    });
  };

  const inputCls = "w-full mt-1 px-2.5 py-1.5 text-[13px] bg-nexus-bg border border-nexus-border rounded-md text-nexus-text placeholder:text-nexus-muted/50 focus:outline-none focus:border-nexus-accent transition-colors";

  return (
    <form onSubmit={handleSubmit} className="mt-3 space-y-2 p-2.5 bg-nexus-bg/50 rounded-md border border-nexus-border/50">
      <div>
        <label className="text-[11px] text-nexus-muted">名称</label>
        <input
          className={inputCls}
          value={name}
          onChange={e => setName(e.target.value)}
          placeholder="clean"
          autoFocus
        />
      </div>
      <div>
        <label className="text-[11px] text-nexus-muted">命令</label>
        <input
          className={`${inputCls} font-mono`}
          value={cmd}
          onChange={e => setCmd(e.target.value)}
          placeholder="mvn clean"
        />
      </div>
      <div>
        <label className="text-[11px] text-nexus-muted">超时（秒）</label>
        <input
          className={inputCls}
          value={timeoutSecs}
          onChange={e => setTimeoutSecs(e.target.value)}
          placeholder="留空 = 默认 60；0 = 不限制（打包/构建类建议 1800）"
          inputMode="numeric"
        />
      </div>
      <div className="flex items-center justify-between pt-1">
        <div>
          {onDelete && (
            <button
              type="button"
              className="px-3 py-1 text-[11px] text-nexus-error hover:bg-nexus-error/10 rounded transition-colors"
              onClick={onDelete}
            >删除此命令</button>
          )}
        </div>
        <div className="flex gap-2">
          <button
            type="button"
            className="px-3 py-1 text-[11px] text-nexus-muted hover:text-nexus-text rounded hover:bg-nexus-hover/50"
            onClick={onCancel}
          >取消</button>
          <button
            type="submit"
            className="px-3 py-1 text-[11px] bg-nexus-accent text-white rounded hover:bg-nexus-accent-hover disabled:opacity-40"
            disabled={!name.trim() || !cmd.trim()}
          >{initial ? '更新' : '添加'}</button>
        </div>
      </div>
    </form>
  );
}
