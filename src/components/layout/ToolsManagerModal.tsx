import { useEffect, useState } from 'react';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import { Modal } from '../ui/Modal';
import { openToolsApi, type OpenTool } from '../../services/service';
import { useToolStore } from '../../stores/toolStore';
import { showNotification } from '../ui/Toast';
import { reportError } from '../../utils/error';

interface Props {
  open: boolean;
  onClose: () => void;
}

/**
 * 打开工具库管理（全局）：手动增删改。
 * 工具 = 可执行文件路径（executable）+ 参数模板（args，可选）。
 * 服务通过下拉绑定其中一个；工具删除时相关服务绑定自动解除（数据库级联）。
 * command 为历史整串命令字段：executable 为空的行属旧格式，编辑保存一次即升级。
 */
export function ToolsManagerModal({ open, onClose }: Props) {
  const openTools = useToolStore(s => s.openTools);
  const refreshTools = useToolStore(s => s.refreshTools);

  // 编辑中的工具（null = 新增模式）
  const [editing, setEditing] = useState<OpenTool | null>(null);
  const [formName, setFormName] = useState('');
  const [formExecutable, setFormExecutable] = useState('');
  const [formArgs, setFormArgs] = useState('');
  /** 保存中（防连点，UX-16） */
  const [saving, setSaving] = useState(false);
  /** 待删除的工具（非 null = 确认框开着） */
  const [deleting, setDeleting] = useState<OpenTool | null>(null);
  /** 它的引用清单（null = 还没查到/查不到） */
  const [deleteUsage, setDeleteUsage] = useState<{ services: string[]; templates: string[] } | null>(null);
  const [deletingBusy, setDeletingBusy] = useState(false);

  useEffect(() => {
    if (!open) return;
    setEditing(null);
    setFormName('');
    setFormExecutable('');
    setFormArgs('');
    // 关掉再打开不该残留上一次的确认框
    setDeleting(null);
    setDeleteUsage(null);
  }, [open]);

  const beginEdit = (t: OpenTool) => {
    setEditing(t);
    setFormName(t.name);
    setFormExecutable(t.executable);
    setFormArgs(t.args);
  };

  const resetForm = () => {
    setEditing(null);
    setFormName('');
    setFormExecutable('');
    setFormArgs('');
  };

  const handleSave = async () => {
    if (!formName.trim() || !formExecutable.trim()) return;
    // 防连点（UX-16）：`id: null` 是"新建"语义，双击就会建出两条同名工具
    if (saving) return;
    setSaving(true);
    try {
      await openToolsApi.save({ id: editing?.id ?? null, name: formName, executable: formExecutable, args: formArgs });
      await refreshTools();
      resetForm();
      showNotification({ variant: 'success', title: editing ? '工具已更新' : '工具已添加' });
    } catch (e) {
      reportError('保存工具失败', e);
    }
    setSaving(false);
  };

  /** 弹出文件选择器挑选可执行程序（也可手动输入：编辑/Path 命令场景） */
  const handleSelectExecutable = async () => {
    const selected = await openDialog({
      multiple: false,
      title: '选择可执行文件',
      filters: [{ name: '可执行程序', extensions: ['exe', 'cmd', 'bat', 'com'] }],
    });
    if (typeof selected === 'string' && selected.trim()) {
      setFormExecutable(selected.replaceAll('\\', '/'));
    }
  };

  /**
   * 删除工具：**必须先确认**（UX-4）。
   *
   * 原实现点一下垃圾桶图标就删掉了，而删除是级联的——跨项目所有绑了它的服务一起失联、
   * 模板里的默认打开工具被清空，提示却只有一句"已删除工具「X」"，不说影响了谁。
   * 同一应用里文件、项目、服务、模板的删除都有确认框，只有这一处没有。
   */
  const handleDelete = async (t: OpenTool) => {
    setDeleting(t);
    try {
      setDeleteUsage(await openToolsApi.usage(t.id));
    } catch (e) {
      // 查不到引用不等于不能删：如实告知"影响范围未知"，把选择权留给用户
      reportError('查询工具引用失败', e, {
        variant: 'warning',
        title: '查不到这个工具被谁引用',
        description: '确认框仍会打开，但无法列出受影响的服务的清单',
      });
      setDeleteUsage(null);
    }
  };

  const confirmDelete = async () => {
    const t = deleting;
    if (!t || deletingBusy) return;
    setDeletingBusy(true);
    try {
      await openToolsApi.delete(t.id);
      await refreshTools();
      showNotification({ title: `已删除工具「${t.name}」` });
      setDeleting(null);
    } catch (e) {
      reportError('删除工具失败', e);
    }
    setDeletingBusy(false);
  };

  const inputCls = "w-full px-2.5 py-1.5 text-[13px] bg-nexus-bg border border-nexus-border rounded-md text-nexus-text placeholder:text-nexus-muted/50 focus:outline-none focus:border-nexus-accent transition-colors";

  return (
    <Modal open={open} title="打开工具库" onClose={onClose} width="560px">
      <div className="space-y-4">
        {/* ── 编辑表单 ── */}
        <div className="space-y-2 p-3 bg-nexus-bg/30 border border-nexus-border/50 rounded-lg">
          <div className="flex items-center gap-3">
            <div className="flex-1">
              <label className="text-[11px] text-nexus-muted block mb-1">名称</label>
              <input className={inputCls} value={formName} onChange={e => setFormName(e.target.value)}
                placeholder="IntelliJ IDEA" />
            </div>
            <div className="flex-[1.4]">
              <label className="text-[11px] text-nexus-muted block mb-1">可执行文件</label>
              <div className="relative">
                <input className={`${inputCls} font-mono pr-8`} value={formExecutable}
                  onChange={e => setFormExecutable(e.target.value)}
                  placeholder="C:\Program Files\JetBrains\...\idea64.exe" />
                <button
                  className="absolute right-1 top-1/2 -translate-y-1/2 p-1 text-nexus-muted hover:text-nexus-text rounded"
                  title="浏览选择"
                  onClick={handleSelectExecutable}
                >
                  {/* 与添加服务/工作目录选择同款图标 */}
                  <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.3"><rect x="1" y="3" width="12" height="9" rx="1"/><path d="M1 5h12"/><path d="M5 1h2l1 2H5z"/></svg>
                </button>
              </div>
            </div>
          </div>
          <div>
            <label className="text-[11px] text-nexus-muted block mb-1">参数（可选）</label>
            <input className={`${inputCls} font-mono`} value={formArgs} onChange={e => setFormArgs(e.target.value)}
              placeholder="--reuse-window {path}" />
          </div>
          <div className="flex items-center justify-between">
            <p className="text-[10px] text-nexus-muted/50">
              {`{path}`} 会作为独立参数传入服务工作目录；参数留空则自动把目录追加到末尾
            </p>
            <div className="flex gap-2">
              {editing && (
                <button
                  className="px-3 py-1 text-[11px] text-nexus-muted hover:text-nexus-text rounded hover:bg-nexus-hover/50"
                  onClick={resetForm}
                >取消</button>
              )}
              <button
                className="px-3 py-1 text-[11px] bg-nexus-accent text-white rounded hover:bg-nexus-accent-hover disabled:opacity-40 font-medium"
                disabled={saving || !formName.trim() || !formExecutable.trim()}
                onClick={handleSave}
              >{saving ? '保存中…' : (editing ? '更新' : '添加工具')}</button>
            </div>
          </div>
        </div>

        {/* ── 工具列表 ── */}
        <div className="max-h-[240px] overflow-auto space-y-1.5">
          {openTools.length === 0 ? (
            <p className="text-center text-[12px] text-nexus-muted/40 py-4">暂无工具，在上方填写名称与可执行文件手动添加</p>
          ) : openTools.map(t => {
            const legacy = !t.executable && !!t.command;
            return (
              <div key={t.id} className="flex items-center gap-2 px-3 py-2 bg-nexus-bg/30 border border-nexus-border/50 rounded-lg">
                <span className="flex-1 min-w-0">
                  <span className="flex items-center gap-1.5">
                    <span className="text-[13px] text-nexus-text truncate">{t.name}</span>
                    {legacy && (
                      <span className="text-[10px] text-nexus-warning bg-nexus-warning/15 px-1.5 py-px rounded flex-shrink-0"
                        title="旧版命令格式：编辑并填写可执行文件后保存一次即可升级"
                      >旧格式</span>
                    )}
                  </span>
                  <span className="block text-[11px] text-nexus-muted/60 font-mono truncate">
                    {legacy ? t.command : [t.executable, t.args].filter(Boolean).join(' ')}
                  </span>
                </span>
                <button
                  className="p-1.5 text-nexus-muted hover:text-nexus-accent rounded hover:bg-nexus-hover/50 flex-shrink-0"
                  title="编辑"
                  onClick={() => beginEdit(t)}
                >
                  <svg width="11" height="11" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2"><path d="M7 2l1 1-5.5 5.5H1.5V7.5L7 2z"/></svg>
                </button>
                <button
                  className="p-1.5 text-nexus-muted hover:text-nexus-error rounded hover:bg-nexus-error/10 flex-shrink-0"
                  title="删除（服务绑定将自动解除）"
                  onClick={() => handleDelete(t)}
                >
                  <svg width="11" height="11" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.3"><path d="M2.5 3h5M3.5 3V2a.5.5 0 01.5-.5h2a.5.5 0 01.5.5v1M4 4.5v3M6 4.5v3M3 3l.5 6a1 1 0 001 .5h3a1 1 0 001-.5L9 3"/></svg>
                </button>
              </div>
            );
          })}
        </div>
      </div>

      {/* 删除确认（UX-4）：先说清"会影响谁"，再让用户点。
          嵌套在工具库弹窗之上——Modal 用 fixed + 更高层级渲染，视觉上叠一层遮罩 */}
      <Modal open={!!deleting} title="确认删除工具" onClose={() => setDeleting(null)} width="440px">
        <div className="space-y-3">
          <p className="text-[13px] text-nexus-text">
            确定要删除工具 <span className="text-nexus-error font-medium">「{deleting?.name ?? ''}」</span> 吗？
          </p>
          <div className="text-[12px] text-nexus-muted space-y-1.5">
            <p>工具本身（可执行文件、参数）会永久删除，且**无法撤销**。</p>
            {deleteUsage === null ? (
              <p className="text-nexus-warning">未能查到它被谁引用：可能还有服务正在用它。</p>
            ) : deleteUsage.services.length === 0 && deleteUsage.templates.length === 0 ? (
              <p>当前没有服务或模板在用它。</p>
            ) : (
              <>
                {deleteUsage.services.length > 0 && (
                  <p className="text-nexus-warning">
                    以下 {deleteUsage.services.length} 个服务的「用 XX 打开」绑定会被解除：
                    <span className="block mt-1 text-nexus-muted/80 break-all">
                      {deleteUsage.services.join('、')}
                    </span>
                  </p>
                )}
                {deleteUsage.templates.length > 0 && (
                  <p className="text-nexus-warning">
                    以下 {deleteUsage.templates.length} 个模板的默认打开工具会被清空：
                    <span className="block mt-1 text-nexus-muted/80 break-all">
                      {deleteUsage.templates.join('、')}
                    </span>
                  </p>
                )}
              </>
            )}
          </div>
          <div className="flex justify-end gap-2 pt-1">
            <button
              className="px-3 py-1.5 text-[12px] text-nexus-muted hover:text-nexus-text rounded-md hover:bg-nexus-hover/50"
              onClick={() => setDeleting(null)}
            >取消</button>
            <button
              className="px-3 py-1.5 text-[12px] bg-nexus-error text-white rounded-md hover:bg-nexus-error/85 disabled:opacity-40 font-medium"
              disabled={deletingBusy}
              onClick={() => void confirmDelete()}
            >{deletingBusy ? '删除中…' : '删除工具'}</button>
          </div>
        </div>
      </Modal>
    </Modal>
  );
}
