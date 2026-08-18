import { useState } from 'react';
import { serviceApi, type Service, type ToolCommand } from '../../services/service';
import { open } from '@tauri-apps/plugin-dialog';
import { showNotification } from '../ui/Toast';

const WATCH_MODE_OFF = 0;
const WATCH_MODE_CONFIRM = 1;
const WATCH_MODE_AUTO = 2;

/** 编辑对象：服务或模板（模板无 project_id/sort_index，其余字段一致） */
type ServiceConfig = Omit<Service, 'project_id' | 'sort_index'>;

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
        await serviceApi.updateTemplate(payload);
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
    <div className="absolute top-0 bottom-0 w-[360px] bg-nexus-surface border-l border-nexus-border flex flex-col z-10 shadow-2xl"
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
                  className={`relative flex-shrink-0 w-[34px] h-[18px] rounded-full transition-colors ${
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

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (!name.trim() || !cmd.trim()) return;
    onSave({
      id: initial?.id ?? crypto.randomUUID(),
      name: name.trim(),
      command: cmd.trim(),
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
