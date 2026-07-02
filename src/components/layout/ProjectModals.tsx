import { useState, useEffect, useRef } from 'react';
import { projectApi, type Project } from '../../services/service';
import { Modal } from '../ui/Modal';
import { showNotification } from '../ui/Toast';

interface CreateModalProps {
  open: boolean;
  onClose: () => void;
  onCreated: (project: Project) => void;
}

export function CreateProjectModal({ open, onClose, onCreated }: CreateModalProps) {
  const [name, setName] = useState('');
  const [path, setPath] = useState('');
  const [terminalInitCommand, setTerminalInitCommand] = useState('');
  const [saving, setSaving] = useState(false);
  const nameRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (open) {
      setName(''); setPath(''); setTerminalInitCommand(''); setSaving(false);
      setTimeout(() => nameRef.current?.focus(), 80);
    }
  }, [open]);

  const handleSelectPath = async () => {
    const { open: openDialog } = await import('@tauri-apps/plugin-dialog');
    const selected = await openDialog({ directory: true, title: '选择项目目录' });
    if (selected) setPath(selected);
  };

  const handleCreate = async () => {
    const n = name.trim();
    const p = path.trim();
    if (!n || !p) return;
    setSaving(true);
    try {
      const project = await projectApi.add(n, p, terminalInitCommand);
      onCreated(project);
      onClose();
    } catch (e: unknown) {
      const msg = typeof e === 'string' ? e : e instanceof Error ? e.message : String(e);
      console.error('创建项目失败:', msg);
      showNotification({ variant: 'error', title: msg || '创建项目失败' });
    }
    setSaving(false);
  };

  return (
    <Modal open={open} title="新建项目" onClose={onClose}>
      <div className="space-y-4">
        <div>
          <label className="text-[11px] font-semibold text-nexus-muted uppercase tracking-wider block mb-1">项目名称</label>
          <input
            ref={nameRef}
            className="w-full px-3 py-2 text-[13px] bg-nexus-bg border border-nexus-border rounded text-nexus-text placeholder:text-nexus-muted focus:outline-none focus:border-nexus-accent"
            placeholder="例如: my-web-app"
            value={name}
            onChange={e => setName(e.target.value)}
            onKeyDown={e => { if (e.key === 'Enter') handleCreate(); }}
          />
        </div>
        <div>
          <label className="text-[11px] font-semibold text-nexus-muted uppercase tracking-wider block mb-1">项目路径</label>
          <div className="relative">
            <input
              className="w-full pl-3 pr-8 py-2 text-[13px] bg-nexus-bg border border-nexus-border rounded text-nexus-text font-mono placeholder:text-nexus-muted focus:outline-none focus:border-nexus-accent"
              placeholder="/path/to/project"
              value={path}
              onChange={e => setPath(e.target.value)}
              onKeyDown={e => { if (e.key === 'Enter') handleCreate(); }}
            />
            <button
              className="absolute right-1 top-1/2 -translate-y-1/2 p-1 text-nexus-muted hover:text-nexus-text rounded"
              onClick={handleSelectPath}
            >
              <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.3"><rect x="1" y="3" width="12" height="9" rx="1"/><path d="M1 5h12"/><path d="M5 1h2l1 2H5z"/></svg>
            </button>
          </div>
        </div>
        <div>
          <label className="text-[11px] font-semibold text-nexus-muted uppercase tracking-wider block mb-1">
            终端初始命令
            <span className="font-normal normal-case ml-1 text-nexus-muted/60">（可选）</span>
          </label>
          <textarea
            className="w-full px-3 py-2 text-[13px] bg-nexus-bg border border-nexus-border rounded text-nexus-text font-mono placeholder:text-nexus-muted focus:outline-none focus:border-nexus-accent resize-none"
            placeholder="终端创建时自动执行的命令，例如:&#10;cd src&#10;source .env"
            rows={3}
            value={terminalInitCommand}
            onChange={e => setTerminalInitCommand(e.target.value)}
          />
          <p className="text-[11px] text-nexus-muted/50 mt-1">项目终端创建时自动执行，不影响服务终端</p>
        </div>
        <div className="flex items-center justify-end gap-2 pt-1">
          <button
            className="px-4 py-1.5 text-[12px] text-nexus-text-muted hover:text-nexus-text rounded hover:bg-nexus-hover/50"
            onClick={onClose}
          >取消</button>
          <button
            className="px-6 py-1.5 text-[13px] bg-nexus-accent text-white rounded hover:bg-nexus-accent-hover disabled:opacity-40 disabled:cursor-not-allowed"
            disabled={saving || !name.trim() || !path.trim()}
            onClick={handleCreate}
          >
            {saving ? '创建中…' : '创建项目'}
          </button>
        </div>
      </div>
    </Modal>
  );
}

interface EditModalProps {
  project: Project | null;
  onClose: () => void;
  onUpdated: () => void;
  onProjectName?: (name: string) => void;
}

