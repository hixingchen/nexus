import { useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { arrayMove } from '@dnd-kit/sortable';
import { serviceApi, parseToolCommands, type Service, type ToolCommand } from '../../services/service';
import { pickDirectory } from '../../services/system';
import { useToolStore } from '../../stores/toolStore';
import { readServiceDraft, serviceDraftKey, useServiceDraftStore } from '../../stores/serviceDraftStore';
import { ToolsManagerModal } from './ToolsManagerModal';
import { showNotification } from '../ui/Toast';
import { reportError } from '../../utils/error';
import { useClickOutside } from '../../hooks/useClickOutside';

const WATCH_MODE_OFF = 0;
const WATCH_MODE_CONFIRM = 1;
const WATCH_MODE_AUTO = 2;

/** 配置面板输入框统一样式（配置表单与工具命令表单各有一份，逐字相同——改一处漏一处会出现两种输入框） */
const INPUT_CLS = "w-full mt-1 px-2.5 py-1.5 text-[13px] bg-nexus-bg border border-nexus-border rounded-md text-nexus-text placeholder:text-nexus-muted/50 focus:outline-none focus:border-nexus-accent transition-colors";

/**
 * 遮蔽环境变量文本里的**值**（保留键名与行结构，用户仍能看出配了哪些变量）。
 *
 * 只用于**显示**：面板内部状态与保存路径始终是真实值（遮蔽态 textarea 只读，
 * 不存在"把掩码存进配置"的可能）。`KEY=`（空值）、注释行、无 `=` 的行原样保留。
 */
function maskEnvValues(text: string): string {
  return text
    .split('\n')
    .map(line => {
      const eq = line.indexOf('=');
      if (eq < 0) return line;
      const key = line.slice(0, eq);
      const value = line.slice(eq + 1);
      if (value.trim() === '') return line;
      // 固定长度掩码：不泄露原值长度（长度本身也能透露信息）
      return `${key}=••••••`;
    })
    .join('\n');
}

/**
 * 编辑对象：服务或模板（模板无 project_id/sort_index，其余字段一致；模板额外带 open_tool_id）。
 *
 * `project_id` 是**可选**的：编辑既有对象时后端不需要它，而"新建服务"要靠它确定归属
 * （见 handleSave 的 add 分支）——服务对象本来就带着它，模板没有。
 */
type ServiceConfig = Omit<Service, 'project_id' | 'sort_index'> & {
  open_tool_id?: string;
  project_id?: string;
};

interface Props {
  service: ServiceConfig;
  onSave: () => void;
  /** 编辑模式：template 时保存到模板库，并隐藏「另存为模板」 */
  mode?: 'service' | 'template';
  /** 面板标题（有值时显示标题栏，区分服务/模板编辑） */
  title?: string;
  /** 关闭面板（标题栏右端的 ✕）。不传则不渲染 ✕——旧调用点仍可靠"再点一次卡片"关闭 */
  onClose?: () => void;
  /** 另存为模板成功后的回调（父组件刷新模板库） */
  onSavedAsTemplate?: () => void;
  /** 面板右侧偏移（px）= 服务列宽度：服务列 absolute 覆盖在主区域上，编辑面板需显示在其左侧 */
  rightOffset?: number;
}

export function ServiceEditPanel({ service, onSave, mode = 'service', title, rightOffset = 360, onSavedAsTemplate, onClose }: Props) {
  /** 草稿键与面板的编辑目标一一对应（见 serviceDraftStore 的六条丢失路径说明） */
  const draftKey = serviceDraftKey(service.id, mode);
  /**
   * 挂载时读一次草稿恢复表单；不订阅，避免每次写入草稿导致本组件多渲染一轮。
   *
   * **新建时不恢复**（`service.id` 为空）：新建的草稿键是固定的空串，一恢复就会把
   * 上一次没保存的内容带进来——而用户点「新建」期望的是空表单（用户反馈过
   * "新增功能要做好清空操作"）。编辑既有对象时草稿照常生效，那是它本来的用途。
   */
  const [initialDraft] = useState(() => (service.id ? readServiceDraft(draftKey) : undefined));
  const [name, setName] = useState(initialDraft?.name ?? service.name);
  const [command, setCommand] = useState(initialDraft?.command ?? service.command);
  const [cwd, setCwd] = useState(initialDraft?.cwd ?? service.cwd);
  const [watchPaths, setWatchPaths] = useState(initialDraft?.watchPaths ?? service.watch_paths);
  const [watchInclude, setWatchInclude] = useState(initialDraft?.watchInclude ?? service.watch_include);
  const [watchExclude, setWatchExclude] = useState(initialDraft?.watchExclude ?? service.watch_exclude);
  const [envVars, setEnvVars] = useState(initialDraft?.envVars ?? service.env_vars);
  /** 环境变量值是否明文显示（默认遮蔽，见 maskEnvValues） */
  const [showEnvValues, setShowEnvValues] = useState(false);
  const [restartMode, setRestartMode] = useState(initialDraft?.restartMode ?? service.restart_mode);
  const [enabled, setEnabled] = useState(initialDraft?.enabled ?? service.enabled);
  const [showFileTree, setShowFileTree] = useState(initialDraft?.showFileTree ?? service.show_file_tree);
  const [saving, setSaving] = useState(false);
  const [savingTemplate, setSavingTemplate] = useState(false);
  const [showAdvanced, setShowAdvanced] = useState(false);

  // 工具命令状态（DB 里的 TEXT 列属不受信数据，用带校验的解析器而不是裸 JSON.parse）。
  // 原值快照只算一次：dirty 判定每键都要比较，不能每次渲染重新 parse
  const [toolCommands, setToolCommands] = useState<ToolCommand[]>(
    () => initialDraft?.toolCommands ?? parseToolCommands(service.tool_commands),
  );
  const [originalToolCommands] = useState(() => JSON.stringify(parseToolCommands(service.tool_commands)));
  const [editingToolCmd, setEditingToolCmd] = useState<ToolCommand | null>(null);
  const [showToolCmdForm, setShowToolCmdForm] = useState(false);
  const [showToolManager, setShowToolManager] = useState(false);

  // 打开工具绑定（全局工具库 + 当前服务绑定）
  const openTools = useToolStore(s => s.openTools);
  const boundToolId = useToolStore(s => service.id ? s.bindings[service.id] : undefined);
  const bindTool = useToolStore(s => s.bind);
  // 模板模式：默认打开工具存模板字段（随"保存模板"提交，不是即时绑定）
  const [tplToolId, setTplToolId] = useState(initialDraft?.tplToolId ?? service.open_tool_id ?? '');
  /**
   * 新建服务时选中的打开工具。
   *
   * 此时服务还没有 id，而绑定 API 要真实 id——直接调会报"服务ID不能为空"
   * （用户反馈的正是这条）。所以先记在本地，等保存拿到 id 后再真正绑
   * （见 handleSave 的 add 分支）。`null` = 还没动过这个字段。
   */
  const [pendingToolId, setPendingToolId] = useState<string | null>(null);
  // 当前模式生效的工具选择值（服务=store 即时绑定；新建=本地暂存；模板=表单状态）
  const pickerToolId = mode === 'template'
    ? (tplToolId || undefined)
    : (service.id ? boundToolId : (pendingToolId ?? undefined));
  const pickerTool = openTools.find(t => t.id === pickerToolId);
  const pickerToolName = pickerTool?.name;

  /**
   * 表单是否有未保存修改：与**已持久化的配置**逐字段比较（工具命令按序列化文本比）。
   * 只有 true 时才写草稿——改回原样会自动清掉草稿与未保存标记。
   */
  const formDirty =
    name !== service.name || command !== service.command || cwd !== service.cwd
    || watchPaths !== service.watch_paths || watchInclude !== service.watch_include
    || watchExclude !== service.watch_exclude || envVars !== service.env_vars
    || restartMode !== service.restart_mode || enabled !== service.enabled
    || showFileTree !== service.show_file_tree
    || (mode === 'template' && tplToolId !== (service.open_tool_id ?? ''))
    || JSON.stringify(toolCommands) !== originalToolCommands;

  // 草稿落地：任何字段变化都同步进 store（面板被卸载/切项目也不丢）。
  // 显式列出全部字段而不是空依赖：空依赖会因闭包过期而漏写最新值
  useEffect(() => {
    useServiceDraftStore.getState().setDraft(draftKey, formDirty ? {
      name, command, cwd, watchPaths, watchInclude, watchExclude, envVars,
      restartMode, enabled, showFileTree, toolCommands, tplToolId,
    } : null);
  }, [draftKey, formDirty, name, command, cwd, watchPaths, watchInclude, watchExclude, envVars,
      restartMode, enabled, showFileTree, toolCommands, tplToolId]);

  /** 放弃未保存修改：回到已持久化的配置，并清掉草稿 */
  const handleDiscard = () => {
    setName(service.name);
    setCommand(service.command);
    setCwd(service.cwd);
    setWatchPaths(service.watch_paths);
    setWatchInclude(service.watch_include);
    setWatchExclude(service.watch_exclude);
    setEnvVars(service.env_vars);
    setRestartMode(service.restart_mode);
    setEnabled(service.enabled);
    setShowFileTree(service.show_file_tree);
    setTplToolId(service.open_tool_id ?? '');
    setToolCommands(parseToolCommands(service.tool_commands));
    useServiceDraftStore.getState().setDraft(draftKey, null);
  };

  // 打开方式选择器：自定义浮层（fixed + portal，参考右键菜单模式）。
  // 原生 <select> 的选项样式/展开体验与面板风格割裂，且不可控
  const [toolPickerOpen, setToolPickerOpen] = useState(false);
  const pickerAnchorRef = useRef<HTMLButtonElement | null>(null);
  const [pickerPos, setPickerPos] = useState<{ left: number; top: number; width: number } | null>(null);

  const toolPickerRef = useRef<HTMLDivElement | null>(null);

  // 外部点击 / 滚动 / Escape 时关闭。触发器（anchor）也算"内部"：点在触发器上由它自己的
  // click 显式 toggle，否则会出现"mousedown 刚关掉、click 又打开"导致菜单关不掉。
  // captureScroll：面板的滚动容器非 window，scroll 不冒泡，需捕获阶段监听
  useClickOutside(toolPickerRef, () => setToolPickerOpen(false), {
    refs: [pickerAnchorRef],
    escape: true,
    captureScroll: true,
  });

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
    if (!service.id) {
      // 新建中：还没有 id，绑定 API 会报"服务ID不能为空"。先记本地，保存后补绑
      setPendingToolId(toolId);
      return;
    }
    try {
      await bindTool(service.id, toolId);
      showNotification({ variant: 'success', title: toolId ? '已绑定打开工具' : '已解除绑定' });
    } catch (err) {
      reportError('设置打开工具失败', err);
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
    // 走后端原生选择器：选中的目录会被记为"用户已确认"，项目外目录才允许配置
    const selected = await pickDirectory({ purpose: 'serviceCwd', defaultPath: cwd });
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
        if (service.id) {
          await serviceApi.updateTemplate({ ...payload, openToolId: tplToolId });
        } else {
          // id 为空 = 模板库里刚点「新建」出来的空壳，走 create（id 由后端生成）
          await serviceApi.createServiceTemplate({
            name: name.trim(), command, cwd, watchPaths, watchInclude, watchExclude,
            envVars, restartMode, enabled, showFileTree,
            toolCommands: JSON.stringify(toolCommands),
            openToolId: tplToolId,
          });
        }
      } else if (!service.id) {
        // 新建服务：项目里还没有这条记录，走 add。**带全字段**——这正是把"添加服务"
        // 从弹窗换成面板的理由：那里只能填三个字段，其余四项由后端写死，用户得建完
        // 再开一次面板补
        const created = await serviceApi.add({
          projectId: service.project_id ?? '',
          name: name.trim(), command, cwd, watchPaths,
          // 空值 = "还没配" → 不传，由后端填默认。那套默认排除目录的**唯一来源在后端**，
          // 前端再抄一份迟早与它漂移（现在前端的 placeholder 只有 4 行，后端有 8 行，
          // 已经不一致了——正好说明不该抄）
          watchInclude: watchInclude || undefined,
          watchExclude: watchExclude || undefined,
          envVars, restartMode, enabled, showFileTree,
          toolCommands: JSON.stringify(toolCommands),
        });
        // 保存拿到 id 之后才能绑：新建时选的打开工具在这里补上
        if (pendingToolId) {
          try {
            await bindTool(created.id, pendingToolId);
          } catch (err) {
            // 服务已经建好了——绑定失败不该让整次保存看起来像失败了
            reportError('服务已创建，但设置打开工具失败', err);
          }
        }
      } else {
        await serviceApi.update(payload);
      }
      // 已持久化：草稿使命完成（否则重新打开面板会把旧编辑又"复活"回来）
      useServiceDraftStore.getState().setDraft(draftKey, null);
      onSave();
    } catch (e: unknown) {
      reportError(mode === 'template' ? '保存模板配置失败' : '保存服务配置失败', e);
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
      reportError('保存模板失败', e);
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

  /**
   * 工具命令上移/下移。
   *
   * 顺序就是数组顺序，也正是右键菜单里的排列顺序（菜单按这个数组渲染），
   * 所以这里改完跟着表单一起保存即可——`formDirty` 按序列化文本比较，重排会自动算作"未保存的修改"，
   * 不需要额外的脏标记。
   *
   * 用 dnd-kit 的 `arrayMove`（服务卡片与模板的拖拽排序用的是同一个函数）：自己写 splice
   * 容易在下标越界时静默把元素挪错位，而边界判断只需要在调用点拦一下。
   */
  const moveToolCommand = (id: string, delta: number) => {
    setToolCommands(prev => {
      const idx = prev.findIndex(c => c.id === id);
      const to = idx + delta;
      if (idx < 0 || to < 0 || to >= prev.length) return prev; // 首/尾再移就原地不动
      return arrayMove(prev, idx, to);
    });
  };

  const labelCls = "text-[11px] font-semibold text-nexus-muted uppercase tracking-wider";
  const cardCls = "bg-nexus-bg/30 border border-nexus-border/50 rounded-lg p-3.5";

  return (
    <div className="absolute top-0 bottom-0 w-[360px] bg-nexus-surface border-l border-nexus-border flex flex-col z-[60] shadow-2xl"
      style={{ right: rightOffset }}>
      {/* 标题栏：标题 + 关闭。
          为什么要有 ✕（用户反馈"点开了怎么关"）：此前退出只有"再点一次同一张卡片"
          这条隐蔽路径（toggle），面板上看不出任何出口 */}
      {(title || onClose) && (
        <div className="flex items-center gap-2 px-4 h-[42px] border-b border-nexus-border flex-shrink-0">
          <span className="flex-1 min-w-0 text-[13px] text-nexus-text font-medium truncate">{title}</span>
          {onClose && (
            <button
              type="button"
              className="flex-shrink-0 -mr-1 p-1 text-nexus-muted rounded hover:text-nexus-text hover:bg-nexus-hover/50 transition-colors"
              // 说清楚"关了不会丢"：下面的未保存提示条也写着同一件事
              title="关闭（未保存的改动会留作草稿，下次打开还在）"
              onClick={onClose}
            >
              <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round">
                <line x1="3" y1="3" x2="9" y2="9" />
                <line x1="9" y1="3" x2="3" y2="9" />
              </svg>
            </button>
          )}
        </div>
      )}

      {/* 未保存提示：面板的字段改动会随面板卸载/切项目保留（草稿按 id 存），
          但没有持久化——这里给一个显式出口，避免用户以为已经保存 */}
      {formDirty && (
        <div className="flex items-center gap-2 px-4 h-[28px] flex-shrink-0 bg-nexus-warning/10 border-b border-nexus-warning/25">
          <span className="w-1.5 h-1.5 rounded-full bg-nexus-warning flex-shrink-0" />
          <span className="flex-1 text-[11.5px] text-nexus-warning">有未保存的修改（切换服务/面板不会丢失）</span>
          <button
            className="text-[11.5px] text-nexus-warning hover:underline"
            title="回到已保存的配置"
            onClick={handleDiscard}
          >放弃修改</button>
        </div>
      )}
      <div className="flex-1 overflow-auto p-3 space-y-3">
        {/* ── 基本信息卡片 ── */}
        <div className={cardCls}>
          <div className="space-y-3">
            <div>
              <label className={labelCls}>名称</label>
              <input className={INPUT_CLS} value={name} onChange={e => setName(e.target.value)} />
            </div>
            <div>
              <label className={labelCls}>启动命令</label>
              <textarea className={`${INPUT_CLS} font-mono resize-none`} rows={2} value={command}
                onChange={e => setCommand(e.target.value)} placeholder="npm run dev" />
            </div>
            <div>
              <label className={labelCls}>工作目录</label>
              <div className="relative mt-1">
                <input className={`${INPUT_CLS} pr-8`} value={cwd}
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
                <input className={`${INPUT_CLS} font-mono mt-1`} value={watchPaths}
                  onChange={e => setWatchPaths(e.target.value)} placeholder='["./server", "./shared"]' />
              </div>
              <div>
                <label className={labelCls}>包含文件</label>
                <textarea className={`${INPUT_CLS} font-mono resize-none mt-1`} rows={3} value={watchInclude}
                  onChange={e => setWatchInclude(e.target.value)}
                  placeholder={'*\n*.ts\n*.tsx\n*.rs\n*.py'} />
              </div>
              <div>
                <label className={labelCls}>排除</label>
                <textarea className={`${INPUT_CLS} font-mono resize-none mt-1`} rows={4} value={watchExclude}
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
              {toolCommands.map((cmd, idx) => (
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
                        {/* 排序：列表顺序 = 右键菜单顺序。首/尾那侧禁用（点了也是原地不动） */}
                        <button
                          className="p-1.5 text-nexus-muted hover:text-nexus-text hover:bg-nexus-hover/50 rounded-md transition-colors disabled:opacity-25 disabled:hover:bg-transparent disabled:hover:text-nexus-muted"
                          title="上移（列表顺序 = 右键菜单顺序）"
                          disabled={idx === 0}
                          onClick={() => moveToolCommand(cmd.id, -1)}
                        >
                          <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.3">
                            <path d="M3.5 8.5L7 5l3.5 3.5"/>
                          </svg>
                        </button>
                        <button
                          className="p-1.5 text-nexus-muted hover:text-nexus-text hover:bg-nexus-hover/50 rounded-md transition-colors disabled:opacity-25 disabled:hover:bg-transparent disabled:hover:text-nexus-muted"
                          title="下移（列表顺序 = 右键菜单顺序）"
                          disabled={idx === toolCommands.length - 1}
                          onClick={() => moveToolCommand(cmd.id, 1)}
                        >
                          <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.3">
                            <path d="M3.5 5.5L7 9l3.5-3.5"/>
                          </svg>
                        </button>
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
              {/* 顺序只在右键菜单里能看出来，这里写明一次，避免"排了不知道排的是什么" */}
              <p className="text-[10px] text-nexus-muted/30 mt-1.5">列表顺序 = 右键菜单里工具命令的顺序（保存后生效）</p>
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
              <div className="flex items-center justify-between">
                <label className={labelCls}>环境变量</label>
                {/* SEC-6：变量值默认遮蔽。服务配置里常放 token/密码（命令日志已掩码，
                    这里此前是明文直接摊在屏幕上——截图/录屏/共享屏幕即泄露）*/}
                <button
                  type="button"
                  className="text-[11px] text-nexus-muted hover:text-nexus-accent transition-colors"
                  onClick={() => setShowEnvValues(v => !v)}
                  title={showEnvValues ? '遮蔽变量值（可防止截图泄露凭据）' : '显示并编辑变量值'}
                >{showEnvValues ? '遮蔽值' : '显示值'}</button>
              </div>
              <textarea
                className={`${INPUT_CLS} font-mono resize-none mt-1`}
                rows={4}
                value={showEnvValues ? envVars : maskEnvValues(envVars)}
                // 遮蔽态只读：否则用户会在掩码文本上编辑，保存时把掩码写进配置
                readOnly={!showEnvValues}
                onChange={e => setEnvVars(e.target.value)}
                placeholder={'PORT=3000\nNODE_ENV=development'}
                title={showEnvValues ? undefined : '值已遮蔽（点右上「显示值」后可编辑）'}
              />
              {!showEnvValues && envVars.trim() !== '' && (
                <p className="text-[11px] text-nexus-muted mt-1">
                  值已遮蔽，点右上「显示值」后可查看与编辑；保存时始终写真实值
                </p>
              )}
            </div>
          )}
        </div>
      </div>

      {/* 底部 */}
      <div className="p-3 border-t border-nexus-border flex-shrink-0 space-y-2">
        {/* 另存为模板：只有**已保存**的服务才能另存（新建中的还没有 id，后端无从取配置） */}
        {mode !== 'template' && service.id && (
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
  const [timeoutSecs, setTimeoutInput] = useState(
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

  return (
    <form onSubmit={handleSubmit} className="mt-3 space-y-2 p-2.5 bg-nexus-bg/50 rounded-md border border-nexus-border/50">
      <div>
        <label className="text-[11px] text-nexus-muted">名称</label>
        <input
          className={INPUT_CLS}
          value={name}
          onChange={e => setName(e.target.value)}
          placeholder="clean"
          autoFocus
        />
      </div>
      <div>
        <label className="text-[11px] text-nexus-muted">命令</label>
        <input
          className={`${INPUT_CLS} font-mono`}
          value={cmd}
          onChange={e => setCmd(e.target.value)}
          placeholder="mvn clean"
        />
      </div>
      <div>
        <label className="text-[11px] text-nexus-muted">超时（秒）</label>
        <input
          className={INPUT_CLS}
          value={timeoutSecs}
          onChange={e => setTimeoutInput(e.target.value)}
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