export function EditProjectModal({ project, onClose, onUpdated, onProjectName }: EditModalProps) {
  const [name, setName] = useState('');
  const [path, setPath] = useState('');
  const [terminalInitCommand, setTerminalInitCommand] = useState('');
  const [saving, setSaving] = useState(false);
  const nameRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (project) {
      setName(project.name);
      setPath(project.path);
      setTerminalInitCommand(project.terminal_init_command || '');
      setSaving(false);
      setTimeout(() => nameRef.current?.focus(), 80);
    }
  }, [project]);

  const handleSelectPath = async () => {
    const { open: openDialog } = await import('@tauri-apps/plugin-dialog');
    const selected = await openDialog({ directory: true, title: '选择项目目录' });
    if (selected) setPath(selected);
  };

  const handleSave = async () => {
    const n = name.trim();
    const p = path.trim();
    if (!n || !p || !project) return;
    setSaving(true);
    try {
      await projectApi.update(project.id, n, p, terminalInitCommand);
      onProjectName?.(n);
      showNotification({ title: `已更新「${n}」` });
      onUpdated();
      onClose();
    } catch (e: unknown) {
      showNotification({ variant: 'error', title: String(e) });
    }
    setSaving(false);
  };

  return (
    <Modal open={!!project} title="编辑项目" onClose={onClose}>
      <div className="space-y-4">
        <div>
          <label className="text-[11px] font-semibold text-nexus-muted uppercase tracking-wider block mb-1">项目名称</label>
          <input
            ref={nameRef}
            className="w-full px-3 py-2 text-[13px] bg-nexus-bg border border-nexus-border rounded text-nexus-text placeholder:text-nexus-muted focus:outline-none focus:border-nexus-accent"
            placeholder="例如: my-web-app"
            value={name}
            onChange={e => setName(e.target.value)}
            onKeyDown={e => { if (e.key === 'Enter') handleSave(); }}
          />
        </div>
        <div>
          <label className="text-[11px] font-semibold text-nexus-muted uppercase tracking-wider block mb-1">项目路径</label>
          <div className="relative">
            <input
              className="w-full pl-3 pr-8 py-2 text-[13px] bg-nexus-bg border border-nexus-border rounded text-nexus-text font-mono placeholder:text-nexus-muted focus:outline-none focus:border-nexus-accent"
              placeholder="/path/to/project"
              value={path}
              onChange={e => setPath(e.target.value)}
              onKeyDown={e => { if (e.key === 'Enter') handleSave(); }}
            />
            <button
              className="absolute right-1 top-1/2 -translate-y-1/2 p-1 text-nexus-muted hover:text-nexus-text rounded"
              onClick={handleSelectPath}
            >
              <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.3"><rect x="1" y="3" width="12" height="9" rx="1"/><path d="M1 5h12"/><path d="M5 1h2l1 2H5z"/></svg>
            </button>
          </div>
        </div>
        <div>
          <label className="text-[11px] font-semibold text-nexus-muted uppercase tracking-wider block mb-1">
            终端初始命令
            <span className="font-normal normal-case ml-1 text-nexus-muted/60">（可选）</span>
          </label>
          <textarea
            className="w-full px-3 py-2 text-[13px] bg-nexus-bg border border-nexus-border rounded text-nexus-text font-mono placeholder:text-nexus-muted focus:outline-none focus:border-nexus-accent resize-none"
            placeholder="终端创建时自动执行的命令，例如:&#10;cd src&#10;source .env"
            rows={3}
            value={terminalInitCommand}
            onChange={e => setTerminalInitCommand(e.target.value)}
          />
          <p className="text-[11px] text-nexus-muted/50 mt-1">项目终端创建时自动执行，不影响服务终端</p>
        </div>
        <div className="flex items-center justify-end gap-2 pt-1">
          <button
            className="px-4 py-1.5 text-[12px] text-nexus-text-muted hover:text-nexus-text rounded hover:bg-nexus-hover/50"
            onClick={onClose}
          >取消</button>
          <button
            className="px-6 py-1.5 text-[13px] bg-nexus-accent text-white rounded hover:bg-nexus-accent-hover disabled:opacity-40 disabled:cursor-not-allowed"
            disabled={saving || !name.trim() || !path.trim()}
            onClick={handleSave}
          >{saving ? '保存中…' : '保存修改'}</button>
        </div>
      </div>
    </Modal>
  );
}

interface DeleteModalProps {
  target: { id: string; name: string } | null;
  onClose: () => void;
  onDeleted: (id: string) => void;
  /** 当删除的项目是当前选中项目时调用，用于取消选中 */
  onDeselectIfSelected?: () => void;
}

export function DeleteProjectModal({ target, onClose, onDeleted, onDeselectIfSelected }: DeleteModalProps) {
  const [deleting, setDeleting] = useState(false);

  const handleDelete = async () => {
    if (!target) return;
    setDeleting(true);
    try {
      await projectApi.delete(target.id);
      onDeselectIfSelected?.();
      showNotification({ variant: 'warning', title: `已删除「${target.name}」` });
      onDeleted(target.id);
      onClose();
    } catch (e: unknown) {
      showNotification({ variant: 'error', title: String(e) });
    }
    setDeleting(false);
  };

  return (
    <Modal open={!!target} title="确认删除" onClose={onClose}>
      <div className="space-y-4">
        <p className="text-[13px] text-nexus-text">
          确定要删除项目 <span className="text-nexus-warning font-medium">「{target?.name}」</span> 吗？
        </p>
        <p className="text-[12px] text-nexus-muted">
          该项目下的所有服务配置也会被一并删除，此操作不可撤销。
        </p>
        <div className="flex items-center justify-end gap-2">
          <button
            className="px-4 py-1.5 text-[12px] text-nexus-text-muted hover:text-nexus-text rounded hover:bg-nexus-hover/50"
            onClick={onClose}
          >取消</button>
          <button
            className="px-5 py-1.5 text-[13px] bg-nexus-error text-white rounded hover:bg-nexus-error/80 disabled:opacity-40"
            disabled={deleting}
            onClick={handleDelete}
          >{deleting ? '删除中…' : '确认删除'}</button>
        </div>
      </div>
    </Modal>
  );
}
